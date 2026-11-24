//! The whole post, driven the way the window drives it.
//!
//! This is the test to play before opening the interface, because it exercises
//! exactly what the window will and prints what it saw: a turn goes in through
//! [`aria_core::Core::heard_turn`] — the same door the microphone and the
//! keyboard both use — and what ARIA says comes back out as `spoken` events on
//! the real stream.
//!
//! `#[ignore]`, because it starts the real CLI and **spends subscription
//! quota**. Played deliberately:
//!
//! ```bash
//! cargo test -p aria-core --test pilot -- --ignored --nocapture
//! ```
//!
//! The delegated session is a double, so the only quota spent is the agent's
//! own two turns: the warm-up and the question.

use std::sync::Arc;
use std::time::Duration;

use aria_core::Core;
use aria_core::agent::pilot::{self, Pilot};
use aria_core::events::Event;
use aria_core::transport::{self, SessionToken};

mod common;
use common::Scripted;

#[tokio::test]
#[ignore = "starts the real CLI and spends quota"]
async fn a_turn_goes_in_and_aria_answers_out_loud() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,aria_core=debug".into()),
        )
        .try_init();

    let core = Core::new();
    // A double for the delegated session. What is under test is the post, not
    // the weather, and a real session would spend quota of its own.
    core.attach_provider(Arc::new(Scripted {
        answer: common::WEATHER,
        fail: false,
    }));

    // Bound before anything starts, and bound synchronously, exactly as the
    // application does it: the agent's MCP endpoint lives on this listener and
    // the CLI connects to it on its first turn.
    let listener =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, transport::EVENTS_PORT))
            .unwrap_or_else(|e| {
                // The port is pinned, not ephemeral — the window's CSP names it —
                // so this test and a running application cannot share it. Said
                // with the fix in it, because the OS error alone sends you looking
                // in the wrong place.
                panic!(
                    "le port {} est pris : ARIA tourne déjà. Ferme `npm run dev` \
                 (sous Windows le wrapper npm meurt sans ses enfants — \
                 `taskkill /IM aria.exe /F`) puis rejoue. ({e})",
                    transport::EVENTS_PORT
                )
            });
    {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            let _ = transport::serve_events_on(listener, core, SessionToken::new()).await;
        });
    }

    // Subscribed BEFORE the pilot starts. The other order drops whatever is
    // published while the agent is warming up.
    let mut events = core.broker().subscribe();

    let config = pilot::config(&core);
    let _pilot = Pilot::start(Arc::clone(&core), config)
        .await
        .expect("l'agent principal démarre — la CLI est-elle sur le PATH ?");

    // The one door. The microphone calls this, the keyboard calls this.
    core.heard_turn("Quel temps fait-il demain à Lyon ?");

    let mut spoken: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while tokio::time::Instant::now() < deadline {
        let Ok(Ok(envelope)) = tokio::time::timeout(Duration::from_secs(25), events.recv()).await
        else {
            break;
        };
        match envelope.event {
            Event::Spoken { text } => {
                println!("ARIA : « {text} »");
                let done = text.contains("vingt-trois");
                spoken.push(text);
                if done {
                    break;
                }
            }
            Event::SessionOpened { title, .. } => println!("session : {title}"),
            Event::Transcript { text, final_ } => println!("entendu ({final_}) : {text}"),
            _ => {}
        }
    }

    assert!(
        !spoken.is_empty(),
        "ARIA n'a rien dit. Le modèle a-t-il ses outils ? Rejoue avec \
         RUST_LOG=aria_core=trace"
    );
    assert!(
        spoken.iter().any(|line| line.contains("vingt-trois")),
        "le résultat de la session n'est pas ressorti : {spoken:?}"
    );

    // The waiting phrase. T0 measured the model's first text at 1113 ms median
    // against a 350 ms grace period, so it should be there — but it is a race
    // against a network, so a miss is printed rather than failed.
    if spoken.len() < 2 {
        println!(
            "note : pas de phrase d'attente — le modèle a répondu en moins de \
             350 ms, ce qui serait une première"
        );
    }
}
