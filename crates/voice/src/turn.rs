//! End of turn: two signals, because one of them cannot work alone.
//!
//! Smart Turn v3 judges the **waveform**. Measured on twenty labelled French
//! utterances on 2026-10-26, that splits cleanly into one solved problem and one
//! impossible one:
//!
//! | Family | Right | Median score |
//! |---|---|---|
//! | finished | 8/8 | 0.98 |
//! | hesitation — filler, lengthened vowel, pitch held up | 5/6 | 0.01 |
//! | clean cut — stops dead mid-phrase | 4/6 | **0.84** |
//!
//! A hesitation announces itself acoustically and the model hears all of it. A
//! clean cut differs from a finished sentence **by syntax alone**, and no amount
//! of waveform carries that. Waiting for a better acoustic model to fix the
//! third row is waiting for something that does not exist.
//!
//! The second opinion is already running and costs nothing: **Parakeet predicts
//! punctuation**. A transcript with no terminal mark is an unfinished sentence,
//! stated independently of how it was said.
//!
//! The rule is **asymmetric**, and that is the whole design:
//!
//! > No terminal mark ⇒ wait, whatever the prosody says.
//! > Terminal mark ⇒ the prosody decides.
//!
//! The asymmetry comes from the data, not from taste. The ASR punctuates nearly
//! everything, so the *absence* of a mark is strong evidence of an unfinished
//! sentence, while its *presence* is weak evidence of a finished one — three
//! deliberate cut-offs came back with a full stop, one of them after the model
//! invented the missing words.

use crate::mel;
use anyhow::{Result, anyhow, bail};
use parakeet_rs::ort::{self, session::Session, value::Tensor};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Smart Turn v3, loaded and ready to be asked a question.
///
/// Asked at a **pause**, never on the critical path: 16 ms per decision on CPU,
/// feature extraction included, and the pause is where that time is free.
pub struct Detector {
    session: Session,
    mel: mel::Mel,
}

impl Detector {
    /// 8.3 MB, and it lives outside the repository like the rest of the models.
    pub fn default_model_path() -> PathBuf {
        if let Ok(path) = std::env::var("ARIA_TURN_MODEL") {
            return PathBuf::from(path);
        }
        crate::models_root().join("smart-turn-v3").join("smart-turn-v3.2-cpu.onnx")
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            bail!(
                "no model at {}\n\
                 From: https://huggingface.co/pipecat-ai/smart-turn-v3, file \
                 smart-turn-v3.2-cpu.onnx",
                path.display()
            );
        }
        // Shared, never built here: ONNX Runtime allows one environment per
        // process and the voice chain loads two models.
        let environment = crate::asr::environment()?;
        let session = Session::builder(&environment)
            .map_err(|e| anyhow!("cannot build a session: {e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow!("cannot load the graph: {e}"))?;
        Ok(Self {
            session,
            mel: mel::Mel::new(),
        })
    }

    /// Probability that the turn ended at the **end** of `audio`.
    ///
    /// Two alignment rules, and both were learned by getting them wrong:
    ///
    /// - the audio handed over is the last 8 s of the **stream** ending at the
    ///   pause, silence included — never the isolated segment. An isolated
    ///   phrase leaves six seconds of zeros after it, so the model sees speech
    ///   that stopped long ago and answers "finished"; that is how three
    ///   genuine cut-offs scored 0.97;
    /// - shorter than the window, it is padded on the **left**. Whisper pads on
    ///   the right, which is right for a transcriber, where position means
    ///   nothing. Here the moment being judged is the end of the audio, and
    ///   padding on the right moves it away from the edge. Measured on the
    ///   labelled set: seven suspensions under 0.10 instead of three.
    pub fn prosody(&mut self, audio: &[f32]) -> Result<f32> {
        let mut window = vec![0f32; mel::WINDOW_SAMPLES];
        let take = audio.len().min(mel::WINDOW_SAMPLES);
        let at = mel::WINDOW_SAMPLES - take;
        window[at..].copy_from_slice(&audio[audio.len() - take..]);

        let features = self.mel.features(&window);
        let input = Tensor::from_array(([1usize, mel::N_MELS, mel::WINDOW_FRAMES], features))
            .map_err(|e| anyhow!("cannot build the input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs!["input_features" => input])
            .map_err(|e| anyhow!("inference failed: {e}"))?;
        let (_, logits) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("cannot read the logit: {e}"))?;
        // The output named `logits` is **already a probability**, despite the
        // name. Found by arithmetic: a sigmoid on top gave a near-constant 0.71
        // to 0.73 across every segment, and inverting it recovered 0.12, 0.895,
        // 0.944, 0.995 — the shape of a probability. A second sigmoid squashes
        // [0, 1] into [0.5, 0.73], which is exactly the flat curve that looked
        // like broken features.
        Ok(logits[0])
    }

    /// The window to hand to [`prosody`](Self::prosody) for a pause at `end` in
    /// `stream`.
    pub fn window(stream: &[f32], end: usize) -> &[f32] {
        &stream[end.saturating_sub(mel::WINDOW_SAMPLES)..end]
    }
}

/// What to do at a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Answer.
    Over,
    /// Keep listening: the sentence is not done.
    Continuing,
}

/// Does this transcript end mid-sentence?
///
/// An ellipsis counts as unfinished even though it is made of full stops —
/// Parakeet writes a trailing-off as `. . .`, and reading its last character
/// would file the clearest hesitation in the set under "finished".
pub fn looks_unfinished(transcript: &str) -> bool {
    let text = transcript.trim_end();
    if text.is_empty() {
        return true;
    }
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.ends_with("...") || compact.ends_with('…') {
        return true;
    }
    !matches!(
        text.chars().next_back(),
        Some('.') | Some('!') | Some('?') | Some(';') | Some(':')
    )
}

/// Words that, said last, announce more: « en gros », « donc », « et »…
///
/// Parakeet puts a full stop at every pause, so « …tous mes projets. En
/// gros. » reads as finished and was answered (2026-11-23). These do not make
/// the turn unfinished — that would wait for ever on someone who really did
/// stop there — they buy it more silence. See [`patience`].
const LEADS_ON: [&str; 16] = [
    "en gros",
    "donc",
    "et",
    "mais",
    "alors",
    "du coup",
    "en fait",
    "c'est-à-dire",
    "parce que",
    "genre",
    "enfin",
    "bref",
    "puis",
    "ensuite",
    "aussi",
    "par exemple",
];

/// Whether the last words lead on to more. See [`LEADS_ON`].
pub fn leads_on(transcript: &str) -> bool {
    let text: String = transcript
        .to_lowercase()
        .replace('’', "'")
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .to_string();
    LEADS_ON.iter().any(|lead| {
        text.strip_suffix(lead)
            .is_some_and(|before| before.is_empty() || !before.ends_with(|c: char| c.is_alphanumeric()))
    })
}

/// How much **more** silence a turn judged over should get before it is
/// handed on — a slowing down, never a wait without end.
///
/// The sentence detector answers « is this sentence finished? ». Explaining a
/// subject is a string of finished sentences with breaths between them, and on
/// 2026-11-23 one explanation went out as five turns and three delegations.
/// What tells « finished sentence » from « finished speaking » is not in the
/// sentence; it is in how long the person has been talking, and in the word
/// they stopped on.
///
/// - a short turn — a question, an order — gets nothing extra: answering it in
///   300 ms is what makes ARIA feel quick;
/// - a turn that stopped on a word that leads on gets the most;
/// - a long one gets more the longer it is.
///
/// `spoken_ms` is the span from the first speech of the turn to the pause.
pub fn patience(transcript: &str, spoken_ms: u64) -> Duration {
    let lead = if leads_on(transcript) { 900 } else { 0 };
    let length = match spoken_ms {
        0..4_000 => 0,
        4_000..10_000 => 500,
        _ => 900,
    };
    Duration::from_millis(lead.max(length))
}

/// The two signals, combined.
///
/// `prosody` is Smart Turn's probability and `threshold` the exposed setting.
/// Transcript first: it is the cheaper test and the one that can veto outright.
pub fn decide(prosody: f32, threshold: f32, transcript: &str) -> Turn {
    if looks_unfinished(transcript) {
        return Turn::Continuing;
    }
    if prosody >= threshold {
        Turn::Over
    } else {
        Turn::Continuing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transcripts as Parakeet actually produced them on the calibration set —
    /// not invented examples. The spacing in `. . .` is its own, and the whole
    /// point of the first two cases.
    #[test]
    fn an_ellipsis_is_not_a_full_stop() {
        assert!(looks_unfinished(
            "Je cherche le nom de. . . comment dire. . ."
        ));
        assert!(looks_unfinished("Euh. . . Est-ce que tu pourrais. . ."));
        assert!(!looks_unfinished("Deux cent trente-quatre euros."));
    }

    /// The turns of 2026-11-23, as Parakeet wrote them.
    #[test]
    fn a_word_that_leads_on_buys_silence() {
        assert!(leads_on("A garder en mémoire, mais tous mes projets. Aria. En gros."));
        assert!(leads_on("Je veux un renard, et du coup."));
        assert!(leads_on("C'est-à-dire."));
        assert!(!leads_on("Quelle heure est-il ?"));
        assert!(!leads_on("Mets le minuteur à dix minutes."));
        // A word that merely ends in one of them is not it.
        assert!(!leads_on("Il fait très chaud au Maroc."), "« maroc » is not « et »");
        assert!(!leads_on("Ouvre le projet Coupa Crypt."));
    }

    /// A slowing down, never a wait: every case ends, and a short request is
    /// not slowed at all.
    #[test]
    fn patience_grows_with_the_turn_and_is_bounded() {
        assert_eq!(patience("Quelle heure est-il ?", 1_500), Duration::ZERO);
        assert!(patience("Tous mes projets. En gros.", 1_500) > Duration::ZERO);
        let long = "…sans qu'il n'y ait jamais accès.";
        assert!(patience(long, 6_000) > Duration::ZERO);
        assert!(patience(long, 12_000) >= patience(long, 6_000));
        assert!(patience("Donc.", 60_000) <= Duration::from_secs(1));
    }

    #[test]
    fn a_dangling_word_has_no_terminal_mark() {
        assert!(looks_unfinished("Le rendez-vous est prévu pour"));
        assert!(looks_unfinished(
            "Donc si je comprends bien, tu veux dire que"
        ));
        assert!(looks_unfinished("Donne-moi la liste des"));
        assert!(!looks_unfinished("Quelle heure est-il?"));
        assert!(!looks_unfinished("Merci Aria."));
    }

    /// The two cases that justify the whole module: the waveform says
    /// "finished" and it is wrong, and only the transcript knows.
    #[test]
    fn the_transcript_overrides_a_confident_waveform() {
        assert_eq!(
            decide(0.97, 0.85, "Le rendez-vous est prévu pour"),
            Turn::Continuing
        );
        assert_eq!(
            decide(0.94, 0.85, "Je cherche le nom de. . . comment dire. . ."),
            Turn::Continuing
        );
    }

    /// And the two it cannot save, stated so nobody expects it to.
    ///
    /// The first: the ASR invented "l'appliquer" and punctuated it, so both
    /// signals agree on a turn that was not over. The second: "comment dire"
    /// is a complete idiomatic chunk, the voice falls on it naturally, and both
    /// the ASR and the waveform heard what was actually said — a bad item in
    /// the set rather than a failure of either model.
    ///
    /// Neither is recoverable from text or from prosody. The grace timer is
    /// what catches them: ARIA starts answering, the speaker carries on, the
    /// turn is cancelled.
    #[test]
    fn two_cases_defeat_both_signals() {
        assert_eq!(
            decide(0.96, 0.85, "J'aimerais que tu t'occupes de l'appliquer."),
            Turn::Over
        );
        assert_eq!(
            decide(0.94, 0.85, "Je cherche le nom de... comment dire."),
            Turn::Over
        );
    }

    #[test]
    fn a_punctuated_sentence_still_needs_the_prosody() {
        // "Déplace le fichier dans." — punctuated, but the voice did not fall.
        assert_eq!(
            decide(0.01, 0.85, "Déplace le fichier dans."),
            Turn::Continuing
        );
    }
}
