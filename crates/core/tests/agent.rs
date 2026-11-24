//! The main voice agent, against the real CLI.
//!
//! `#[ignore]` and not because it is flaky: it **spends subscription quota**.
//! It is the exit check for the tier-A session of epic 08, and it is played
//! deliberately:
//!
//! ```bash
//! cargo test -p aria-core --test agent -- --ignored --nocapture
//! ```
//!
//! What it proves is what cannot be proved without a process: that one process
//! holds several turns, that the voice system prompt displaced the coding one,
//! and that the first word arrives inside the budget the filler was sized
//! against.

use std::time::{Duration, Instant};

use aria_core::agent::filler::{Filler, Waiting};
use aria_core::agent::session::{Config, MainAgent, Speech};

/// Longest a turn may take before the test gives up on it.
const PATIENCE: Duration = Duration::from_secs(60);

struct Turn {
    said: String,
    first_text: Option<Duration>,
    whole: Duration,
}

async fn one_turn(agent: &mut MainAgent, utterance: &str) -> Turn {
    let started = Instant::now();
    agent.say(utterance).await.expect("the session takes input");

    let mut said = String::new();
    let mut first_text = None;
    loop {
        let next = tokio::time::timeout(PATIENCE, agent.next()).await;
        match next {
            Err(_) => panic!(
                "no answer in {PATIENCE:?}\nstderr: {}",
                agent.stderr().await
            ),
            Ok(None) => panic!(
                "the session ended mid-turn\nstderr: {}",
                agent.stderr().await
            ),
            Ok(Some(Speech::Delta(text))) => {
                first_text.get_or_insert_with(|| started.elapsed());
                said.push_str(&text);
            }
            Ok(Some(Speech::Finished { text, usage })) => {
                println!(
                    "  {} jetons entrée ({} depuis le cache), {} sortie",
                    usage.input_tokens, usage.cache_read_input_tokens, usage.output_tokens
                );
                if said.is_empty() {
                    said = text;
                }
                return Turn {
                    said,
                    first_text,
                    whole: started.elapsed(),
                };
            }
            Ok(Some(Speech::Failed(message))) => panic!("the turn failed: {message}"),
            Ok(Some(other)) => println!("  ({other:?})"),
        }
    }
}

#[tokio::test]
#[ignore = "spends subscription quota"]
async fn one_process_holds_several_turns_and_speaks_french_without_markdown() {
    let mut agent = MainAgent::start(Config::default())
        .await
        .expect("the CLI is on PATH");

    let mut filler = Filler::new();
    let utterances = [
        "Bonjour, qui es-tu ?",
        "Et qu'est-ce que tu sais faire ?",
        "Très bien, merci.",
    ];

    let mut first_texts = Vec::new();
    for utterance in utterances {
        println!("\n> {utterance}");
        let turn = one_turn(&mut agent, utterance).await;
        println!(
            "  premier texte {:?}, tour complet {:?}",
            turn.first_text, turn.whole
        );
        println!("  « {} »", turn.said.trim());

        assert!(!turn.said.trim().is_empty(), "it said nothing");

        // Everything it writes is spoken, so these are not style preferences —
        // an asterisk and a bullet are both audible.
        for audible in ['*', '#', '`'] {
            assert!(
                !turn.said.contains(audible),
                "{audible} in a spoken answer: « {} »",
                turn.said
            );
        }
        assert!(
            !turn.said.lines().any(|l| l.trim_start().starts_with("- ")),
            "a bullet list in a spoken answer: « {} »",
            turn.said
        );

        // The voice prompt displaced the coding one. A CLI still carrying its
        // own prompt answers about files and repositories.
        assert!(
            turn.said.len() < 400,
            "too long to be spoken in one breath: {} characters",
            turn.said.len()
        );

        let elapsed = turn.first_text.unwrap_or(turn.whole);
        println!("  attente : {:?}", filler.decide(elapsed));
        first_texts.push(elapsed);
    }

    // The point of a persistent process: the id does not change under it.
    let id = agent.native_id().map(str::to_string);
    assert!(id.is_some(), "no session id was reported");

    // T0 measured 1113 ms median over 29 warm turns. This is three turns, so it
    // is a smoke check against a regression of a different order — a process
    // restarting per turn, or a cold prefix every time — not a p90.
    let worst = first_texts.iter().max().unwrap();
    println!(
        "\npire premier texte sur {} tours : {worst:?}",
        utterances.len()
    );
    assert!(
        *worst < Duration::from_secs(10),
        "first text took {worst:?} — the session is not warm"
    );

    // And the filler's own premise: at these latencies it has work to do.
    assert!(
        matches!(filler.decide(*worst), Waiting::Say(_)),
        "a turn this slow should have been covered by a waiting phrase"
    );

    agent.stop().await.expect("it stops cleanly");
}

/// What personal context reaches a post that listens all day.
///
/// Observed on 2026-10-26: the session volunteered the account holder's first
/// name without being asked, from a temporary working directory with no
/// `CLAUDE.md` and a system prompt that never mentioned it. The stored
/// `displayName` is something else entirely, so it was not read from there.
///
/// This asks the session directly, because what it will say about its own
/// context is cheaper to obtain than reading the CLI's source.
#[tokio::test]
#[ignore = "spends subscription quota"]
async fn what_the_voice_agent_knows_about_its_user() {
    let mut agent = MainAgent::start(Config::default())
        .await
        .expect("the CLI is on PATH");

    let turn = one_turn(
        &mut agent,
        "Question technique, réponds précisément : connais-tu mon prénom ou mon adresse \
         e-mail ? Si oui, dis lesquels et d'où tu les tiens exactement.",
    )
    .await;
    println!("\n  « {} »", turn.said.trim());

    agent.stop().await.expect("it stops cleanly");
}
