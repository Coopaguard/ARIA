//! The wall clock, and the one way a time is written for the model.
//!
//! # Why the main agent is told the time at all
//!
//! Asked for by the user on 2026-11-12, from a scenario nothing in the chain
//! could answer: fifteen minutes after a delegation, « ça fait combien de temps
//! que je t'ai demandé l'heure ? ».
//!
//! The result itself was never the problem — [`super::journal`] has always
//! pushed it back into the agent as a user message, so the context held it. What
//! the context did not hold was **when** any of it happened, and the post has no
//! clock of its own: `--system-prompt` replaces the harness prompt that would
//! have carried a date, and [`crate::actions`] — the short path, where a
//! `what_time_is_it` would live — is nine lines of documentation and no
//! executor.
//!
//! So the agent could be asked about the task and answer, and could not be asked
//! *when* and answer anything but a guess. Out loud, a guessed duration is
//! indistinguishable from a known one.
//!
//! # Why the stamp rides on the message and never in the system prompt
//!
//! Because the system prompt is written once and then **frozen**.
//! `--system-prompt-snapshot` defaults to `on`: the CLI records the prompt on a
//! conversation's first request and replays it verbatim for ever after
//! (`docs/cli/claude-code-cli.md`, relevé le 2026-11-12 sur la 2.1.312). The main agent is one
//! conversation that lives as long as the application — days — so a date put
//! there would be the date ARIA started, announced with confidence for the rest
//! of the week. That is worse than not knowing.
//!
//! The message is the only thing that is new on every turn. So the stamp goes
//! there, on **every** message without exception, which is also what makes a
//! duration computable rather than merely known: the model subtracts two stamps
//! it can both see, instead of being told one and asked to remember the other.
//!
//! # Where a stamp is added, and nowhere else
//!
//! Exactly two places, and both are the moment a message is *rendered for the
//! model*: [`super::conversation::Told::for_agent`] on the wire, and
//! [`super::journal::Journal::render`] for the day a local model holds the
//! conversation. Everything upstream carries a [`Stamp`] and no formatting, so
//! a message cannot pick one up twice on its way out.

use chrono::{DateTime, Local};

/// When something happened, on the user's own wall clock.
///
/// Local and not UTC, because this is read out loud. An assistant that answers
/// "il est treize heures trente-deux" to someone whose kitchen clock says
/// fourteen thirty-two is wrong in the only way that matters.
pub type Stamp = DateTime<Local>;

/// Now.
pub fn now() -> Stamp {
    Local::now()
}

/// The stamp as the model reads it: `[2026-11-12 14:32]`.
///
/// Eighteen characters, and the shape is three decisions:
///
/// - **the date is there**, because ARIA runs for days and a bare `14:32` makes
///   a question asked last night indistinguishable from one asked this
///   afternoon;
/// - **ISO and not French**, because the model does arithmetic on it. A French
///   weekday would also need chrono's `unstable-locales`, and the weekday is a
///   separate question — see the open item in the decision log;
/// - **no seconds.** Three more characters on every message, in a context the
///   CLI replays whole on every turn, to buy a precision that is never said out
///   loud: a duration is spoken in minutes or in hours.
pub fn stamp(at: Stamp) -> String {
    at.format("[%Y-%m-%d %H:%M]").to_string()
}

/// A message as the main agent receives it.
pub fn stamped(at: Stamp, text: &str) -> String {
    format!("{} {text}", stamp(at))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A fixed local time, so a test can place two messages fifteen minutes
    /// apart without waiting fifteen minutes.
    pub(crate) fn at(hour: u32, minute: u32) -> Stamp {
        Local
            .with_ymd_and_hms(2026, 11, 12, hour, minute, 0)
            .single()
            .expect("2026-11-12 is not a daylight-saving fold anywhere")
    }

    #[test]
    fn a_stamp_carries_the_date_and_the_minute() {
        assert_eq!(stamp(at(14, 32)), "[2026-11-12 14:32]");
    }

    /// The model is asked to subtract these, so the hour must be the
    /// twenty-four hour one and zero-padded — `2:05` and `14:05` sort and
    /// subtract differently.
    #[test]
    fn the_hour_is_padded_and_on_twenty_four() {
        assert_eq!(stamp(at(2, 5)), "[2026-11-12 02:05]");
    }

    #[test]
    fn a_message_keeps_its_text_after_the_stamp() {
        assert_eq!(
            stamped(at(14, 32), "Quelle heure est-il ?"),
            "[2026-11-12 14:32] Quelle heure est-il ?"
        );
    }

    /// The whole budget argument in one assertion: this rides on every message
    /// of a context that is replayed in full on every turn.
    #[test]
    fn the_stamp_stays_cheap() {
        assert_eq!(stamp(now()).len(), 18);
    }
}
