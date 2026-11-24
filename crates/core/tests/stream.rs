//! The event stream, over a real socket.
//!
//! The broker's unit tests prove the ring and the sequence; they cannot see the
//! wire. These do, and they cover the two things the design rests on and that a
//! refactor can silently break:
//!
//!  - a client that arrives late is handed the history, in order;
//!  - a client that reconnects holding a position gets only what it missed, and
//!    never the same event twice.
//!
//! The second one is the subtle half. The server subscribes to the live channel
//! *before* reading the history, because the other order loses everything
//! published between the two calls — and that overlap then has to be dropped on
//! the way out. Get either half wrong and a reconnect doubles a message
//! fragment or an activity step, which looks like a rendering bug and is not.

use std::sync::Arc;
use std::time::Duration;

use aria_core::Core;
use aria_core::permissions::Profile;
use aria_core::sessions::SessionState;
use aria_core::transport::{SessionToken, serve_events_on};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

/// Start a core and its server on a port the OS picks. Returns the port and the
/// token, which is the only way in.
async fn start() -> (Arc<Core>, u16, SessionToken) {
    let core = Core::new();
    let token = SessionToken::new();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("loopback, port 0");
    let port = listener.local_addr().unwrap().port();

    let served = Arc::clone(&core);
    let served_token = token.clone();
    tokio::spawn(async move {
        let _ = serve_events_on(listener, served, served_token).await;
    });

    (core, port, token)
}

async fn connect(
    port: u16,
    token: &SessionToken,
    since: u64,
) -> impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin {
    let url = format!(
        "ws://127.0.0.1:{port}/events?token={}&since={since}",
        token.as_str()
    );
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("the token is the right one");
    socket
}

/// Read one text frame, giving up rather than hanging the suite.
async fn next_json(
    socket: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
) -> serde_json::Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("the server answers within five seconds")
        .expect("the stream is not closed")
        .expect("the frame is not an error");
    match frame {
        Message::Text(text) => serde_json::from_str(&text).expect("valid JSON"),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

#[tokio::test]
async fn a_late_client_is_handed_the_history_in_order() {
    let (core, port, token) = start().await;

    // Everything below happened before anybody was watching.
    let session = core.open_session("compiler le projet", Some(Profile::Strict));
    core.set_session_state(session, SessionState::Executing);
    core.finish_session(session, "compilé");

    let mut socket = connect(port, &token, 0).await;
    let replay = next_json(&mut socket).await;

    assert_eq!(replay["type"], "resume.replay");
    let events = replay["events"].as_array().expect("an array of events");
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        vec![
            "session.opened",
            "session.state",
            "session.state",
            "task.done"
        ],
        "the window draws itself from this, so the order is the contract"
    );
    // The profile travels with the session, because epic 04 displays it.
    assert_eq!(events[0]["profile"], "strict");
}

#[tokio::test]
async fn a_reconnecting_client_gets_what_it_missed_and_nothing_twice() {
    let (core, port, token) = start().await;

    let session = core.open_session("première", None);
    let mut first = connect(port, &token, 0).await;
    let replay = next_json(&mut first).await;
    let seen = replay["events"].as_array().unwrap().last().unwrap()["seq"]
        .as_u64()
        .unwrap();
    drop(first);

    // Two more events while nobody is connected. Sleep is not shutdown.
    core.set_session_state(session, SessionState::Executing);
    core.finish_session(session, "fini");

    let mut second = connect(port, &token, seen).await;
    let resumed = next_json(&mut second).await;
    assert_eq!(resumed["type"], "resume.replay");

    let seqs: Vec<u64> = resumed["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert!(
        seqs.iter().all(|seq| *seq > seen),
        "a replay that re-sends what the client holds doubles message fragments \
         and activity steps: {seqs:?} against a held position of {seen}"
    );
    assert_eq!(seqs.len(), 3, "state, state, task.done");
}

#[tokio::test]
async fn an_event_published_while_connected_arrives_live() {
    let (core, port, token) = start().await;
    let mut socket = connect(port, &token, 0).await;
    // Drain the (empty) replay first.
    let replay = next_json(&mut socket).await;
    assert_eq!(replay["type"], "resume.replay");

    core.open_session("en direct", None);

    let live = next_json(&mut socket).await;
    assert_eq!(live["type"], "session.opened");
    assert_eq!(live["title"], "en direct");
    // Flattened, as `serde` writes it: the event's fields sit alongside the
    // envelope's. The TypeScript mirror depends on exactly this shape.
    assert!(live["seq"].is_u64());
    assert!(live["at"].is_u64());
}

#[tokio::test]
async fn the_wrong_token_does_not_get_in() {
    let (_core, port, _token) = start().await;
    let url = format!("ws://127.0.0.1:{port}/events?token=not-the-one&since=0");
    assert!(
        tokio_tungstenite::connect_async(url).await.is_err(),
        "loopback keeps the network out, not other local processes — and the \
         tile sandbox is a local process by design"
    );
}
