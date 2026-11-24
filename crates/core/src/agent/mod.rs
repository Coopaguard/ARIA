//! The main voice agent: the one that holds the conversation.
//!
//! Not a delegated session. Those are epics 01 to 05 and live under
//! [`crate::providers`]; this is the post that listens all day, answers
//! directly when it can, and hands work over when it cannot.
//!
//! ```text
//!        transcript
//!            │
//!            ▼
//!   MAIN AGENT — a persistent CLI session, `claude-haiku-5-5`
//!            │
//!    ┌───────┼────────────────────┐
//!    ▼       ▼                    ▼
//!  answer  run_action      delegate_task
//!    │     (short path)           │
//!    │                            ▼
//!    │                   a session (epics 01–05)
//!    ▼                            │
//!   TTS ◄──── spoken, verbatim ◄──┘
//! ```
//!
//! # The rule that shapes everything here
//!
//! **The main agent is never billed per token** (decision of 2026-10-24). It
//! runs on the machine, or it goes through the CLI that is already installed.
//! A machine that listens all day and answers every sentence cannot be a meter
//! running: at that price one may as well buy a turnkey voice service, which
//! this project ruled out. The Messages API is therefore **out of scope for
//! this post** — it stays the road to remote routing services, not to the turn
//! of speech.
//!
//! Tier A, the persistent CLI session, is the only one implemented. Tier B —
//! Bolt E4B behind a local sidecar — is deferred to the "choice of model" work,
//! and the seam is here so that it can arrive without moving anything else.
//!
//! # Three mechanisms, each written because measuring said so
//!
//! | | Where | Why it is not the obvious design |
//! |---|---|---|
//! | the waiting phrase is **local** | [`filler`] | the model's first text lands at 1113 ms median — three times too slow for a first syllable |
//! | a late result re-enters as a **user** message | [`journal`] | an orphan tool message fails the E4B template outright, and is a guess on any other |
//! | the agent **repeats**, never paraphrases | below | a 4B model that paraphrases a payload corrupts it — above all numbers, above all aloud |
//!
//! # Repeating rather than paraphrasing
//!
//! The scenario the project was sketched from carried the fault in itself: the
//! session finds **23 °C** and the main agent announces **24 °C**. So the
//! session returns a `spoken` field, ready to say, and the core pushes it
//! **straight to the TTS** before writing it into the history as an assistant
//! turn that has already been said.
//!
//! No generation, so no drift, no tokens and no latency. [`speak_verbatim`]
//! is the whole of it, and it is one line because that is the point: anything
//! longer would be a chance to rephrase something.

pub mod clock;
pub mod conversation;
pub mod delegation;
pub mod filler;
pub mod instructions;
pub mod journal;
pub mod local;
pub mod marks;
pub mod pilot;
pub mod sentences;
pub mod session;
pub mod skills;
pub mod tools;

/// What the core says out loud for a returned result.
///
/// The session's own wording, optionally after a lead-in that the **core**
/// chooses — never the model. A lead-in is a nicety; regenerating the sentence
/// to add one would risk the numbers inside it.
pub fn speak_verbatim(spoken: &str, lead_in: Option<&str>) -> String {
    match lead_in {
        Some(lead) if !lead.is_empty() => format!("{lead} {spoken}"),
        _ => spoken.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect the reference scenario carried: 23 becomes 24.
    #[test]
    fn the_numbers_come_through_untouched() {
        let from_session = "Il fera 23 °C ce matin et 27 °C cet après-midi.";
        assert_eq!(speak_verbatim(from_session, None), from_session);
        assert!(speak_verbatim(from_session, Some("Alors —")).contains("23 °C"));
        assert!(speak_verbatim(from_session, Some("Alors —")).starts_with("Alors —"));
    }

    #[test]
    fn an_empty_lead_in_adds_nothing() {
        assert_eq!(
            speak_verbatim("Trois factures.", Some("")),
            "Trois factures."
        );
    }
}
