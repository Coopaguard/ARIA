//! Handing the answer to the voice **as it is written**, a sentence at a time.
//!
//! # The wait this removes
//!
//! ARIA used to speak at `Speech::Finished`, with the whole answer. So the
//! window filled with text, word by word, and nothing came out of the
//! loudspeaker until the model had finished writing — seconds, on an answer of
//! any length. What the user sees is their question answered in writing while
//! ARIA stays silent, and that reads as a slow assistant no matter how fast
//! the first syllable then arrives.
//!
//! Epic 08 calls the shape that fixes it the
//! **duplex cascade**: a TTS that speaks before the text is finished. The one
//! piece missing was a TTS that could be fed in pieces, and decision 56 is
//! what provided it — both local engines stream, and the player's queue is
//! appended to rather than replaced, so consecutive sentences play as one
//! stretch of speech.
//!
//! # Why this is not just "split on a full stop"
//!
//! Two constraints pull in opposite directions, and the batching here is the
//! compromise between them.
//!
//! **Start early.** The first piece should go the instant one complete
//! sentence exists, because that delay is the whole symptom.
//!
//! **Then stop chopping.** Every hand-off costs a fixed synthesis setup, and
//! the generation that follows has its own speed. Measured on this machine on
//! 2026-10-29, `cargo test -p aria-voice --lib speed -- --ignored`: **272 ms**
//! to the first sample, then **1.66×** real time. A chunk worth *n* seconds of
//! audio therefore costs about `0.27 + 0.6n` to produce, which stays ahead of
//! playback as soon as *n* is over roughly 0.7 s — and with the ear running
//! alongside, which cost about a fifth of the throughput when it was measured
//! inside the application, over about one second. Split finer than that and
//! the voice falls behind what it is reading, which is audible as gaps in the
//! middle of sentences — worse than the wait it was meant to remove.
//!
//! **That ratio is a build setting, not a property of the model.** The same
//! engine ran at 0.49× before `-C target-cpu=x86-64-v3` reached `xn`'s SIMD
//! kernels — below 1.0, where no chunking works at all because the queue
//! drains faster than it fills whatever the pieces are. Decision 63 carries
//! the measurement; if that flag ever goes, this file's arithmetic goes with
//! it.
//!
//! Hence [`FIRST_BREATH`] and [`LATER_BATCH`]: the first complete sentence
//! leaves immediately, and after that the pieces are accumulated until they
//! are worth a call.
//!
//! # What it refuses to do
//!
//! It never speaks text twice. `Speech::Finished` carries the CLI's own
//! `result`, which is authoritative but **not** guaranteed to be
//! byte-identical to the concatenated deltas, so [`Pending::rest_of`] matches
//! what was already said against it ignoring whitespace and hands back only
//! the tail. When that match fails — the model's final text is not a
//! continuation of what ARIA has been saying — it hands back nothing. A
//! slightly truncated answer is recoverable by asking again; the whole answer
//! repeated out loud over the top of itself is not.

/// The first piece goes as soon as a sentence ends, however short.
///
/// One character, i.e. no threshold at all, and that is deliberate: « Oui. »
/// is a complete answer and must not wait for company.
const FIRST_BREATH: usize = 1;

/// After the first, pieces are gathered until they are worth a synthesis call.
///
/// Eighty characters is around five seconds of speech, which takes a little
/// over three to produce — a margin wide enough to survive the ear running at
/// the same time. Deliberately not tighter: fewer, longer calls pay the 272 ms
/// setup fewer times, and the text itself arrives from the CLI far faster than
/// it can be spoken, so a larger batch costs nothing in latency after the
/// first piece.
///
const LATER_BATCH: usize = 80;

/// What the agent writes when a turn was not for it — decision 119.
///
/// In a wake by the name, everything heard goes to the agent, and it has to be
/// able to **not answer** the people talking among themselves. The prompt asks
/// for this exact mark; [`is_silence`] is what makes it one, and is wider on
/// purpose — `...`, `…`, an empty line all mean the same, and none of them is
/// worth a syllable.
pub const SILENCE: &str = "…";

/// Whether a piece of the answer is nothing to say: no letter, no digit.
///
/// Wider than [`SILENCE`] because a model asked for « … » writes « ... » often
/// enough, and because a phrase with nothing pronounceable in it would come out
/// of the loudspeaker as a breath at best.
pub fn is_silence(text: &str) -> bool {
    !text.chars().any(char::is_alphanumeric)
}

/// One turn's answer, as it arrives.
#[derive(Default, Debug)]
pub struct Pending {
    /// Everything the deltas have carried this turn.
    said: String,
    /// How much of `said` has gone to the voice, in bytes.
    handed: usize,
    /// Whether this turn has stopped being worth listening to.
    ///
    /// See [`Pending::mute`].
    muted: bool,
}

impl Pending {
    /// Stop speaking this turn, and drop what has not gone out.
    ///
    /// # The stutter this exists to stop
    ///
    /// The main agent's prompt says it, in capitals: call `delegate_task`,
    /// **then say nothing** — a waiting phrase has already been spoken, and a
    /// second announcement sounds like a stammer. Models mostly obey; on
    /// 2026-10-30 one did not, and the result was heard out loud:
    ///
    /// > — Je regarde.
    /// > — Je recherche en ligne, le résultat arrive dans quelques secondes.
    ///
    /// Two machines announcing the same departure. That text was never audible
    /// before this module existed, because only the turn's **final** text was
    /// spoken; speaking the deltas made the chatter reachable, so the module
    /// that made it reachable is the one that has to stop it.
    ///
    /// A prompt is a request. This is the mechanism — and the right place for
    /// it, because the rule is about **what ARIA says**, not about what the
    /// model is allowed to think.
    ///
    /// Only a *delegating* call mutes. `run_action` will one day answer in its
    /// own result, and silencing the turn after it would throw away the answer
    /// rather than a stutter.
    pub fn mute(&mut self) {
        self.muted = true;
        self.handed = self.said.len();
    }

    /// Take in a delta; give back whatever is now worth speaking.
    pub fn push(&mut self, delta: &str) -> Option<String> {
        if self.muted {
            // Still accumulated: the journal gets the whole answer, the
            // loudspeaker does not.
            self.said.push_str(delta);
            self.handed = self.said.len();
            return None;
        }
        self.said.push_str(delta);
        let fresh = &self.said[self.handed..];
        let end = last_break(fresh)?;
        let want = if self.handed == 0 {
            FIRST_BREATH
        } else {
            LATER_BATCH
        };
        if end < want {
            return None;
        }
        let piece = fresh[..end].trim().to_string();
        self.handed += end;
        if piece.is_empty() { None } else { Some(piece) }
    }

    /// At the end of the turn: what is left to say of `whole`.
    ///
    /// `None` means *say nothing* — see the module header on why that is the
    /// safe side of this decision.
    pub fn rest_of(&self, whole: &str) -> Option<String> {
        if self.muted {
            // The turn handed work to a session; its last words are the same
            // announcement, arriving once more.
            return None;
        }
        if self.handed == 0 {
            return Some(whole.trim().to_string());
        }
        let at = after_prefix(whole, &self.said[..self.handed])?;
        Some(whole[at..].trim().to_string())
    }

    pub fn reset(&mut self) {
        self.said.clear();
        self.handed = 0;
        self.muted = false;
    }
}

/// The last offset in `text` where a sentence safely ends.
///
/// Safely means: a terminator, then any closing marks, then **whitespace**.
/// Requiring the space is what keeps `3.5` and `fichier.rs` in one piece
/// without a single special case for either.
fn last_break(text: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut found = None;
    let mut i = 0;
    while i < chars.len() {
        let (at, c) = chars[i];
        if !matches!(c, '.' | '!' | '?' | '…') {
            i += 1;
            continue;
        }
        // A run, so that `?!` and `. »` end one sentence and not three.
        let mut j = i;
        while let Some(&(_, next)) = chars.get(j + 1) {
            if matches!(next, '.' | '!' | '?' | '…' | '"' | '\'' | '»' | ')' | ']') {
                j += 1;
            } else {
                break;
            }
        }
        let (last_at, last) = chars[j];
        let end = last_at + last.len_utf8();
        let followed_by_space = chars
            .get(j + 1)
            .is_some_and(|(_, next)| next.is_whitespace());
        if followed_by_space && !ends_in_abbreviation(&text[..at]) {
            found = Some(end);
        }
        i = j + 1;
    }
    found
}

/// Abbreviations that end in a full stop and are not the end of anything.
///
/// French, because that is what ARIA speaks, and short on purpose: every entry
/// is a word that *does* appear in spoken answers. A missing one costs a pause
/// in the wrong place, not a wrong word, so the list is allowed to be
/// incomplete.
const ABBREVIATIONS: [&str; 16] = [
    "m", "mme", "mlle", "dr", "pr", "etc", "cf", "p", "ex", "av", "ap", "st", "ste", "no", "art",
    "env",
];

fn ends_in_abbreviation(before: &str) -> bool {
    let word: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if word.is_empty() {
        return false;
    }
    // A lone capital is an initial: "J. Dupont", never the end of a sentence.
    if word.chars().count() == 1 && word.chars().all(char::is_uppercase) {
        return true;
    }
    ABBREVIATIONS.contains(&word.to_lowercase().as_str())
}

/// Where `whole` continues after the text of `spoken`, ignoring whitespace.
///
/// Whitespace-insensitive because the CLI's final `result` and the deltas it
/// streamed are the same words and not always the same spacing, and a missing
/// newline must not cost the user the end of their answer.
fn after_prefix(whole: &str, spoken: &str) -> Option<usize> {
    let mut chars = whole.char_indices().peekable();
    for wanted in spoken.chars().filter(|c| !c.is_whitespace()) {
        loop {
            let (_, c) = *chars.peek()?;
            chars.next();
            if c.is_whitespace() {
                continue;
            }
            if c != wanted {
                return None;
            }
            break;
        }
    }
    Some(chars.peek().map_or(whole.len(), |(at, _)| *at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_sentence_leaves_as_soon_as_it_is_whole() {
        let mut pending = Pending::default();
        assert_eq!(pending.push("Il fera vingt-trois"), None);
        assert_eq!(
            pending.push(" degrés ce matin."),
            None,
            "no space after yet"
        );
        assert_eq!(
            pending.push(" Le vent"),
            Some("Il fera vingt-trois degrés ce matin.".to_string())
        );
    }

    /// « Oui. » is an answer. It must not wait for a second sentence.
    #[test]
    fn a_one_word_answer_is_not_held_back() {
        let mut pending = Pending::default();
        assert_eq!(pending.push("Oui. "), Some("Oui.".to_string()));
    }

    /// The other half of the compromise: after the first piece, chopping finely
    /// would make the voice fall behind what it is reading.
    #[test]
    fn later_pieces_are_gathered_until_they_are_worth_a_call() {
        let mut pending = Pending::default();
        pending.push("Oui. ").expect("the first breath");
        assert_eq!(
            pending.push("Bien. Très bien. "),
            None,
            "two short sentences are not yet worth a synthesis call"
        );
        let long = "J'ai regardé le calendrier et il n'y a rien de prévu avant jeudi prochain. ";
        assert_eq!(
            pending.push(long).as_deref(),
            Some(format!("Bien. Très bien. {}", long.trim()).as_str())
        );
    }

    #[test]
    fn a_decimal_is_not_a_sentence() {
        assert_eq!(last_break("Il fera 23.5 degrés"), None);
        assert_eq!(last_break("Ouvre le fichier main.rs stp"), None);
    }

    #[test]
    fn an_abbreviation_is_not_a_sentence_either() {
        assert_eq!(last_break("Demande à M. Dupont ce"), None);
        assert_eq!(last_break("des pommes, des poires, etc. et puis"), None);
        // But the same word ending a real sentence still breaks.
        assert!(last_break("Il fera beau. Et demain").is_some());
    }

    #[test]
    fn a_closing_quote_belongs_to_the_sentence_it_closes() {
        let text = "Il a dit « bonjour ». Puis il";
        let end = last_break(text).expect("a break");
        assert_eq!(&text[..end], "Il a dit « bonjour ».");
    }

    #[test]
    fn what_was_said_is_never_said_twice() {
        let mut pending = Pending::default();
        pending.push("Oui. ").expect("the first breath");
        assert_eq!(
            pending.rest_of("Oui. Il fera beau."),
            Some("Il fera beau.".to_string())
        );
    }

    /// The CLI's `result` is authoritative but not byte-identical to the
    /// deltas: a newline where there was a space must not cost the tail.
    #[test]
    fn a_difference_in_spacing_does_not_lose_the_tail() {
        let mut pending = Pending::default();
        pending.push("Oui. ").expect("the first breath");
        assert_eq!(
            pending.rest_of("Oui.\n\nIl fera beau."),
            Some("Il fera beau.".to_string())
        );
    }

    /// The case that must stay quiet rather than repeat itself out loud.
    #[test]
    fn an_answer_that_is_not_a_continuation_is_not_spoken_again() {
        let mut pending = Pending::default();
        pending.push("Oui. ").expect("the first breath");
        assert_eq!(pending.rest_of("Non, finalement."), None);
    }

    /// The defect reported on 2026-10-30: two announcements for one departure.
    #[test]
    fn nothing_more_is_said_once_the_work_has_been_handed_off() {
        let mut pending = Pending::default();
        pending.mute();
        assert_eq!(
            pending.push("Je recherche en ligne, le résultat arrive. "),
            None,
            "la phrase d'attente a déjà été prononcée"
        );
        assert_eq!(
            pending.rest_of("Je recherche en ligne, le résultat arrive."),
            None,
            "ni dans les deltas, ni à la fin du tour"
        );
    }

    /// What was already out stays out — nothing can be unspoken — but the tail
    /// is dropped rather than finished.
    #[test]
    fn muting_mid_answer_drops_the_tail_and_not_what_was_said() {
        let mut pending = Pending::default();
        assert_eq!(pending.push("Oui. "), Some("Oui.".to_string()));
        pending.mute();
        assert_eq!(pending.push("Et je cherche le reste. "), None);
        assert_eq!(pending.rest_of("Oui. Et je cherche le reste."), None);
    }

    /// One turn is muted; the next one is not.
    #[test]
    fn the_next_turn_starts_able_to_speak() {
        let mut pending = Pending::default();
        pending.mute();
        pending.reset();
        assert_eq!(pending.push("Oui. "), Some("Oui.".to_string()));
    }

    #[test]
    fn a_turn_that_said_nothing_yet_speaks_the_whole_answer() {
        let pending = Pending::default();
        assert_eq!(
            pending.rest_of("  Il fera beau.  "),
            Some("Il fera beau.".to_string())
        );
    }
}

#[cfg(test)]
mod silence {
    use super::*;

    /// Decision 119: the mark the prompt asks for, and the spellings a model
    /// writes instead, are all nothing to say — and a real answer is not.
    #[test]
    fn a_mark_with_nothing_pronounceable_is_silence() {
        for quiet in [SILENCE, "...", " … ", "", "—"] {
            assert!(is_silence(quiet), "« {quiet} » devrait se taire");
        }
        for said in ["Oui.", "Il est 14 h.", "… bon, d'accord."] {
            assert!(!is_silence(said), "« {said} » devrait se dire");
        }
    }
}
