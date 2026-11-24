//! The permission relay, against the real CLI.
//!
//! Epic 02's exit condition: two sessions run at once, each asks for
//! permission, both requests appear **under their own session**, approving one
//! unblocks only that one, and each session carries on with the right answer.
//!
//! All of it is `#[ignore]`d, for the same reason as `bridge.rs`: it launches
//! Claude Code and spends subscription quota.
//!
//! ```text
//! cargo test -p aria-core --test relay -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` is not optional: these tests bind the real event port,
//! which is pinned because the window's CSP names it.
//!
//! # Why the tests ask for a file to be written
//!
//! Measured on CLI 2.1.165, 2026-10-25: in `--permission-mode default`,
//! `Bash(echo …)` ran **without** reaching the relay, while `Write` reached it
//! every time. The CLI clears some trivial commands on its own before the
//! relay is consulted — the same shape of limitation epic 02 already records
//! for `auto` mode, one mode earlier than expected. A test built on `echo`
//! would have been green for the wrong reason.

use std::sync::Arc;
use std::time::Duration;

use aria_core::Core;
use aria_core::events::Event;
use aria_core::permissions::{Channel, Decision, PermissionRequest, Profile};
use aria_core::providers::claude_code::ClaudeCode;
use aria_core::providers::{AgentProvider, LaunchSpec};
use aria_core::sessions::SessionId;
use aria_core::transport::{SessionToken, serve_events};

/// Stand the core up with its HTTP server, exactly as the application does.
///
/// There is no shortcut: the relay only works if the CLI can reach us, so the
/// test runs the real server on the real port.
async fn running_core() -> Option<Arc<Core>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .try_init();

    let core = Core::new();
    let provider = Arc::new(ClaudeCode::new());
    if !provider.availability().await.can_launch() {
        eprintln!("CLI absente — test sauté");
        return None;
    }
    core.attach_provider(provider);

    let served = Arc::clone(&core);
    tokio::spawn(async move {
        if let Err(e) = serve_events(served, SessionToken::new()).await {
            eprintln!("le serveur n'a pas démarré : {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    Some(core)
}

/// A directory the session may write in, and that nothing else uses.
fn sandbox(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aria-relay-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the sandbox can be created");
    dir
}

/// Ask for a file to be written — something the CLI genuinely relays.
fn writing_session(dir: &std::path::Path, file: &str) -> LaunchSpec {
    LaunchSpec {
        prompt: format!(
            "Crée le fichier {file} dans le répertoire de travail, contenant le mot OK. \
             N'explique rien."
        ),
        // `Strict` maps to `default`: the CLI asks before writing, which is
        // what sends the request through the relay.
        profile: Profile::Strict,
        cwd: Some(dir.to_path_buf()),
        max_turns: Some(4),
        ..Default::default()
    }
}

/// Wait for a request filed against one session.
async fn wait_for_request(
    core: &Arc<Core>,
    session: SessionId,
    limit: Duration,
) -> Option<PermissionRequest> {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if let Some(found) = core
            .pending_permissions()
            .into_iter()
            .find(|r| r.session == session)
        {
            return Some(found);
        }
        // Stop as soon as the session is over. Polling past the end wastes the
        // whole timeout and reports "no request arrived" long after the reason
        // stopped being a mystery.
        if core
            .sessions()
            .into_iter()
            .any(|s| s.id == session && !s.state.is_live())
        {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    None
}

/// What the session did, for a failure message worth reading.
///
/// "The relay was never consulted" is true and useless on its own: the next
/// question is always *what did it do instead*, and answering it costs another
/// run against the real CLI. So the answer is collected here.
fn what_happened(core: &Arc<Core>, session: SessionId) -> String {
    let Some(found) = core.sessions().into_iter().find(|s| s.id == session) else {
        return "la session a disparu du registre".into();
    };
    format!(
        "état {:?}, profil {:?}, issue {:?}",
        found.state, found.profile, found.native_id
    )
}

async fn wait_until_finished(core: &Arc<Core>, session: SessionId, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if core
            .sessions()
            .into_iter()
            .any(|s| s.id == session && !s.state.is_live())
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    false
}

#[tokio::test]
#[ignore = "lance la vraie CLI et consomme du quota"]
async fn the_cli_connects_to_our_relay_and_finds_the_tool() {
    let Some(core) = running_core().await else {
        return;
    };

    let mut events = core.broker().subscribe();
    core.launch_session(
        "connexion",
        LaunchSpec {
            prompt: "Dis juste « bonjour ».".into(),
            profile: Profile::Plan,
            max_turns: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect("the session launches");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut engaged = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
            Ok(Ok(envelope)) => {
                if matches!(envelope.event, Event::SessionEngaged { .. }) {
                    engaged = true;
                }
                if matches!(
                    envelope.event,
                    Event::TaskDone { .. } | Event::TaskError { .. }
                ) {
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(engaged, "the session never reported an init event");
}

/// The whole mechanism once: a request reaches the desk, the answer goes back,
/// the session acts on it.
#[tokio::test]
#[ignore = "lance la vraie CLI et consomme du quota"]
async fn an_approval_lets_the_action_through() {
    let Some(core) = running_core().await else {
        return;
    };
    let dir = sandbox("allow");

    let id = core
        .launch_session("autorisation", writing_session(&dir, "preuve.txt"))
        .await
        .expect("the session launches");

    let request = match wait_for_request(&core, id, Duration::from_secs(90)).await {
        Some(request) => request,
        None => panic!(
            "the relay was never consulted — {}. Collect the raw stream with              RUST_LOG=aria_core=trace to see which tool it reached for.",
            what_happened(&core, id)
        ),
    };
    println!("demande : {} — {:?}", request.tool, request.command);
    assert_eq!(request.session, id, "a request has to carry its session");

    core.answer_permission(request.id, Decision::Allow, Channel::Window)
        .expect("the answer is accepted");

    assert!(
        wait_until_finished(&core, id, Duration::from_secs(90)).await,
        "the session never got past its permission request"
    );
    assert!(
        dir.join("preuve.txt").exists(),
        "approving has to actually let the action through — a relay that says \
         yes and changes nothing is worse than one that says no"
    );
}

/// The other half, and the one that matters more: a refusal stops it.
#[tokio::test]
#[ignore = "lance la vraie CLI et consomme du quota"]
async fn a_refusal_stops_the_action() {
    let Some(core) = running_core().await else {
        return;
    };
    let dir = sandbox("deny");

    let id = core
        .launch_session("refus", writing_session(&dir, "interdit.txt"))
        .await
        .expect("the session launches");

    let request = match wait_for_request(&core, id, Duration::from_secs(90)).await {
        Some(request) => request,
        None => panic!(
            "the relay was never consulted — {}",
            what_happened(&core, id)
        ),
    };

    core.answer_permission(request.id, Decision::Deny, Channel::Window)
        .expect("the answer is accepted");

    assert!(wait_until_finished(&core, id, Duration::from_secs(90)).await);
    assert!(
        !dir.join("interdit.txt").exists(),
        "a refusal that does not stop the action is a guard rail in name only"
    );
}

/// Epic 02's exit condition, in full.
#[tokio::test]
#[ignore = "lance la vraie CLI et consomme du quota — deux sessions"]
async fn two_sessions_do_not_cross_wires() {
    let Some(core) = running_core().await else {
        return;
    };
    let first_dir = sandbox("first");
    let second_dir = sandbox("second");

    let first = core
        .launch_session("première", writing_session(&first_dir, "un.txt"))
        .await
        .expect("the first session launches");
    let second = core
        .launch_session("seconde", writing_session(&second_dir, "deux.txt"))
        .await
        .expect("the second session launches");

    let a = wait_for_request(&core, first, Duration::from_secs(120))
        .await
        .expect("the first session asked");
    let b = wait_for_request(&core, second, Duration::from_secs(120))
        .await
        .expect("the second session asked");

    // Each request under its own session — which is the whole reason the relay
    // endpoint carries a per-session token rather than a shared one.
    assert_eq!(a.session, first);
    assert_eq!(b.session, second);
    assert_ne!(a.id, b.id);

    // Approve one, refuse the other, and check each session did what it was
    // told — not what the other one was told.
    core.answer_permission(a.id, Decision::Allow, Channel::Window)
        .expect("the first answer is accepted");
    core.answer_permission(b.id, Decision::Deny, Channel::Window)
        .expect("the second answer is accepted");

    assert!(wait_until_finished(&core, first, Duration::from_secs(120)).await);
    assert!(wait_until_finished(&core, second, Duration::from_secs(120)).await);

    assert!(
        first_dir.join("un.txt").exists(),
        "the approved session was blocked by the other one's refusal"
    );
    assert!(
        !second_dir.join("deux.txt").exists(),
        "the refused session acted on the other one's approval"
    );
}
