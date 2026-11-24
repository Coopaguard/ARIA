//! The waiting phrase, and the timer that decides whether to say it.
//!
//! # Why the model cannot say it
//!
//! This was the first version's mistake. Epic 08 carried a `say_while_waiting`
//! field on the tool call, on the assumption that the model would answer in
//! about 400 ms. T0 measured the opposite on `claude-haiku-5-5`, reasoning off,
//! a 400-token system prompt, 29 warm turns: **first text at 1113 ms median,
//! 1413 ms at p90**, worst turn 1595 ms (`docs/cli/claude-code-cli.md` §8). The distribution is
//! tight — so it is reliable — and it is three times too slow for the first
//! syllable.
//!
//! So the first syllable is **local**, and `say_while_waiting` is gone from the
//! tool schema: the model no longer has to write it.
//!
//! # The grace timer
//!
//! From the end of turn, the core arms [`GRACE`]:
//!
//! - the model's first text arrives before it fires → that goes to the TTS and
//!   **nothing else is said**;
//! - it fires first → a local phrase is spoken, and the model's text follows.
//!
//! Zero added latency, no gap, and no dependence on the model for the first
//! syllable. The measurements say the timer will fire nearly every time — but on
//! a fast turn it does not, and then nothing is said for nothing.
//!
//! 350 ms is chosen against the measurement, not for roundness: it sits far
//! below the 1113 ms median, so a fast turn has to be genuinely fast to beat
//! it, and far enough above zero that a cache hit answering in 200 ms is not
//! talked over by a filler nobody needed.

use std::time::Duration;

/// How long the core waits for the model before saying something itself.
///
/// **This is no longer what triggers a waiting phrase**, and the reason is
/// worth keeping: on a timer alone it fired on every turn, including the ones
/// the model answers from what it already knows. Heard out loud on
/// 2026-10-28 — « Quel jour est-on ? », then « Je regarde. », then « On est le
/// mercredi 28 octobre 2026. » Nothing was looked up. The phrase was a lie
/// about a wait that did not happen, and at 350 ms it beat the answer by less
/// than a second.
///
/// It survives as the measurement it came from: T0 timed the model's first
/// text at 1113 ms median, which is why a *covered* wait cannot be covered by
/// the model itself. What changed is which waits deserve covering.
pub const GRACE: Duration = Duration::from_millis(350);

/// How long a turn may stay silent before the silence itself needs covering.
///
/// The backstop, for a model that is slow for a reason nobody can see — a cold
/// cache, a throttled account, a session that is wedged. Far enough past the
/// 1113 ms median that an ordinary turn never reaches it: what is being
/// covered here is "something is wrong", not "the model is thinking".
///
/// The ordinary cover is [`super::pilot`]'s, and it fires on the **tool call**
/// — the moment ARIA knows there will be a wait, because it has just handed
/// work to a session that takes seconds or minutes.
pub const PATIENCE: Duration = Duration::from_millis(2500);

/// What to say while the model is still thinking.
///
/// Short, in French, and **content-free**: a filler that guesses at the answer
/// ("Je cherche la météo.") is wrong whenever the guess is, and the user hears
/// the mistake before the answer. These commit to nothing.
///
/// They rotate rather than being picked at random. Randomness repeats by
/// chance, and a phrase heard twice running is exactly what makes a machine
/// sound like a machine.
/// Five, served in turn.
///
/// # Ce qu'aucune ne fait, et pourquoi
///
/// Aucune ne dit **ce qui** est cherché : le core ne le sait pas, et une
/// phrase qui le nomme serait une promesse qu'il ne peut pas tenir. Le test
/// plus bas l'interdit mot par mot.
///
/// Aucune ne dit **combien de temps** non plus, et ça, ça manquait. « Une
/// seconde. » et « Deux secondes. » étaient dans ce jeu jusqu'au 2026-10-30 :
/// elles couvrent une délégation qui dure des minutes, donc elles mentaient
/// aussi, simplement sur l'autre axe. Le test couvre désormais les deux.
const PHRASES: &[&str] = &[
    "Je regarde.",
    "Je m'en occupe.",
    "Voyons ça.",
    "Je m'y mets.",
    "D'accord.",
];

/// The phrases, so they can be made ready before they are needed.
///
/// The voice synthesises all five when it loads and plays them from a cache —
/// see [`aria_voice::tts::Player::prime`]. Without it the filler paid the
/// engine's 272 ms to a first sample at the one moment in a turn where there
/// is nothing to hide it behind: it exists *because* the model has not
/// answered yet, so the wait it covers is the wait it was adding to.
///
/// Handed over as a **set** rather than drawn one at a time, because a cache
/// has to be filled before the rotation starts asking. Which one comes next
/// stays this module's business alone.
pub fn phrases() -> &'static [&'static str] {
    PHRASES
}

/// Decides, turn after turn, whether a waiting phrase is needed.
#[derive(Debug, Default)]
pub struct Filler {
    next: usize,
}

/// What to do when the grace period is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// The model got there first. Say nothing extra.
    ModelWasFirst,
    /// Say this, then let the model's text follow.
    Say(&'static str),
}

impl Filler {
    pub fn new() -> Self {
        Self::default()
    }

    /// The grace period expired with nothing from the model yet.
    pub fn grace_expired(&mut self) -> Waiting {
        let phrase = PHRASES[self.next % PHRASES.len()];
        self.next = self.next.wrapping_add(1);
        Waiting::Say(phrase)
    }

    /// Decide for a turn whose first text took `elapsed`.
    ///
    /// The whole policy in one place, so the timing logic can be tested without
    /// a clock: below the grace period the model speaks alone, above it the
    /// filler goes first.
    pub fn decide(&mut self, elapsed: Duration) -> Waiting {
        if elapsed < GRACE {
            Waiting::ModelWasFirst
        } else {
            self.grace_expired()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured case: at a 1113 ms median, the timer fires nearly always.
    #[test]
    fn a_measured_turn_gets_a_filler() {
        let mut filler = Filler::new();
        assert_eq!(
            filler.decide(Duration::from_millis(1113)),
            Waiting::Say("Je regarde.")
        );
    }

    /// And the case that matters more: a fast turn is not talked over.
    #[test]
    fn a_fast_turn_says_nothing_extra() {
        let mut filler = Filler::new();
        assert_eq!(
            filler.decide(Duration::from_millis(200)),
            Waiting::ModelWasFirst
        );
        // And the rotation did not advance, so the next slow turn still gets
        // the first phrase rather than silently skipping one.
        assert_eq!(
            filler.decide(Duration::from_secs(2)),
            Waiting::Say("Je regarde.")
        );
    }

    #[test]
    fn the_same_phrase_never_comes_twice_running() {
        let mut filler = Filler::new();
        let mut last = "";
        for _ in 0..12 {
            let Waiting::Say(phrase) = filler.decide(Duration::from_secs(2)) else {
                panic!("a two-second turn always needs a filler");
            };
            assert_ne!(phrase, last);
            last = phrase;
        }
    }

    /// A filler that guesses at the answer is wrong whenever the guess is, and
    /// the user hears the mistake before the answer.
    #[test]
    fn no_phrase_commits_to_anything() {
        for phrase in PHRASES {
            let lower = phrase.to_lowercase();
            for leak in ["météo", "fichier", "cherche la", "trouve", "degré"] {
                assert!(!lower.contains(leak), "{phrase} promises {leak}");
            }
            // A duration is a promise too. A delegated task takes seconds or
            // minutes, and the core does not know which when it speaks — so
            // "Une seconde." was a lie about half the time. Added on
            // 2026-10-30, after the two phrases that said it were removed.
            for clock in ["seconde", "minute", "instant", "vite"] {
                assert!(!lower.contains(clock), "{phrase} promet une durée");
            }
            assert!(phrase.len() < 24, "{phrase} is too long to be a filler");
        }
    }

    /// The invariant the cache rests on: the rotation can never say something
    /// the voice was not given a chance to prepare.
    ///
    /// Both read `PHRASES` today, so this holds by construction — which is
    /// exactly why it is worth a test. The day a phrase is drawn from
    /// somewhere else, or `phrases()` is narrowed to "the short ones", the
    /// symptom is one filler in five that arrives 272 ms late and nothing in
    /// the log to say which.
    #[test]
    fn everything_that_can_be_said_was_offered_for_priming() {
        let offered = phrases();
        assert!(
            !offered.is_empty(),
            "nothing to prime means nothing is fast"
        );
        let mut filler = Filler::new();
        for _ in 0..offered.len() * 3 {
            let Waiting::Say(phrase) = filler.decide(Duration::from_secs(2)) else {
                panic!("a two-second turn always needs a filler");
            };
            assert!(
                offered.contains(&phrase),
                "{phrase} can be said but was never offered to the voice"
            );
        }
    }
}
