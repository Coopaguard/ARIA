//! The memory post, on the wire (decision 106).
//!
//! Two things are tested here that a unit test of the router cannot see,
//! because each is *between* two pieces:
//!
//! 1. the **handshake** — a real MCP client initialises, lists, and gets a
//!    page back. A tool the CLI cannot see is indistinguishable from a tool
//!    that does not exist, and nothing in an assertion on the router says the
//!    JSON-RPC in front of it works;
//! 2. the **token** — the guard is an axum layer, so it only exists over HTTP.
//!    And the token here is a *session's*, not the main agent's, which is the
//!    one difference from `agent_tools.rs` worth a test of its own: a session
//!    that has ended must stop opening the door.
//!
//! Both spend nothing and run on every commit. No vectoriser is attached, so
//! the deep search falls back to the lexical half — which is decision 83's
//! degradation and exactly the state a first start is in.

use std::sync::Arc;

use aria_core::Core;
use aria_core::mcp::subagent::{self, MemoryTools};
use aria_core::store::{Memory, Root};
use aria_core::transport::{self, MEMORY_PATH, SessionToken};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A core holding a memory with one page in it.
///
/// The temporary directory is returned because dropping it deletes the pages,
/// and a page deleted under the index is a different test.
fn ready() -> (Arc<Core>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = Root::open(dir.path()).expect("root");
    let mut memory = Memory::open(root).expect("memory");
    memory
        .remember(
            "Il prend son café serré, sans sucre.",
            Some("profil/louis"),
        )
        .expect("page");
    let core = Core::new();
    core.attach_memory(memory);
    (core, dir)
}

// ------------------------------------------------------------ the handshake

#[tokio::test]
async fn a_real_mcp_client_lists_three_tools_and_gets_a_page() {
    let (core, _dir) = ready();

    // Over a pipe rather than HTTP: the token guard is a layer in front of the
    // door, not in the handlers, which is what makes this module pure
    // protocol and testable this way.
    let (ours, theirs) = tokio::io::duplex(16 * 1024);
    let server = MemoryTools::new(Arc::clone(&core));
    tokio::spawn(async move {
        if let Ok(running) = server.serve(ours).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(theirs).await.expect("the client initialises");

    let listed = client.list_all_tools().await.expect("tools/list answers");
    let mut names: Vec<&str> = listed.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    // Two reads and a proposal. The writes stay on the voice post, where there
    // is a human in the room — see `aria_core::mcp::subagent`.
    assert_eq!(
        names,
        vec![subagent::PROPOSE, subagent::READ_PAGE, subagent::RECALL]
    );

    // A resume, with the id a `read_page` then needs.
    let found = client
        .call_tool(CallToolRequestParams::new(subagent::RECALL).with_arguments(
            serde_json::json!({ "query": "café" }).as_object().cloned().unwrap_or_default(),
        ))
        .await
        .expect("recall answers");
    let said = format!("{:?}", found.content);
    assert!(said.contains("profil/louis"), "{said}");
    assert!(said.contains("café serré"), "{said}");

    // And the page in full, which is the half a resume does not carry.
    let page = client
        .call_tool(CallToolRequestParams::new(subagent::READ_PAGE).with_arguments(
            serde_json::json!({ "page": "profil/louis" })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ))
        .await
        .expect("read_page answers");
    let read = format!("{:?}", page.content);
    // The header is what tells a resume from a page: `recall` never returns
    // it, and it is where `sources` and `verified` live. Spelled `type:` on
    // disk and `kind` in the struct, which is OKF's name for it.
    assert!(read.contains("type: profil"), "{read}");
    assert!(read.contains("title: Louis"), "{read}");
    assert!(read.contains("## Faits"), "{read}");
    assert!(read.contains("café serré"), "{read}");

    // An id nobody holds is answered in words, with what to do next, rather
    // than as a protocol error that would end the session's turn.
    let missing = client
        .call_tool(CallToolRequestParams::new(subagent::READ_PAGE).with_arguments(
            serde_json::json!({ "page": "profil/personne" })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ))
        .await
        .expect("a missing page is an answer, not a failure");
    let refusal = format!("{:?}", missing.content);
    assert!(refusal.contains("recall"), "{refusal}");

    // A proposal needs to know whose session it is, and a pipe carries no
    // token: refused in words, and nothing is kept.
    let orphan = client
        .call_tool(CallToolRequestParams::new(subagent::PROPOSE).with_arguments(
            serde_json::json!({ "fact": "Il aime le thé." })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ))
        .await
        .expect("a proposal without a session is an answer, not a failure");
    let said = format!("{:?}", orphan.content);
    assert!(said.contains("pas reconnue"), "{said}");
}

// -------------------------------------------------------------- the proposal

/// Over HTTP, with the session's token: the proposal lands under that session,
/// which is the routing the pipe above cannot carry.
#[tokio::test]
async fn a_session_proposes_over_http_and_the_proposal_is_its_own() {
    let (core, _dir) = ready();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().unwrap().port();
    let session = core.open_session("une tâche", None);
    let token = core.session_routes().open(session);
    {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            let _ = transport::serve_events_on(listener, core, SessionToken::new()).await;
        });
    }
    let path = format!("{MEMORY_PATH}?s={token}");

    let (status, head, _) = exchange(port, &path, None, &initialize()).await;
    assert_eq!(status, 200, "{head}");
    let id = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("mcp-session-id")
                .then(|| value.trim().to_string())
        })
        .expect("a stateful server names its session");
    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    exchange(port, &path, Some(&id), &initialized).await;

    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": subagent::PROPOSE,
            "arguments": {
                "fact": "Il a une imprimante Brother.",
                "page": "materiel/imprimante"
            }
        }
    });
    let (status, _, body) = exchange(port, &path, Some(&id), &call).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("Proposé"), "{body}");

    let kept = core.take_memory_proposals(session);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].fact, "Il a une imprimante Brother.");
    assert_eq!(kept[0].page.as_deref(), Some("materiel/imprimante"));
}

fn initialize() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }
    })
}

/// One JSON-RPC message posted, and the status, head and body that came back.
async fn exchange(
    port: u16,
    path: &str,
    session: Option<&str>,
    message: &serde_json::Value,
) -> (u16, String, String) {
    const CRLF: &str = "\r\n";
    let body = message.to_string();
    let mut request = format!("POST {path} HTTP/1.1{CRLF}Host: 127.0.0.1{CRLF}");
    request.push_str(&format!("Content-Type: application/json{CRLF}"));
    request.push_str(&format!("Accept: application/json, text/event-stream{CRLF}"));
    if let Some(id) = session {
        request.push_str(&format!("Mcp-Session-Id: {id}{CRLF}"));
    }
    request.push_str(&format!("Content-Length: {}{CRLF}", body.len()));
    request.push_str(&format!("Connection: close{CRLF}{CRLF}{body}"));

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the listener is up");
    stream.write_all(request.as_bytes()).await.expect("sent");
    let mut response = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut response),
    )
    .await;
    let text = String::from_utf8_lossy(&response).into_owned();
    let blank = format!("{CRLF}{CRLF}");
    let (head, body) = text.split_once(blank.as_str()).unwrap_or((text.as_str(), ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, head.to_string(), body.to_string())
}

// ----------------------------------------------------------------- the token

#[tokio::test]
async fn only_a_live_session_token_opens_the_memory() {
    let (core, _dir) = ready();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().unwrap().port();

    let session = core.open_session("une tâche", None);
    let token = core.session_routes().open(session);

    {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            let _ = transport::serve_events_on(listener, core, SessionToken::new()).await;
        });
    }

    assert_eq!(
        post(port, MEMORY_PATH).await,
        401,
        "sans jeton, la mémoire ne répond pas"
    );
    assert_eq!(
        post(port, &format!("{MEMORY_PATH}?s=pas-le-jeton")).await,
        401,
        "un jeton deviné ne doit pas ouvrir"
    );
    // 200 and not merely "not 401": a 404 from a path mounted wrong would
    // pass the weaker assertion, and the endpoint would be silently absent.
    assert_eq!(
        post(port, &format!("{MEMORY_PATH}?s={token}")).await,
        200,
        "le jeton de la session doit ouvrir son propre point d'accès"
    );

    // And the session ends. This is the half that makes the exposure window
    // the life of a session rather than the life of the application.
    core.session_routes().close(session);
    assert_eq!(
        post(port, &format!("{MEMORY_PATH}?s={token}")).await,
        401,
        "le jeton d'une session terminée doit cesser d'ouvrir"
    );
}

/// The status code of an `initialize` posted at a path.
async fn post(port: u16, path: &str) -> u16 {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }
    })
    .to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    send(port, &request).await
}

async fn send(port: u16, request: &str) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the listener is up");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("the request is sent");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("the response is read");
    let head = String::from_utf8_lossy(&response);
    head.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}
