//! The core's own HTTP server: where clients get the event stream.
//!
//! Decision 13 makes this a piece of the core rather than an implementation
//! detail of the shell, and for a reason that only shows up later: the tiles
//! need a **second local origin**, and the thing that serves it cannot live in
//! the window it is isolated from.
//!
//! # Two ports, and they are not interchangeable
//!
//! | Port | Serves | Who may talk to it |
//! |---|---|---|
//! | [`EVENTS_PORT`] | the event stream, this module | the window, with the session token |
//! | [`TILES_PORT`] | the tile sandbox (T7, not yet built) | tile iframes |
//!
//! Separate listeners, on purpose. A tile is sandboxed with `allow-scripts`
//! **and** `allow-same-origin`, which is only safe because its origin differs
//! from the application's — so if the tile sandbox and the event stream shared
//! an origin, a tile could open the window's event stream and read every
//! session, every command and every pending permission request. Decision 13
//! puts the real boundary in the host process, not in the `sandbox` attribute;
//! this is that boundary, at the level of the socket.
//!
//! # The token
//!
//! Bound to loopback, which keeps the network out but not other local
//! processes: anything running as this user can reach `127.0.0.1`. So the
//! stream wants a **session token**, generated at start-up, handed to the
//! window through Tauri's IPC, and never written anywhere. It is cheap, and it
//! is what makes "distinct origin" more than a statement of intent.
//!
//! # What goes the other way
//!
//! Nothing. This channel is core → client only. Commands travel over Tauri's
//! IPC, where they are scoped by a capability file, which is the same split
//! The original specification describes: WebSocket **for the events**.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::Core;
use crate::events::{Resume, Seq};

/// Where the window reads the event stream.
///
/// Pinned, not ephemeral: the window's CSP has to name it in `connect-src`, and
/// a CSP cannot be written against a port picked at run time. The prototype used
/// a port wildcard to stay robust; decision 13 says the V0 pins it.
pub const EVENTS_PORT: u16 = 7421;

/// Where the tile sandbox will be served from, in T7.
///
/// Declared here, next to [`EVENTS_PORT`], so the reason they differ is visible
/// in one place. Already named in the window's CSP `frame-src`, and already
/// proven workable by a throwaway prototype.
pub const TILES_PORT: u16 = 7420;

/// A client's credentials for the event stream.
#[derive(Clone, Debug)]
pub struct SessionToken(String);

impl SessionToken {
    /// A fresh token for this run of the application.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// What the window is given, over IPC.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time-ish comparison is not the point here — the token is a
    /// v4 UUID and the attacker is a local process that can guess once per
    /// connection. Equality is enough, and pretending otherwise would be
    /// security theatre.
    fn matches(&self, candidate: &str) -> bool {
        self.0 == candidate
    }
}

#[derive(Clone)]
struct Server {
    core: Arc<Core>,
    token: SessionToken,
}

#[derive(Debug, Deserialize)]
struct Subscribe {
    token: String,
    /// The last sequence the client holds. Absent means "nothing".
    #[serde(default)]
    since: Seq,
}

/// Serve the event stream until the process ends.
///
/// Binds [`EVENTS_PORT`] on loopback. Returns an error if the port is taken,
/// which is worth surfacing rather than retrying on another one: a window whose
/// CSP names 7421 cannot reach a server that moved.
pub async fn serve_events(core: Arc<Core>, token: SessionToken) -> anyhow::Result<()> {
    serve_events_on(bind_events()?, core, token).await
}

/// Bind [`EVENTS_PORT`], **synchronously**, before anything is spawned.
///
/// Exists so a caller can make the port reachable and only then start whatever
/// will connect to it — the main agent's CLI connects to the MCP endpoint on
/// this same listener, and that handshake losing a race is not an error: it is
/// a model with no tools, apologising. Binding is enough for that, because the
/// kernel queues connections until axum gets round to accepting them.
///
/// It hands back a **standard** listener and not a `tokio` one, which is the
/// whole of the fix for a panic this cost on 2026-10-27:
/// `tokio::net::TcpListener::from_std` registers with the reactor and so must
/// be called from inside the runtime — "there is no reactor running" — while
/// the point of this function is to be callable from outside it. The
/// conversion therefore lives in [`serve_events_on`], where there is always a
/// runtime, and no caller can put it in the wrong place.
pub fn bind_events() -> anyhow::Result<std::net::TcpListener> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], EVENTS_PORT));
    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("le port {EVENTS_PORT} est pris — ARIA tourne déjà ?"))?;
    tracing::info!("flux d'événements sur ws://{addr}/events");
    Ok(listener)
}

/// The same server on a listener the caller already has.
///
/// Exists so a test can bind port 0 and get whatever the OS gives it. The
/// product never calls this with anything but [`EVENTS_PORT`] — the port is
/// pinned because a CSP cannot be written against one chosen at run time.
pub async fn serve_events_on(
    listener: std::net::TcpListener,
    core: Arc<Core>,
    token: SessionToken,
) -> anyhow::Result<()> {
    // Both steps here, and neither at the call site. `set_nonblocking` is not
    // optional — `from_std` documents that the caller owns it, and a blocking
    // socket handed to the reactor misbehaves rather than failing — and
    // `from_std` needs a running reactor, which is guaranteed here and only
    // here. Together they make the pair impossible to get wrong.
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    // The relay's MCP endpoint rides on the same listener as the event
    // stream, and that is deliberate rather than lazy. The origin that has to
    // be *separate* is the tile sandbox ([`TILES_PORT`]), because a tile runs
    // in a browser context with `allow-same-origin`. The MCP endpoint is
    // consumed by a subprocess we launched ourselves, not by a page, so origin
    // isolation buys nothing there — what protects it is the per-session token
    // in its URL.
    let mcp = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        {
            let core = Arc::clone(&core);
            move || Ok(crate::mcp::Relay::new(Arc::clone(&core)))
        },
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        Default::default(),
    );

    // And the main agent's own endpoint, which is the other direction: the
    // relay above is permission coming in from a session, this is work going
    // out to one. `crate::mcp::voice` says why they are not the same server.
    let agent = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        {
            let core = Arc::clone(&core);
            move || Ok(crate::mcp::voice::VoiceTools::new(Arc::clone(&core)))
        },
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        Default::default(),
    );
    // And the memory, read-only, for the delegated sessions (decision 106).
    // A third server rather than two more tools on the relay above, and
    // `crate::mcp::subagent` gives the reason: the budget. Mounted on the same
    // listener as the other two, since all three are consumed by subprocesses
    // we launched and none by a page.
    let memory = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        {
            let core = Arc::clone(&core);
            move || Ok(crate::mcp::subagent::MemoryTools::new(Arc::clone(&core)))
        },
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        Default::default(),
    );

    // Guarded by the **session** tokens rather than the main agent's, because
    // the audience is every delegated session and each holds its own. Same
    // place as the agent's guard and for the same two reasons: it covers
    // `tools/list`, and it leaves `crate::mcp::subagent` to be pure protocol.
    let memory = Router::new()
        .fallback_service(memory)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(core.session_routes()),
            authorise_session,
        ));

    // Guarded in front of the door rather than in the tool handlers, which is
    // the opposite of the relay's choice and deliberate: the relay reads its
    // token because it needs the routing, so it cannot refuse before
    // dispatching. Here there is one caller, the token is pure
    // authentication, and a check here also covers `tools/list`.
    let agent = Router::new()
        .fallback_service(agent)
        .layer(axum::middleware::from_fn_with_state(
            core.agent_token().to_string(),
            authorise,
        ));

    let app = Router::new()
        .route("/events", get(upgrade))
        .route("/health", get(|| async { "ok" }))
        .nest_service("/mcp", mcp)
        .nest(MEMORY_PATH, memory)
        .nest(AGENT_PATH, agent)
        .with_state(Server { core, token });
    axum::serve(listener, app).await?;
    Ok(())
}

/// Let a request through only if it carries the token of a session we still
/// hold.
///
/// Any live session's token opens it, and that is correct rather than loose:
/// the memory belongs to the user, every session was opened by the user, and
/// there is nothing here that one session could read and another could not.
/// What the token buys is that a process we did **not** launch cannot read the
/// user's notes by guessing a port.
///
/// A token whose session has ended stops working, because
/// [`crate::mcp::SessionRoutes::close`] forgets it — so the window of exposure
/// is the life of the session, not the life of the application.
async fn authorise_session(
    State(routes): State<Arc<crate::mcp::SessionRoutes>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let presented = request.uri().query().and_then(crate::mcp::token_in);
    if presented.and_then(|token| routes.resolve(token)).is_none() {
        tracing::warn!("un appel sans jeton de session valide sur {MEMORY_PATH}");
        return (StatusCode::UNAUTHORIZED, "no").into_response();
    }
    next.run(request).await
}

/// Let a request through only if it carries the main agent's token.
async fn authorise(
    State(expected): State<String>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let presented = request.uri().query().and_then(crate::mcp::token_in);
    if presented != Some(expected.as_str()) {
        // No detail in the body, and said at `warn`: on this endpoint an
        // unauthorised caller is a local process trying to start work on the
        // machine, which is worth seeing in a log.
        tracing::warn!("un appel sans jeton valide sur {AGENT_PATH}");
        return (StatusCode::UNAUTHORIZED, "no").into_response();
    }
    next.run(request).await
}

/// Where the main agent's own tools are served.
///
/// A second path on the same listener, not a second port: both are consumed by
/// subprocesses we launched ourselves, never by a page, so origin isolation
/// buys nothing. What separates them is the audience — see
/// [`crate::mcp::voice`] — and what protects each is its token.
pub const AGENT_PATH: &str = "/agent";

/// Where a delegated session reads ARIA's memory.
///
/// A third path on the same listener, for the same reason as the second: the
/// audience differs, the token protects each. Decision 106.
pub const MEMORY_PATH: &str = "/memory";

/// The URL one session's relay endpoint lives at.
///
/// The token is both the routing key and the only thing that makes the
/// endpoint reachable: a second local process that guessed the port still has
/// nothing to say.
pub fn relay_url(token: &str) -> String {
    format!("http://127.0.0.1:{EVENTS_PORT}/mcp?s={token}")
}

/// The MCP servers one delegated session is given.
///
/// **What to serve, not how to configure it.** Until 2026-11-12 this function
/// returned Claude Code's `{"mcpServers": …}` JSON, which put one engine's
/// configuration file format inside engine-agnostic code — the first of the
/// four leaks found when the provider abstraction was taken seriously. Each
/// provider now renders these; the Claude Code one still writes `type: "http"`,
/// read off the installed CLI on 2026-10-25 rather than taken from a guide.
///
/// Two servers under one token: the relay, and the memory read-only
/// (decision 106). One session, one secret, two doors.
pub fn session_servers(token: &str) -> Vec<crate::providers::McpServer> {
    vec![
        crate::providers::McpServer {
            name: crate::mcp::SERVER_NAME.to_string(),
            url: relay_url(token),
        },
        crate::providers::McpServer {
            name: crate::mcp::subagent::SERVER_NAME.to_string(),
            url: memory_url(token),
        },
    ]
}

/// Where a delegated session reaches the memory.
pub fn memory_url(token: &str) -> String {
    format!("http://127.0.0.1:{EVENTS_PORT}{MEMORY_PATH}?s={token}")
}

/// Where the main agent reaches its own tools.
pub fn agent_url(token: &str) -> String {
    format!("http://127.0.0.1:{EVENTS_PORT}{AGENT_PATH}?s={token}")
}

/// The `--mcp-config` payload for the main agent.
///
/// The same shape as a session's, pointed at the other endpoint, and under the
/// same server name — so the tools are called `mcp__aria__delegate_task`,
/// which is what [`crate::mcp::voice::allowed_tools`] puts in `--tools`.
pub fn agent_mcp_config(token: &str) -> String {
    server_config(&agent_url(token))
}

fn server_config(url: &str) -> String {
    serde_json::json!({
        "mcpServers": {
            crate::mcp::SERVER_NAME: {
                "type": "http",
                "url": url,
            }
        }
    })
    .to_string()
}

async fn upgrade(
    State(server): State<Server>,
    Query(params): Query<Subscribe>,
    ws: WebSocketUpgrade,
) -> Response {
    if !server.token.matches(&params.token) {
        // Said in **our** log, not in the body: a local process probing the
        // port still learns only that it guessed wrong. We, on the other hand,
        // need to be able to tell a refused window from one that never
        // knocked — those look identical on screen and cost an evening once.
        tracing::warn!(
            "jeton refusé sur /events ({} caractères présentés)",
            params.token.len()
        );
        return (StatusCode::UNAUTHORIZED, "no").into_response();
    }
    let since = params.since;
    tracing::info!("une fenêtre s'abonne au flux depuis {since}");
    ws.on_upgrade(move |socket| stream(socket, server.core, since))
}

/// The microphone level, as the socket carries it.
///
/// Deliberately **not** an [`crate::events::Envelope`]: it has no sequence, it
/// is never replayed and it is not in the audit. A client that folds events
/// must be able to tell the two apart at a glance, which is what the distinct
/// `type` is for.
#[derive(serde::Serialize)]
struct Meter {
    #[serde(rename = "type")]
    kind: Kind,
    level: f32,
}

#[derive(serde::Serialize)]
enum Kind {
    #[serde(rename = "meter")]
    Meter,
}

impl Meter {
    fn new(level: f32) -> Self {
        Self {
            kind: Kind::Meter,
            level,
        }
    }
}

async fn stream(mut socket: WebSocket, core: Arc<Core>, since: Seq) {
    // Subscribe BEFORE reading history. The other order drops every event
    // published between the two calls, which is exactly the window of time a
    // reconnecting client cares about.
    let mut live = core.broker().subscribe();

    let resume = core.broker().resume_from(since);
    let mut sent_through = match &resume {
        Resume::Replay { events, .. } => events.last().map(|e| e.seq).unwrap_or(since),
        // The client has to resynchronise from a snapshot, so nothing has been
        // sent: let the live stream deliver everything from here.
        Resume::Gap { oldest, .. } => oldest.saturating_sub(1),
    };
    if !send_json(&mut socket, &resume).await {
        return;
    }

    // The microphone level, sampled rather than subscribed to.
    //
    // It changes fifty times a second and is worthless one frame later, so it
    // is **not** on the event stream — see `Core::level`. Here it is read at a
    // fixed cadence instead, which is what a meter wants: the client gets the
    // latest value, never a backlog, and a frame missed is a frame nobody
    // would have seen.
    let mut levels = core.levels();
    let mut meter = tokio::time::interval(Duration::from_millis(50));
    meter.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_sent = -1.0f32;

    loop {
        let envelope = tokio::select! {
            // Events first when both are ready: a level is a decoration, an
            // event is the state of the machine.
            biased;
            event = live.recv() => event,
            _ = meter.tick() => {
                let level = *levels.borrow_and_update();
                // Only when it moved enough to see. A silent room would
                // otherwise send twenty identical frames a second for ever.
                if (level - last_sent).abs() > 0.015 {
                    last_sent = level;
                    if !send_json(&mut socket, &Meter::new(level)).await {
                        return;
                    }
                }
                continue;
            }
        };

        match envelope {
            Ok(envelope) => {
                // Skip what the replay already covered.
                if envelope.seq <= sent_through {
                    continue;
                }
                sent_through = envelope.seq;
                if !send_json(&mut socket, &envelope).await {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                // A slow client, not a lost event: the history still holds
                // them, so resume from where it got to. This is why the ring
                // exists, and why lagging is recoverable rather than fatal.
                tracing::warn!("client lagged by {missed} events, replaying from {sent_through}");
                let resume = core.broker().resume_from(sent_through);
                if let Resume::Replay { events, .. } = &resume
                    && let Some(last) = events.last()
                {
                    sent_through = last.seq;
                }
                if !send_json(&mut socket, &resume).await {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Returns false when the socket is gone, which is the signal to stop.
async fn send_json<T: serde::Serialize>(socket: &mut WebSocket, value: &T) -> bool {
    match serde_json::to_string(value) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(e) => {
            // Our own contract failed to serialise. Report it and keep the
            // connection: dropping the client would hide the bug.
            tracing::error!("event will not serialise: {e}");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tile_origin_is_a_different_port_from_the_event_stream() {
        // Not a style preference. Same origin would let a tile — sandboxed
        // with `allow-same-origin` by MCP Apps' own requirement — subscribe to
        // every session and every pending permission request.
        assert_ne!(EVENTS_PORT, TILES_PORT);
    }

    /// A delegated session is handed **two** servers under one token, and the
    /// three facts that have to agree are written in three different files:
    /// the server names (`crate::mcp`), the paths (above), and the tool names
    /// the core recognises. A disagreement is silent — the session simply has
    /// no memory.
    #[test]
    fn a_session_is_handed_the_relay_and_the_memory_under_one_token() {
        let servers = session_servers("jeton-de-test");

        assert_eq!(servers.len(), 2, "{servers:?}");
        assert_eq!(servers[0].name, crate::mcp::SERVER_NAME);
        assert_eq!(servers[1].name, crate::mcp::subagent::SERVER_NAME);
        assert!(servers[0].url.contains("/mcp?s=jeton-de-test"), "{servers:?}");
        assert!(
            servers[1].url.contains("/memory?s=jeton-de-test"),
            "{servers:?}"
        );
    }

    /// The main agent gets **one** server, and not this one: its post has the
    /// memory tools of its own (decision 105), and the read-only post is for
    /// the sessions it delegates to.
    #[test]
    fn the_main_agent_is_not_handed_the_memory_post() {
        let config = agent_mcp_config("jeton-de-test");
        assert!(!config.contains(crate::mcp::subagent::SERVER_NAME), "{config}");
        assert!(config.contains(crate::mcp::SERVER_NAME), "{config}");
    }

    #[test]
    fn a_wrong_token_does_not_match() {
        let token = SessionToken::new();
        assert!(token.matches(token.as_str()));
        assert!(!token.matches("whatever"));
    }
}
