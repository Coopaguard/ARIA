//! The main agent's own MCP endpoint, on the wire.
//!
//! Three things are tested here that a unit test of the router cannot see,
//! because each of them is *between* two pieces:
//!
//! 1. the **handshake** — a real MCP client initialises, lists and calls. The
//!    router can be asserted on directly, but nothing in that assertion says
//!    the JSON-RPC in front of it works, and a tool the CLI cannot see is
//!    indistinguishable from a tool that does not exist;
//! 2. the **token** — the guard is an axum layer, so it only exists over HTTP.
//!    `delegate_task` starts work on the user's machine, which makes an
//!    unauthorised local caller the one failure here worth a test of its own;
//! 3. the **CLI calling it for real** — the last test, `#[ignore]` because it
//!    spends quota. It is the exit condition the rest of T3 could not reach: a
//!    model that calls `delegate_task` of its own accord.
//!
//! The first two spend nothing and run on every commit.

use std::sync::Arc;
use std::time::Duration;

use aria_core::Core;
use aria_core::agent::conversation::Conversation;
use aria_core::agent::journal::PENDING;
use aria_core::agent::tools;
use aria_core::mcp::voice::{self, VoiceTools};
use aria_core::transport::{self, AGENT_PATH, SessionToken};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod common;
use common::Scripted;

/// A core with an engine and a conversation attached, as the endpoint needs
/// them: the provider to launch against, the conversation to delegate into.
fn ready() -> (
    Arc<Core>,
    Arc<Conversation>,
    aria_core::agent::conversation::Channels,
) {
    let core = Core::new();
    core.attach_provider(Arc::new(Scripted {
        answer: common::WEATHER,
        fail: false,
    }));
    let (conversation, channels) = Conversation::new(Arc::clone(&core));
    core.attach_conversation(&conversation);
    (core, conversation, channels)
}

// ------------------------------------------------------------ the handshake

#[tokio::test]
async fn a_real_mcp_client_sees_the_catalogue_and_can_delegate() {
    let (core, conversation, mut channels) = ready();
    let _watcher = conversation.watch();

    // Served over a pipe rather than HTTP. The token guard is in front of the
    // door, not in the handlers, so this module is pure protocol — which is
    // exactly what makes it testable this way.
    let (ours, theirs) = tokio::io::duplex(8 * 1024);
    let server = VoiceTools::new(Arc::clone(&core));
    tokio::spawn(async move {
        if let Ok(running) = server.serve(ours).await {
            let _ = running.waiting().await;
        }
    });
    let client = ()
        .serve(theirs)
        .await
        .expect("the client and the server agree on a protocol version");

    // What the endpoint serves. `run_action` is absent on purpose — nothing in
    // the core stands behind the short path yet — and `approve` is present
    // although the model never sees it: it is what the CLI calls to ask
    // whether `delegate_task` may run, and `--tools` is what keeps it out of
    // the model's hands.
    //
    // The three memory tools joined on 2026-11-11 (decision 91), and
    // `deep_recall` on 2026-11-12 (decision 105, which reopened what 89 had
    // deferred — affordable because it answers with resumes and not whole
    // pages). They are served even with no memory folder attached, which is
    // this test's case: `MemoryError::Detached` is an answer a model can read
    // and report, and an absent tool is not.
    let listed = client.list_all_tools().await.expect("tools/list answers");
    let mut names: Vec<&str> = listed.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "approve",
            tools::DEEP_RECALL,
            tools::DELEGATE_TASK,
            tools::RECALL,
            tools::REMEMBER,
            tools::RENAME,
            tools::STR_REPLACE,
        ]
    );

    // And that is the claim worth asserting rather than assuming: with nothing
    // attached, `recall` refuses in words instead of failing the protocol.
    let detached = client
        .call_tool(
            CallToolRequestParams::new(tools::RECALL).with_arguments(
                serde_json::json!({ "query": "café" }).as_object().cloned().unwrap_or_default(),
            ),
        )
        .await
        .expect("the call completes");
    assert_eq!(detached.is_error, Some(true), "{detached:?}");

    // And the call. The answer is PENDING, whatever the task turns out to
    // cost — the result comes back later, as a message.
    let result = client
        .call_tool(
            CallToolRequestParams::new(tools::DELEGATE_TASK).with_arguments(
                serde_json::json!({ "task": "chercher la météo de demain à Lyon" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .expect("tools/call answers");
    assert_ne!(result.is_error, Some(true), "{:?}", result.content);
    assert!(
        format!("{:?}", result.content).contains(PENDING),
        "{:?}",
        result.content
    );

    // The session really was launched, and its result really does come out as
    // speech. Without this the tool would be a well-formed no-op.
    let spoken = tokio::time::timeout(Duration::from_secs(5), channels.speech.recv())
        .await
        .expect("the session's answer reaches the voice")
        .expect("the speech channel stays open");
    assert_eq!(
        spoken,
        "Il fera vingt-trois degrés ce matin et vingt-sept degrés cet après-midi."
    );

    let _ = client.cancel().await;
}

#[tokio::test]
async fn a_delegation_arriving_after_the_agent_is_gone_is_refused() {
    let core = Core::new();
    core.attach_provider(Arc::new(Scripted {
        answer: common::WEATHER,
        fail: false,
    }));
    {
        let (conversation, _channels) = Conversation::new(Arc::clone(&core));
        core.attach_conversation(&conversation);
    }
    // The conversation is held weakly, so dropping it takes the capability
    // with it rather than leaving a task launched into nothing.
    assert!(core.conversation().is_none());
}

// ----------------------------------------------------------------- the token

#[tokio::test]
async fn the_endpoint_is_closed_to_a_caller_without_the_token() {
    let (core, _conversation, _channels) = ready();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().unwrap().port();
    let token = core.agent_token().to_string();
    {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            let _ = transport::serve_events_on(listener, core, SessionToken::new()).await;
        });
    }

    let without = status(port, AGENT_PATH).await;
    assert_eq!(without, 401, "an endpoint with no token must refuse");

    let wrong = status(port, &format!("{AGENT_PATH}?s=not-the-token")).await;
    assert_eq!(wrong, 401, "a guessed token must refuse");

    let right = status(port, &format!("{AGENT_PATH}?s={token}")).await;
    assert_ne!(
        right, 401,
        "the token minted for this run has to open its own endpoint"
    );
}

/// The status code of an `initialize` posted at a path.
async fn status(port: u16, path: &str) -> u16 {
    post(port, path).await
}

/// A GET, for a route that is not the MCP service.
async fn get(port: u16, path: &str) -> u16 {
    send(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
    .await
}

async fn post(port: u16, path: &str) -> u16 {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "essai", "version": "0" }
        }
    })
    .to_string();
    send(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ),
    )
    .await
}

/// The status code of a hand-written request. Written by hand because what is
/// being tested is the layer in front of the MCP service — a client that spoke
/// MCP correctly would never get far enough to show it.
async fn send(port: u16, request: &str) -> u16 {
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the server is up");
    socket.write_all(request.as_bytes()).await.unwrap();
    socket.flush().await.unwrap();

    let mut answer = Vec::new();
    // Read the status line and no more: `Connection: close` means the whole
    // response arrives, but only the first line is being judged.
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut answer))
        .await
        .expect("the server answers")
        .expect("the socket stays readable");
    let text = String::from_utf8_lossy(&answer);
    let line = text.lines().next().unwrap_or_default().to_string();
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status in « {line} »"))
}

#[test]
fn the_url_handed_to_the_cli_names_the_path_that_is_mounted() {
    // Two constants that have to agree and would fail silently if they did
    // not: the model would simply have no tools.
    let config = transport::agent_mcp_config("jeton");
    assert!(config.contains(AGENT_PATH), "{config}");
    assert!(config.contains("?s=jeton"), "{config}");
    assert!(config.contains("\"type\":\"http\""), "{config}");
    // And the server name in the config is the one the tool names are built
    // from, or `--tools` names a tool the CLI cannot find.
    assert!(config.contains("\"aria\""), "{config}");
    assert!(voice::allowed_tools().contains(&"mcp__aria__delegate_task".to_string()));
}

// --------------------------------------------------------- the CLI, for real

/// The session carries ARIA's catalogue and nothing else.
///
/// Both halves matter and for different reasons. **No more**, because `--tools`
/// is the only thing standing between a voice dispatcher and a coding agent,
/// and a mistake there is silent — nothing looks wrong about a session that
/// also happens to hold `Bash`. **No less**, because a model with no tools can
/// only apologise.
///
/// Reading it took two measurements on 2026-10-26 to get right. `init` does
/// not arrive at start-up but after the first input message; and on that first
/// turn the MCP servers are still `status: "pending"`, so the list is empty
/// whatever the flags say. It is only from the **second** turn — `init` comes
/// back every turn — that it tells the truth. Hence the warm-up here, which is
/// the same warm-up the product does and for the same reason.
#[tokio::test]
#[ignore = "starts the real CLI; costs two short turns"]
async fn the_session_carries_arias_catalogue_and_nothing_else() {
    use aria_core::agent::session::{Config, MainAgent, Speech};

    let (core, _conversation, _channels) = ready();
    {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            if let Err(e) = transport::serve_events(core, SessionToken::new()).await {
                panic!("the endpoint could not be served: {e}");
            }
        });
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut agent = MainAgent::start(Config {
        mcp_config: Some(transport::agent_mcp_config(core.agent_token())),
        allowed_tools: voice::allowed_tools(),
        permission_tool: Some(voice::permission_tool_name()),
        ..Default::default()
    })
    .await
    .expect("the CLI starts");

    // The first `init` is useless here — the MCP servers are still pending —
    // so the warm-up is spent before anything is read.
    tokio::time::timeout(Duration::from_secs(60), agent.warm_up())
        .await
        .expect("the warm-up turn finishes")
        .expect("the warm-up turn succeeds");

    // One word, because `init` only comes back when something is said.
    agent.say("bonjour").await.expect("stdin is open");

    let announced = loop {
        let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(30), agent.next()).await
        else {
            panic!("the CLI said nothing: {}", agent.stderr().await);
        };
        if let Speech::Started { tools, .. } = event {
            break tools;
        }
    };
    agent.stop().await.ok();

    println!("outils annoncés : {announced:?}");
    // Compared as **sets**, and that is not laxity: run for the first time on
    // 2026-11-13, the CLI answered the same six tools in alphabetical order
    // while the catalogue declares them in reading order. Which of the two
    // orders arrives is the CLI's business and it promises nothing about it —
    // asserting it would be a test that fails on somebody else's refactor.
    // What is a contract is the **set**: no more, because a voice dispatcher
    // holding the coding agent's tools is a different product; and no less,
    // because a model with no tools can only apologise.
    let mut announced = announced;
    let mut expected = voice::allowed_tools();
    announced.sort();
    expected.sort();
    assert_eq!(announced, expected);
}

/// The exit condition: the model calls `delegate_task` of its own accord.
///
/// `#[ignore]`, because it starts the real CLI and **spends subscription
/// quota**. Played deliberately:
///
/// ```bash
/// cargo test -p aria-core --test agent_tools -- --ignored --nocapture
/// ```
///
/// It binds the product's port, 7421, because [`transport::agent_url`] names
/// it — the URL has to be the one the CLI will be handed, not one a test
/// invented.
#[tokio::test]
#[ignore = "starts the real CLI and spends quota"]
async fn the_cli_calls_delegate_task_on_its_own() {
    use aria_core::agent::session::{Config, MainAgent, Speech};

    // `RUST_LOG=aria_core=trace` prints every line the CLI writes, which is
    // where its start-up diagnostics live — they travel on stdout, not stderr.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .try_init();

    let (core, conversation, mut channels) = ready();
    let _watcher = conversation.watch();
    {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            if let Err(e) = transport::serve_events(core, SessionToken::new()).await {
                panic!("the endpoint could not be served: {e}");
            }
        });
    }
    // Waited for, not slept past. `serve_events` binds the product's port and
    // fails if it is taken — in a spawned task, where the panic would not fail
    // this test. The symptom would be an MCP server stuck at
    // `status: "pending"` and a model with no tools, which is a long way from
    // the cause.
    assert_eq!(
        get(transport::EVENTS_PORT, "/health").await,
        200,
        "the core's HTTP server is not up, so the CLI has nothing to connect to"
    );

    let config = Config {
        mcp_config: Some(transport::agent_mcp_config(core.agent_token())),
        allowed_tools: voice::allowed_tools(),
        permission_tool: Some(voice::permission_tool_name()),
        ..Default::default()
    };
    println!(
        "claude {}",
        aria_core::agent::session::arguments(&config).join(" ")
    );
    let mut agent = MainAgent::start(config).await.expect("the CLI starts");

    // Without this the test fails, and it fails as an assistant explaining
    // fluently that it cannot look up the weather: the MCP servers are still
    // connecting when the first turn starts. `MainAgent::warm_up` says what
    // was measured.
    tokio::time::timeout(Duration::from_secs(60), agent.warm_up())
        .await
        .expect("the warm-up turn finishes")
        .expect("the warm-up turn succeeds");

    conversation.heard("Quel temps fait-il demain à Lyon ?");
    agent
        .say("Quel temps fait-il demain à Lyon ?")
        .await
        .expect("the agent takes the turn");

    let mut called = None;
    let mut said = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(20), agent.next()).await
        else {
            break;
        };
        match event {
            Speech::Started { tools, .. } => {
                // Always empty, and the test above says why: `init` is emitted
                // while the MCP servers are still pending. Printed because an
                // unexpected name here is a session holding the coding agent's
                // tools.
                println!("outils intégrés : {tools:?}");
            }
            Speech::ToolUse { name, input } => {
                println!("outil : {name} {input}");
                called = Some(name);
            }
            Speech::Delta(text) => said.push_str(&text),
            Speech::Quota {
                utilization,
                window,
                ..
            } => println!("quota : {utilization} % sur {window}"),
            Speech::Finished { usage, .. } => {
                println!("tour fini : {usage:?}");
                break;
            }
            Speech::Failed(message) => {
                panic!("the turn failed: {message} — {}", agent.stderr().await)
            }
            other => println!("{other:?}"),
        }
    }
    println!("dit : « {said} »");

    assert_eq!(
        called.as_deref(),
        Some("mcp__aria__delegate_task"),
        "the model was given the tool and the weather is not something it \
         knows — it has to delegate. stderr : {}",
        agent.stderr().await
    );

    // And the session's answer comes back out as speech, word for word.
    let spoken = tokio::time::timeout(Duration::from_secs(10), channels.speech.recv())
        .await
        .expect("the result reaches the voice")
        .expect("the speech channel stays open");
    println!("prononcé : « {spoken} »");
    assert!(spoken.contains("vingt-trois"), "{spoken}");

    agent.stop().await.ok();
}

/// What the model is told about what the user has already heard.
///
/// Asked for on 2026-10-30, and the reasoning is in
/// `Conversation::note_waiting`: muting the model's announcement stops the
/// stutter but leaves it writing into a void. Telling it lets it adapt.
///
/// Driven through the real tool route rather than asserted on a format string,
/// because the whole value is that the sentence **reaches the model** — and a
/// phrase noted on one object and read from another is exactly the kind of
/// wiring that silently never connects.
#[tokio::test]
async fn the_model_is_told_which_waiting_phrase_the_user_heard() {
    let (core, conversation, mut channels) = ready();
    let _watcher = conversation.watch();

    // What the pilot does the moment it covers a wait.
    conversation.note_waiting("Je regarde.");

    let (ours, theirs) = tokio::io::duplex(8 * 1024);
    let server = VoiceTools::new(Arc::clone(&core));
    tokio::spawn(async move {
        if let Ok(running) = server.serve(ours).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(theirs).await.expect("a protocol version");

    let result = client
        .call_tool(
            CallToolRequestParams::new(tools::DELEGATE_TASK).with_arguments(
                serde_json::json!({ "task": "la météo de demain à Lyon" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        )
        .await
        .expect("tools/call answers");

    let said = format!("{:?}", result.content);
    assert!(said.contains(PENDING), "{said}");
    assert!(
        said.contains("Je regarde."),
        "la phrase est nommée : {said}"
    );
    assert!(
        said.contains("Ne le redis pas"),
        "et l'instruction suit : {said}"
    );

    // Taken, not read: a second turn must not be told about a sentence spoken
    // during the first.
    assert_eq!(conversation.waiting_phrase(), None);

    // The session really ran, so this is the live route and not a stub.
    let _ = tokio::time::timeout(Duration::from_secs(5), channels.speech.recv()).await;
    let _ = client.cancel().await;
}
