//! The reference scenario, played end to end with a fake session.
//!
//! This is T3's exit condition, and it runs on every commit because **nothing
//! here starts a process or spends quota**: a scripted provider stands in for
//! the Claude subprocess, and the two output channels stand in for the TTS and
//! the main agent.
//!
//! What it proves is the thing the whole epic was written around: that the
//! numbers a session finds are the numbers the user hears. The scenario the
//! project was sketched from carried the fault in itself — the session finds
//! 23 °C and the main agent announces 24 °C — so the assertion below is on the
//! digits, not on the shape.

use std::sync::Arc;
use std::time::Duration;

use aria_core::Core;
use aria_core::agent::conversation::{Conversation, Told};
use aria_core::agent::delegation::FAILED;
use aria_core::agent::filler::{Filler, Waiting};
use aria_core::agent::journal::PENDING;
use aria_core::agent::tools::DelegateTask;

mod common;
use common::{Scripted, WEATHER};

async fn next_within(rx: &mut tokio::sync::mpsc::Receiver<String>, what: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("rien n'est arrivé sur le canal {what}"))
        .unwrap_or_else(|| panic!("le canal {what} s'est fermé"))
}

/// The same, for the agent's channel, which carries *why* it is being told.
async fn told_within(rx: &mut tokio::sync::mpsc::Receiver<Told>) -> Told {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("rien n'est arrivé sur le canal agent")
        .expect("le canal agent s'est fermé")
}

#[tokio::test]
async fn the_weather_scenario_says_the_numbers_the_session_found() {
    let core = Core::new();
    let (conversation, mut channels) = Conversation::new(Arc::clone(&core));
    let _watcher = conversation.watch();

    // 1. The user speaks, the end of turn is detected, and the agent is told.
    //    Told on the same channel a returned result travels on, because they
    //    are the same thing: a user message.
    conversation.heard("Quel temps fait-il demain à Lyon ?");
    let told = told_within(&mut channels.to_agent).await;
    assert_eq!(told.text(), "Quel temps fait-il demain à Lyon ?");
    assert!(
        told.answer_is_spoken(),
        "the user spoke, so ARIA answers this one out loud"
    );
    // And what actually leaves for the agent carries the date and the minute:
    // this is the only thing that tells the post what time it is, and without
    // it "ça fait combien de temps que je t'ai demandé ça" has no answer but a
    // guess. See `aria_core::agent::clock`.
    let sent = told.for_agent();
    assert!(
        sent.starts_with('[') && sent.contains("] Quel temps fait-il"),
        "{sent}"
    );

    // 2. The model is slower than the first syllable, which T0 measured at
    //    1113 ms median — so the waiting phrase is local and goes out first.
    let mut filler = Filler::new();
    let Waiting::Say(phrase) = filler.decide(Duration::from_millis(1113)) else {
        panic!("a turn this slow must be covered");
    };
    conversation.say(phrase).await;
    assert_eq!(next_within(&mut channels.speech, "parole").await, phrase);

    // 3. The model delegates. The tool answers at once; the result is not in it.
    let answered = conversation
        .delegate(
            &Scripted {
                answer: WEATHER,
                fail: false,
            },
            DelegateTask {
                task: "chercher la météo de demain à Lyon".into(),
                context: None,
                model: None,
                project: None,
                follows: None,
            },
        )
        .await
        .expect("the session launches");
    // The id first, so the model can send a follow-up to this very session.
    assert_eq!(answered, format!("Tâche t1. {PENDING}"));
    assert_eq!(conversation.outstanding(), 1);

    // 4. The session finishes; the watcher turns task.done into speech.
    let spoken = next_within(&mut channels.speech, "parole").await;

    // The whole point of the epic, asserted on the digits.
    assert_eq!(
        spoken,
        "Il fera vingt-trois degrés ce matin et vingt-sept degrés cet après-midi."
    );
    // And none of the Markdown around it reached the voice. Not the hyphen:
    // "vingt-trois" and "après-midi" carry one, it is ordinary French and a TTS
    // reads it correctly. What is noise is a hyphen used as a **bullet**, which
    // is a line that starts with one.
    for noise in ['*', '#', '`'] {
        assert!(!spoken.contains(noise), "{noise} dans une phrase parlée");
    }
    assert!(!spoken.starts_with('-'), "une puce dans une phrase parlée");
    assert!(!spoken.contains("23"), "un chiffre non dit : « {spoken} »");

    // 5. The main agent is told the task closed, so it can be asked about it.
    //    `Returned`, not `Said`: the line has already been spoken verbatim, so
    //    whatever the agent replies is context and not speech. Saying it too
    //    was the sentence heard twice on 2026-10-27, 830 ms apart.
    let told = told_within(&mut channels.to_agent).await;
    assert!(
        !told.answer_is_spoken(),
        "the result was already said; a second telling is the machine repeating itself"
    );
    assert!(told.text().contains("chercher la météo de demain à Lyon"));
    assert!(told.text().contains("vingt-trois"));

    assert_eq!(conversation.outstanding(), 0);
}

/// The failure that would otherwise be silence, after a waiting phrase has
/// already been spoken aloud.
#[tokio::test]
async fn a_failed_delegation_is_admitted_out_loud() {
    let core = Core::new();
    let (conversation, mut channels) = Conversation::new(Arc::clone(&core));
    let _watcher = conversation.watch();

    conversation.heard("Range mes factures.");
    let told = told_within(&mut channels.to_agent).await;
    assert_eq!(told.text(), "Range mes factures.");
    assert!(told.answer_is_spoken());
    conversation
        .delegate(
            &Scripted {
                answer: "",
                fail: true,
            },
            DelegateTask {
                task: "ranger les factures".into(),
                context: None,
                model: None,
                project: None,
                follows: None,
            },
        )
        .await
        .expect("the session launches");

    assert_eq!(next_within(&mut channels.speech, "parole").await, FAILED);
    let told = told_within(&mut channels.to_agent).await;
    assert!(told.text().contains("le processus a disparu"));
    assert!(!told.answer_is_spoken(), "the admission was already spoken");
    assert_eq!(conversation.outstanding(), 0);
}

/// A session the user started from the interface answers to the screen, not to
/// the room.
#[tokio::test]
async fn a_session_nobody_delegated_to_stays_silent() {
    let core = Core::new();
    let (conversation, mut channels) = Conversation::new(Arc::clone(&core));
    let _watcher = conversation.watch();

    let theirs = core.open_session("lancée à la main", None);
    core.finish_session(theirs, "À DIRE : coucou");

    assert!(
        tokio::time::timeout(Duration::from_millis(300), channels.speech.recv())
            .await
            .is_err(),
        "ARIA a parlé pour une session qui ne lui appartenait pas"
    );
}
