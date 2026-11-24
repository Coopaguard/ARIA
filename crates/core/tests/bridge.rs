//! The bridge, against the real CLI — and the provider trait, against a fake one.
//!
//! Two exit conditions live here.
//!
//! **Epic 01** wants an end-to-end run: launch a session, ask a short task,
//! read the stream to the end, **close the process**, then resume it by its
//! identifier in a *new* process and get an answer that accounts for the first
//! exchange. That test is marked `#[ignore]` and the reason is not flakiness:
//! it spends the user's subscription quota. Run it deliberately:
//!
//! ```text
//! cargo test -p aria-core --test bridge -- --ignored --nocapture
//! ```
//!
//! **Epic 03** wants proof that a second provider plugs in without touching
//! anything else — "even one that only echoes, even one that can honour two
//! profiles out of four". [`Echo`] below is exactly that, written from outside
//! the module it implements, and it runs in the ordinary suite.

use std::sync::Arc;
use std::time::Duration;

use aria_core::Core;
use aria_core::permissions::Profile;
use aria_core::providers::claude_code::ClaudeCode;
use aria_core::providers::{
    AgentProvider, AgentSession, Availability, LaunchSpec, NativeMode, ProfileSupport,
    ProviderEvent, Usage,
};
use aria_core::sessions::SessionState;

// ---------------------------------------------------------------- the fake one

/// A provider that answers without doing anything, and honours half the
/// profiles.
///
/// Its point is the half it cannot honour: `None` has to travel all the way to
/// the settings screen, or "the interface says which are missing" is a sentence
/// with no mechanism behind it.
struct Echo;

struct EchoSession {
    tx: tokio::sync::mpsc::Sender<ProviderEvent>,
}

#[async_trait::async_trait]
impl AgentSession for EchoSession {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        let _ = self
            .tx
            .send(ProviderEvent::TextDelta(text.to_string()))
            .await;
        let _ = self
            .tx
            .send(ProviderEvent::Finished {
                summary: format!("écho : {text}"),
                usage: Usage::default(),
            })
            .await;
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn native_id(&self) -> Option<String> {
        Some("echo-1".into())
    }
}

#[async_trait::async_trait]
impl AgentProvider for Echo {
    fn id(&self) -> &'static str {
        "echo"
    }

    fn display_name(&self) -> &'static str {
        "Écho (factice)"
    }

    fn profile_mapping(&self) -> Vec<ProfileSupport> {
        vec![
            ProfileSupport {
                profile: Profile::Plan,
                native: Some(NativeMode {
                    name: "read-only".into(),
                    explanation: "ne fait rien, répète".into(),
                }),
            },
            ProfileSupport {
                profile: Profile::Strict,
                native: Some(NativeMode {
                    name: "ask".into(),
                    explanation: "ne fait rien, répète".into(),
                }),
            },
            // The two it cannot honour. This is the interesting half.
            ProfileSupport {
                profile: Profile::Normal,
                native: None,
            },
            ProfileSupport {
                profile: Profile::Autonomous,
                native: None,
            },
        ]
    }

    async fn availability(&self) -> Availability {
        Availability::Ready {
            version: "0".into(),
        }
    }

    async fn launch(
        &self,
        spec: LaunchSpec,
    ) -> anyhow::Result<(
        Box<dyn AgentSession>,
        tokio::sync::mpsc::Receiver<ProviderEvent>,
    )> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let _ = tx
            .send(ProviderEvent::Started {
                native_id: "echo-1".into(),
                tools: vec![],
                mcp_servers: vec![],
            })
            .await;
        let mut session = EchoSession { tx: tx.clone() };
        if !spec.prompt.is_empty() {
            session.send(&spec.prompt).await?;
        }
        Ok((Box::new(session), rx))
    }
}

#[tokio::test]
async fn a_second_provider_plugs_in_without_touching_anything_else() {
    let core = Core::new();
    core.attach_provider(Arc::new(Echo));

    let info = core.provider_info().await.expect("a provider is attached");
    assert_eq!(info.display_name, "Écho (factice)");
    assert!(info.availability.can_launch());
    assert_eq!(info.mapping.len(), 4);

    // The half it cannot honour has to be visible, not absent: the settings
    // screen says which are missing rather than offering a setting with no
    // effect.
    let missing: Vec<Profile> = info
        .mapping
        .iter()
        .filter(|row| row.native.is_none())
        .map(|row| row.profile)
        .collect();
    assert_eq!(missing, vec![Profile::Normal, Profile::Autonomous]);
}

#[tokio::test]
async fn a_launched_session_reaches_the_registry_and_the_stream() {
    let core = Core::new();
    core.attach_provider(Arc::new(Echo));

    let before = core.broker().next_seq();
    let id = core
        .launch_session(
            "quel temps fait-il",
            LaunchSpec {
                prompt: "bonjour".into(),
                profile: Profile::Strict,
                ..Default::default()
            },
        )
        .await
        .expect("the echo provider always launches");

    // The pump runs on its own task; give it a moment to drain.
    tokio::time::sleep(Duration::from_millis(80)).await;

    let session = core
        .sessions()
        .into_iter()
        .find(|s| s.id == id)
        .expect("the session is in the registry");
    // The profile travels with the session, because epic 04 displays it.
    assert_eq!(session.profile, Profile::Strict);
    assert_eq!(session.state, SessionState::Done);
    assert!(
        core.broker().next_seq() > before + 2,
        "every mutation publishes, so a whole session is several events"
    );
}

#[tokio::test]
async fn launching_without_a_provider_fails_and_says_so() {
    let core = Core::new();
    let result = core.launch_session("rien", LaunchSpec::default()).await;
    assert!(result.is_err(), "there is no engine to delegate to");
}

// ------------------------------------------------------------- the real one

/// The installed CLI's availability. Cheap — one `--version` — so it runs in
/// the ordinary suite, and it is the check epic 01 puts at every start-up.
#[tokio::test]
async fn the_installed_cli_is_inspected_rather_than_assumed() {
    let provider = ClaudeCode::new();
    match provider.availability().await {
        Availability::Ready { version } => {
            println!("CLI {version} — prête");
        }
        // On a machine without the CLI this is the correct answer, not a
        // failure: the reason is shown.
        other => {
            println!("{}", other.message());
            assert!(!other.can_launch());
        }
    }
}

/// Epic 01's exit condition, end to end, against the real CLI.
///
/// **Spends subscription quota.** Two short turns on the cheapest model.
#[tokio::test]
#[ignore = "lance la vraie CLI et consomme du quota — `-- --ignored` pour l'exécuter"]
async fn a_session_survives_its_process_and_resumes_with_its_memory() {
    let core = Core::new();
    let provider = Arc::new(ClaudeCode::new());
    if !provider.availability().await.can_launch() {
        eprintln!("CLI absente — test sauté");
        return;
    }
    core.attach_provider(provider.clone());

    // --- first process: say something it will have to remember.
    let first = core
        .launch_session(
            "mémoire",
            LaunchSpec {
                prompt: "Retiens ce nombre : 2026. Réponds seulement « noté ».".into(),
                profile: Profile::Plan,
                max_turns: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("the session launches");

    let finished = wait_until(&core, first, Duration::from_secs(120)).await;
    assert!(finished, "the first turn finished");

    // The native id is what a resume needs, and it only exists once the
    // provider has assigned one — which is why it lives on the session rather
    // than on the handle: the handle is about to be dropped.
    let native_id = core
        .session_native_id(first)
        .expect("the CLI gave us a session id");
    println!("session native : {native_id}");

    core.stop_session(first).await.expect("it stops cleanly");

    // Listen from here, so what the resumed process says can be read back.
    // Subscribing before launching, not after: the other order races the first
    // fragments of the answer.
    let mut heard = core.broker().subscribe();
    let said = tokio::spawn(async move {
        let mut text = String::new();
        while let Ok(envelope) = heard.recv().await {
            match envelope.event {
                aria_core::events::Event::MessageDelta { text: t, .. } => text.push_str(&t),
                aria_core::events::Event::TaskDone { .. }
                | aria_core::events::Event::TaskError { .. } => break,
                _ => {}
            }
        }
        text
    });

    // --- second process: resume, and check the memory came with it.
    let second = core
        .launch_session(
            "reprise",
            LaunchSpec {
                prompt: "Quel nombre t'ai-je demandé de retenir ? Réponds par le nombre seul."
                    .into(),
                profile: Profile::Plan,
                resume: Some(native_id),
                max_turns: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("the resume launches");

    assert!(
        wait_until(&core, second, Duration::from_secs(120)).await,
        "the resumed turn finished"
    );

    let answer = core
        .sessions()
        .into_iter()
        .find(|s| s.id == second)
        .expect("the resumed session is in the registry");
    println!("état : {:?}", answer.state);
    assert_eq!(
        answer.state,
        SessionState::Done,
        "a resume that fails must surface as a failure, not as silence"
    );

    // The exit condition is not "it restarted" — it is that the answer takes
    // the first exchange into account. A resume that opens a blank session
    // would also reach `Done`, and would be useless.
    let text = tokio::time::timeout(Duration::from_secs(5), said)
        .await
        .expect("the listener finished")
        .expect("the listener did not panic");
    println!("réponse : {text}");
    assert!(
        text.contains("2026"),
        "the resumed session had to remember the number from the first \
         process; it answered « {text} »"
    );
}

/// Poll until the session leaves the live states, or give up.
async fn wait_until(core: &Arc<Core>, id: aria_core::sessions::SessionId, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if let Some(session) = core.sessions().into_iter().find(|s| s.id == id)
            && !session.state.is_live()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}
