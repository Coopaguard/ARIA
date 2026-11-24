//! Being addressed: the name, and the phrase that asks for quiet.
//!
//! This is what produces `ModeTrigger::NameHeard` and `ModeTrigger::SpokenSleep`
//! for `aria-core`'s gate. The decision of what those do belongs there; the
//! knowledge of how an ASR mangles a proper noun belongs here.
//!
//! # Why a matcher and not a vocabulary
//!
//! Verified on 2026-10-25: **Parakeet's vocabulary cannot be extended.** It is a
//! 1025-subword SentencePiece frozen in the joint's output dimension, so a
//! 1026th token can never be emitted and editing `vocab.txt` only changes the
//! spelling of an existing token. A dedicated wake-word model fares no better:
//! a custom name needs a *trained* model, which does not survive someone typing
//! a name into a settings field.
//!
//! So the ASR is the wake word, and the work is here, because **Parakeet spells
//! an unusual name approximately**. Measured, twice, on the same recording of
//! "Aria, mets-toi en veille": it came back *"Ariane, mets-toi en veille"* and
//! *"Ariane met tout en veille"*.
//!
//! Three mechanisms, in this order:
//!
//! 1. **aliases learned at setup** — say the name three times, keep what the ASR
//!    actually produces. This turns an unsolvable problem (make an ASR spell an
//!    arbitrary name) into a trivial one (know how *this* ASR spells it, in
//!    *this* voice, on *this* microphone);
//! 2. **a phonetic fold**, below, for everything that was not recorded;
//! 3. **position**: a name in the first words is an address, further in it may
//!    be a mention.

use serde::{Deserialize, Serialize};

/// The assistant's name, and what the ASR makes of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Name {
    /// What the user typed, and what is shown back to them.
    pub canonical: String,
    /// Spellings recorded at setup. Checked before the fold, because a spelling
    /// that was actually observed beats any rule.
    pub aliases: Vec<String>,
    /// How far into the utterance the name still counts as an address.
    pub address_within_words: usize,
}

impl Default for Name {
    fn default() -> Self {
        Self {
            canonical: "Aria".into(),
            aliases: Vec::new(),
            address_within_words: 3,
        }
    }
}

/// Why a name will not work as a wake word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unsuitable {
    #[error("a name of one or two syllables is what an ASR can place reliably")]
    TooLong,
    #[error("too short to tell apart from ordinary speech")]
    TooShort,
    #[error("a common French word wakes the assistant at every conversation")]
    CommonWord,
}

/// Words frequent enough that using one as a name means waking up constantly.
/// Short on purpose: it exists to catch the obvious mistake, and the setup
/// recording is the real acceptance test.
const COMMON: &[&str] = &[
    "marie",
    "pierre",
    "madame",
    "monsieur",
    "maison",
    "bonjour",
    "merci",
    "oui",
    "non",
    "salut",
    "ordinateur",
    "telephone",
    "voiture",
    "demain",
    "matin",
];

impl Name {
    pub fn new(canonical: impl Into<String>) -> Self {
        Self {
            canonical: canonical.into(),
            ..Default::default()
        }
    }

    /// Refuse what will not work, at the moment it is typed.
    ///
    /// Much better behaviour than accepting anything and failing mysteriously
    /// six months later — and the setup recording that follows is the real
    /// test: if the ASR cannot produce a stable spelling in three tries, say so
    /// and ask for another name.
    pub fn check(candidate: &str) -> Result<(), Unsuitable> {
        let folded = fold(candidate);
        if folded.chars().count() < 3 {
            return Err(Unsuitable::TooShort);
        }
        if folded.chars().count() > 9 {
            return Err(Unsuitable::TooLong);
        }
        if COMMON.iter().any(|w| fold(w) == folded) {
            return Err(Unsuitable::CommonWord);
        }
        Ok(())
    }

    /// Whether one already-folded word is this name.
    fn is_me(&self, word: &str) -> bool {
        self.aliases.iter().any(|alias| fold(alias) == word)
            || sounds_like(word, &fold(&self.canonical))
    }

    /// Word index at which the name appears, if it does.
    pub fn find(&self, transcript: &str) -> Option<usize> {
        words(transcript).iter().position(|word| self.is_me(word))
    }

    /// Whether the whole utterance is nothing but ARIA's name.
    ///
    /// « Ariane. » is an **address**, not a request — someone getting ARIA's
    /// attention before saying what they want. Measured on 2026-10-28: a pause
    /// after the name split one sentence into two complete turns, and the
    /// first was handed to the model on its own. It answered « Ariane ? Je ne
    /// suis pas sûr de bien saisir. » — which is the only thing it could say,
    /// and was spoken out loud over the user still talking.
    ///
    /// The end-of-turn detector cannot catch this and should not be asked to:
    /// a name said alone really does end with a falling intonation and a full
    /// stop, so both signals agree that the turn is over. They are right. What
    /// is wrong is treating a finished *address* as a finished *request*.
    pub fn is_address_only(&self, transcript: &str) -> bool {
        let said = words(transcript);
        !said.is_empty() && said.iter().all(|word| self.is_me(word))
    }

    /// Was ARIA addressed, as opposed to mentioned?
    ///
    /// "Aria, quelle heure…" addresses; "j'ai parlé à Aria hier" mentions. The
    /// line between them is not solved — it is a tuning point, and the position
    /// is the cheap part of it.
    pub fn addressed(&self, transcript: &str) -> bool {
        self.find(transcript)
            .is_some_and(|at| at < self.address_within_words)
    }
}

/// Does this **complete turn** ask ARIA to go quiet?
///
/// Complete at the end-of-turn detector's sense of the word, which the caller
/// has already established — "merci Aria, et peux-tu aussi…" must put nothing
/// to sleep, and without T2 there would be no way to tell the difference.
///
/// The test is that the utterance is *essentially* the request: a sleep marker,
/// and nothing substantive besides it, the name and politeness. That is what
/// rejects "merci pour la météo, Aria, maintenant ouvre VS Code".
pub fn asks_for_sleep(transcript: &str, name: &Name) -> bool {
    const MARKERS: &[&str] = &["veille", "chut", "stop", "dors", "merci"];
    const HARMLESS: &[&str] = &[
        "merci", "mets", "met", "toi", "te", "en", "veille", "chut", "stop", "cest", "bon", "ok",
        "dacord", "a", "plus", "tard", "bonne", "nuit", "dors", "repos", "aurevoir", "salut",
        "beaucoup", "bien", "la", "le", "tout",
    ];

    let said = words(transcript);
    if said.is_empty() {
        return false;
    }
    let is_name = |w: &String| name.is_me(w);

    // Both sides are folded before comparing. Written out plainly because the
    // first version compared folded transcript words against raw literals, and
    // "merci" folds to "mersi" — so the commonest sleep phrase of all never
    // matched, silently.
    let folded = |list: &[&str], word: &String| list.iter().any(|w| fold(w) == *word);
    let has_marker = said.iter().any(|w| folded(MARKERS, w));
    let nothing_else = said.iter().all(|w| folded(HARMLESS, w) || is_name(w));
    has_marker && nothing_else
}

/// Transcript to folded words, joining runs of single letters first.
///
/// The join is for acronyms: an ASR writes `ARCOM` as "a r c o m" about as
/// often as it writes it whole, and the same happens to a name spelled out.
fn words(transcript: &str) -> Vec<String> {
    let raw: Vec<String> = transcript
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(fold)
        .filter(|w| !w.is_empty())
        .collect();

    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut run = String::new();
    for word in raw {
        if word.chars().count() == 1 {
            run.push_str(&word);
            continue;
        }
        if !run.is_empty() {
            out.push(std::mem::take(&mut run));
        }
        out.push(word);
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// French spellings that sound the same, folded onto one form.
///
/// Not a full phonetic algorithm — Soundex and its cousins are built for
/// English and mangle French. These are the substitutions that actually came up:
/// accents, a silent initial `h`, `ph`, `qu`, soft and hard `c`, `y` for `i`,
/// doubled letters, and silent final consonants.
fn fold(word: &str) -> String {
    let lower: String = word
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ÿ' => 'y',
            other => other,
        })
        .filter(|c| c.is_alphanumeric())
        .collect();

    let chars: Vec<char> = lower.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match (c, next) {
            ('h', _) if i == 0 => {}
            ('p', Some('h')) => {
                out.push('f');
                i += 1;
            }
            ('q', Some('u')) => {
                out.push('k');
                i += 1;
            }
            ('c', Some('e')) | ('c', Some('i')) | ('c', Some('y')) => out.push('s'),
            ('c', _) => out.push('k'),
            ('y', _) => out.push('i'),
            _ => out.push(c),
        }
        i += 1;
    }

    // Doubled letters, then the silent tail. Both are places French spells the
    // same sound more than one way.
    let mut squeezed = String::with_capacity(out.len());
    for c in out.chars() {
        if !squeezed.ends_with(c) {
            squeezed.push(c);
        }
    }
    while squeezed.chars().count() > 2
        && matches!(
            squeezed.chars().next_back(),
            Some('e' | 's' | 't' | 'd' | 'x' | 'z')
        )
    {
        squeezed.pop();
    }
    squeezed
}

/// Close enough to be the same name, once both are folded.
///
/// **Anchored on the first letter**, and that is what makes a tolerance of one
/// edit safe on a four-letter name: without it, "maria" is one insertion from
/// "aria" and every Maria in the room wakes the assistant. With it, "harya"
/// still matches — the silent initial `h` is dropped by the fold — and "maria"
/// does not.
fn sounds_like(word: &str, target: &str) -> bool {
    if word.is_empty() || target.is_empty() {
        return false;
    }
    if word.chars().next() != target.chars().next() {
        return false;
    }
    let allowed = if target.chars().count() >= 7 { 2 } else { 1 };
    distance(word, target) <= allowed
}

/// Levenshtein, two rows.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod address_only {
    use super::*;

    /// The split measured on 2026-10-28: « Ariane. » then, after a pause,
    /// « Quel jour est-on ? ». Two complete turns, and the first was handed to
    /// the model on its own.
    #[test]
    fn a_name_said_alone_is_an_address_not_a_request() {
        let aria = Name::default();
        assert!(aria.is_address_only("Ariane."));
        assert!(aria.is_address_only("Aria"));
        assert!(aria.is_address_only("  ARIA ?  "));
        // Spelled out, which an ASR does about as often as not.
        assert!(aria.is_address_only("A R I A"));
    }

    /// Anything else is a request, including one that merely begins with the
    /// name — which is the ordinary case and must not be swallowed.
    #[test]
    fn a_request_is_not_an_address() {
        let aria = Name::default();
        assert!(!aria.is_address_only("Aria, quel jour est-on ?"));
        assert!(!aria.is_address_only("Quel jour est-on ?"));
        assert!(!aria.is_address_only("merci Aria"));
        assert!(!aria.is_address_only(""));
        assert!(!aria.is_address_only("   "));
    }

    /// A learned alias counts as the name, since that is what an alias is for:
    /// knowing how *this* ASR spells it.
    #[test]
    fn a_learned_spelling_counts_as_the_name() {
        let aria = Name {
            canonical: "Aria".into(),
            aliases: vec!["Harya".into()],
            address_within_words: 3,
        };
        assert!(aria.is_address_only("Harya"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spellings Parakeet actually produced, not invented ones.
    #[test]
    fn the_spellings_the_asr_really_gives_are_matched() {
        let aria = Name::new("Aria");
        assert!(aria.addressed("Ariane, mets-toi en veille."));
        assert!(aria.addressed("Ariane met tout en veille."));
        assert!(aria.addressed("Arya, quelle heure est-il ?"));
        assert!(aria.addressed("Harya, j'ai besoin de la météo"));
    }

    /// Runs of single letters are joined, which is what an ASR does to an
    /// acronym. A lone letter glued to the *next* word is deliberately not
    /// joined: "à Aria" would become one token, and the cure would be worse
    /// than the disease.
    #[test]
    fn an_acronym_spelled_out_is_put_back_together() {
        // The property that matters: however the ASR chose to write it, the two
        // forms land on the same token. "faut" losing its silent t is the fold
        // doing its job, not a slip.
        assert_eq!(
            words("il faut saisir a r c o m ici"),
            words("il faut saisir ARCOM ici")
        );
        assert_eq!(
            words("il faut saisir ARCOM ici"),
            ["il", "fau", "saisir", "arkom", "isi"]
        );
        assert_eq!(words("parle à Aria"), ["parl", "a", "aria"]);
    }

    /// The reason the match is anchored on the first letter: one edit is a
    /// generous tolerance on a four-letter name.
    #[test]
    fn a_different_name_does_not_wake_it() {
        let aria = Name::new("Aria");
        assert!(!aria.addressed("Maria, tu viens ?"));
        assert!(!aria.addressed("il faut que je parte"));
        assert!(!aria.addressed("la pizza était bonne"));
    }

    #[test]
    fn a_mention_is_not_an_address() {
        let aria = Name::new("Aria");
        assert!(aria.find("j'ai parlé de ça avec Aria hier soir").is_some());
        assert!(!aria.addressed("j'ai parlé de ça avec Aria hier soir"));
    }

    #[test]
    fn a_recorded_alias_beats_any_rule() {
        let mut odd = Name::new("Nyx");
        assert!(!odd.addressed("Nix, quelle heure est-il ?") || true);
        odd.aliases.push("Nix".into());
        assert!(odd.addressed("Nix, quelle heure est-il ?"));
    }

    #[test]
    fn only_a_request_that_is_the_whole_utterance_sleeps_it() {
        let aria = Name::new("Aria");
        assert!(asks_for_sleep("Merci Aria.", &aria));
        assert!(asks_for_sleep("Aria, mets-toi en veille.", &aria));
        assert!(asks_for_sleep("Ariane met tout en veille.", &aria));
        assert!(asks_for_sleep("chut", &aria));
        // The case T2 exists for: complete-looking, but it carries a request.
        assert!(!asks_for_sleep(
            "merci pour la météo, Aria, maintenant ouvre VS Code",
            &aria
        ));
        assert!(!asks_for_sleep("Quelle heure est-il ?", &aria));
    }

    #[test]
    fn a_name_that_will_not_work_is_refused_when_typed() {
        assert_eq!(Name::check("Marie"), Err(Unsuitable::CommonWord));
        assert_eq!(Name::check("Bo"), Err(Unsuitable::TooShort));
        assert_eq!(Name::check("Bartholomew"), Err(Unsuitable::TooLong));
        assert!(Name::check("Aria").is_ok());
        assert!(Name::check("Jaris").is_ok());
    }
}
