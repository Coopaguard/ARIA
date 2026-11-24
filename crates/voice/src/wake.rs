//! The wake word, heard **in the sound** rather than looked for in a transcript.
//!
//! Until 2026-11-22 the name was found by [`crate::name::Name::addressed`], so
//! after 200 to 300 ms of ASR on a closed segment, and with the ASR's mistakes:
//! « A rien » for « Aria » lost the wake and the spoken sleep together (epic
//! 09). The detector here is a 63 k-parameter CNN trained in `../ariawakeword`
//! against [`crate::mel`] — the same function, not a copy.
//!
//! # The model is a resource of the application, not a download
//!
//! `app/resources/wake/wake.onnx` (250 KB, `ariawakeword/models-augmente2`) is
//! a Tauri bundle resource: the installer puts it at `<resources>/wake/`, beside
//! the executable, and `tauri dev` copies it under `target/`. Nothing to fetch,
//! no folder under `%LOCALAPPDATA%/aria/models`, and **the next model replaces
//! that file** — in the repository, or in an installed copy, without
//! rebuilding. `app/` resolves the path; this crate only opens it.
//! [`Settings::model`] points at another file, and the threshold and `k` are
//! settings for the same reason: a new model comes with its own operating point.
//!
//! `voix.tsv`, beside it, lists the voices it was trained on, because a licence
//! is a condition of shipping: one of them, `kyutai-fr-cosette`, is **not**
//! commercial (decision of 2026-10-24).
//!
//! # The contract of the graph
//!
//! - input `mels`: `[batch, 80, 100]` f32 — one second of 16 kHz mono, the
//!   log-mel of [`crate::mel::Mel::features_over`], normalised **over that
//!   second** (the maximum of the window is part of the convention);
//! - output `probabilite`: `[batch, 1]` f32, already through a sigmoid;
//! - one decision every 100 ms, over the last second. Before a full second has
//!   been heard, the window is silence on the left — what a stream's start is.
//!
//! # Two ears since 2026-11-23: the log-mel model and openWakeWord, averaged
//!
//! `app/resources/wake/oww/` holds openWakeWord's two feature models
//! (v0.5.1, see `NOTICE.txt`) and `oww.onnx`, a head trained in `ariawakeword`
//! (`models-oww`) on their embeddings. When it is there and
//! [`Settings::fusion`] is on, each decision is the mean of the two
//! probabilities — what `ariawakeword`'s `evaluer --poids … --oww …` measures,
//! checked decision by decision by its test `parite_aria` (gap < 1e-5 on real
//! recordings). openWakeWord generalises to real voices far better; the
//! log-mel model keeps it from waking on everything.
//!
//! # What it is worth, measured in `ariawakeword` (2026-11-23)
//!
//! | threshold / k | real "Aria" (38, 3 people) | false wakes / h of continuous speech |
//! |---|---|---|
//! | 0.1 / 1 | 89.5 % | 202 |
//! | **0.3 / 1** (default) | **84.2 %** | **89** |
//! | 0.5 / 1 | 63.2 % | 8.7 |
//! | 0.8 / 2 | 47 % | 0.6 |
//!
//! False wakes on 25 h of Common Voice FR, read aloud without pauses: a room
//! where people talk now and then hears far fewer. **Sensitivity first** (the
//! user's decision of 2026-11-23): a missed "Aria" is worse than a wake on
//! « Daria a pris le train ». So [`Settings::confirm`] is off by default too —
//! the transcript was refusing true wakes it heard as « A rien ».

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use parakeet_rs::ort::{self, session::Session, value::Tensor};
use serde::{Deserialize, Serialize};

use crate::mel::{self, HOP, N_MELS};

/// The model's place inside the application's resource directory.
pub const RESOURCE: &str = "wake/wake.onnx";

const INPUT: &str = "mels";
const OUTPUT: &str = "probabilite";

/// One second, in frames of 10 ms and in samples.
pub const WINDOW_FRAMES: usize = 100;
pub const WINDOW_SAMPLES: usize = WINDOW_FRAMES * HOP;
/// 100 ms between two decisions — what the model was trained at.
pub const STRIDE_SAMPLES: usize = 10 * HOP;

const WINDOW_SECONDS: f32 = WINDOW_SAMPLES as f32 / 16_000.0;
const STEP_SECONDS: f32 = STRIDE_SAMPLES as f32 / 16_000.0;

/// What `aria.toml` says about the wake word, under `[wake]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Off falls back on the transcript alone, as before 2026-11-22.
    pub enabled: bool,
    /// Another `.onnx` with the same contract, instead of the shipped one
    /// ([`RESOURCE`]). Empty in the file means the shipped one.
    pub model: Option<PathBuf>,
    /// The probability to reach.
    pub threshold: f32,
    /// How many windows **in a row** above it — one "Aria" spans about five.
    pub consecutive: usize,
    /// Seconds after a wake during which no other counts.
    pub refractory: f32,
    /// The model opens, the transcript must also hold the name.
    ///
    /// Only for an ear that transcribes: one that hands the agent sound
    /// (epic 10) has no transcript, so there the model decides alone.
    pub confirm: bool,
    /// Average with openWakeWord when [`OWW_DIR`] sits beside the model.
    /// Off: the log-mel model alone, as before 2026-11-23.
    pub fusion: bool,
}

impl Default for Settings {
    /// The compromise chosen on 2026-11-23 for the fused detector: 84 % of
    /// real "Aria", no transcript confirmation. Without `oww/` (or with
    /// `fusion` off) 0.3 is far too low for the log-mel model alone — 0.95 / 2
    /// was its point.
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            threshold: 0.3,
            consecutive: 1,
            refractory: 1.5,
            confirm: false,
            fusion: true,
        }
    }
}

impl Settings {
    /// A hand-edited file must not make listening worse than the defaults.
    pub fn is_plausible(&self) -> bool {
        self.threshold > 0.0
            && self.threshold <= 1.0
            && (1..=10).contains(&self.consecutive)
            && (0.0..=10.0).contains(&self.refractory)
    }

    pub fn trigger(&self) -> Trigger {
        Trigger {
            threshold: self.threshold,
            consecutive: self.consecutive,
            refractory: self.refractory,
        }
    }
}

/// From probabilities per window to **wakes**. Copied from
/// `ariawakeword/src/stream.rs`: the model without these rules does not give
/// the figures measured there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trigger {
    pub threshold: f32,
    pub consecutive: usize,
    pub refractory: f32,
}

impl Trigger {
    /// The wakes in a stream of probabilities, in seconds since its start.
    pub fn events(&self, probabilities: &[f32]) -> Vec<f32> {
        let mut live = self.live();
        probabilities.iter().filter_map(|&p| live.push(p)).collect()
    }

    pub fn live(&self) -> Live {
        Live {
            trigger: *self,
            index: 0,
            run: 0,
            quiet_until: f32::MIN,
        }
    }
}

/// The trigger, one probability at a time. [`Trigger::events`] goes through it,
/// so what is measured and what runs live cannot diverge.
#[derive(Clone, Debug)]
pub struct Live {
    trigger: Trigger,
    index: usize,
    run: usize,
    quiet_until: f32,
}

impl Live {
    /// The next window's probability; the instant of the wake if there is one.
    ///
    /// The instant of a window is its **end**: the moment the decision becomes
    /// possible live.
    pub fn push(&mut self, p: f32) -> Option<f32> {
        let t = WINDOW_SECONDS + self.index as f32 * STEP_SECONDS;
        self.index += 1;
        self.run = if p >= self.trigger.threshold { self.run + 1 } else { 0 };
        if self.run >= self.trigger.consecutive.max(1) && t >= self.quiet_until {
            self.quiet_until = t + self.trigger.refractory;
            self.run = 0;
            return Some(t);
        }
        None
    }
}

/// The detector, loaded and listening.
pub struct Detector {
    session: Session,
    mel: mel::Mel,
    /// The last second, silence on the left until one has been heard.
    ring: Vec<f32>,
    /// Samples since the last decision.
    since: usize,
    /// Decisions since the start (or the last reset): the first is number 1.
    decisions: usize,
    /// openWakeWord, averaged with the log-mel model when it is there.
    oww: Option<Oww>,
    live: Live,
}

impl Detector {
    /// The model at `path` — resolved by the caller, which knows where the
    /// application's resources are — with the trigger of `settings`.
    ///
    /// When [`OWW_DIR`] sits beside it with its three files and
    /// [`Settings::fusion`] is on, each decision is the **mean** of the two
    /// models, as measured in `ariawakeword` (`evaluer --poids … --oww …`).
    pub fn load(path: &Path, settings: &Settings) -> Result<Self> {
        let oww = match path.parent().map(|d| d.join(OWW_DIR)) {
            Some(dir) if settings.fusion && dir.is_dir() => {
                let mut oww = Oww::load(&dir)?;
                // The first `run`s allocate; paid here, then forgotten.
                oww.probability(1)?;
                oww.reset();
                Some(oww)
            }
            _ => None,
        };
        if !path.is_file() {
            bail!(
                "no wake model at {}\n\
                 From: ariawakeword, models-augmente2/wake.onnx — shipped as \
                 app/resources/wake/wake.onnx",
                path.display()
            );
        }
        // Shared, never built here: one ONNX environment per process.
        let environment = crate::asr::environment()?;
        let session = Session::builder(&environment)
            .map_err(|e| anyhow!("cannot build a session: {e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow!("cannot load {}: {e}", path.display()))?;
        let mut detector = Self {
            session,
            mel: mel::Mel::new(),
            ring: vec![0.0; WINDOW_SAMPLES],
            since: 0,
            decisions: 0,
            oww,
            live: settings.trigger().live(),
        };
        // The first `run` allocates the arena; paid here, not on the first word.
        detector.probability_of_ring()?;
        Ok(detector)
    }

    /// Whether openWakeWord is averaged in.
    pub fn fused(&self) -> bool {
        self.oww.is_some()
    }

    /// Feed captured samples. True if the name was just heard.
    pub fn push(&mut self, samples: &[f32]) -> Result<bool> {
        let mut woke = false;
        for p in self.probabilities(samples)? {
            woke |= self.live.push(p).is_some();
        }
        Ok(woke)
    }

    /// Feed captured samples; the probability of each decision they complete,
    /// without the trigger. What `ariawakeword` compares against its own
    /// pipeline, so that the figures measured there hold here.
    pub fn probabilities(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let mut out = Vec::new();
        for chunk in samples.chunks(STRIDE_SAMPLES) {
            let room = STRIDE_SAMPLES - self.since;
            let (now, later) = chunk.split_at(chunk.len().min(room));
            out.extend(self.take(now)?);
            out.extend(self.take(later)?);
        }
        Ok(out)
    }

    /// Restart the trigger's clock and forget the last second — after a
    /// reload of the ear, so that a stale window cannot wake.
    pub fn reset(&mut self, settings: &Settings) {
        self.ring.iter_mut().for_each(|s| *s = 0.0);
        self.since = 0;
        self.decisions = 0;
        if let Some(oww) = &mut self.oww {
            oww.reset();
        }
        self.live = settings.trigger().live();
    }

    /// At most one stride's worth: at most one decision.
    fn take(&mut self, samples: &[f32]) -> Result<Option<f32>> {
        if samples.is_empty() {
            return Ok(None);
        }
        self.ring.drain(..samples.len());
        self.ring.extend_from_slice(samples);
        if let Some(oww) = &mut self.oww {
            oww.audio.extend_from_slice(samples);
        }
        self.since += samples.len();
        if self.since < STRIDE_SAMPLES {
            return Ok(None);
        }
        self.since = 0;
        self.decisions += 1;
        let p = self.probability_of_ring()?;
        Ok(Some(match &mut self.oww {
            Some(oww) => 0.5 * (p + oww.probability(self.decisions)?),
            None => p,
        }))
    }

    fn probability_of_ring(&mut self) -> Result<f32> {
        let features = self.mel.features_over(&self.ring, WINDOW_FRAMES);
        probability(&mut self.session, features)
    }
}

fn probability(session: &mut Session, features: Vec<f32>) -> Result<f32> {
    let input = Tensor::from_array(([1usize, N_MELS, WINDOW_FRAMES], features))
        .map_err(|e| anyhow!("cannot build the input tensor: {e}"))?;
    let outputs = session
        .run(ort::inputs![INPUT => input])
        .map_err(|e| anyhow!("wake inference failed: {e}"))?;
    let (_, p) = outputs[OUTPUT]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow!("cannot read the probability: {e}"))?;
    Ok(p[0])
}

// ---------------------------------------------------------------------------
// openWakeWord, the second ear.

/// Beside the model: `melspectrogram.onnx` and `embedding_model.onnx` from
/// openWakeWord v0.5.1 (licences in `app/resources/wake/NOTICE.txt`; its own
/// wake models are not used), and `oww.onnx`, the head trained in `ariawakeword` on their output.
pub const OWW_DIR: &str = "oww";

/// A piece of sound: its step, what is read of it, its frames.
const PIECE: usize = 1280;
const PIECE_READ: usize = PIECE + 480;
const PIECE_FRAMES: usize = 8;
const MEL_BANDS: usize = 32;
const PIECE_VALUES: usize = PIECE_FRAMES * MEL_BANDS;
/// An embedding reads 10 pieces (76 frames); the next starts one piece later.
const PIECES_PER_EMBEDDING: usize = 10;
const EMBED_FRAMES: usize = 76;
const EMBED_DIM: usize = 96;
/// The head reads 16 embeddings.
const EMBEDDINGS: usize = 16;
const PIECES: usize = EMBEDDINGS - 1 + PIECES_PER_EMBEDDING;
const CONTEXT_SAMPLES: usize = (PIECES - 1) * PIECE + PIECE_READ;
/// Piece `m` starts at `m × PIECE - OFFSET` in the stream that begins with
/// one second of silence — the one [`Detector`]'s ring sees.
const OFFSET: usize = CONTEXT_SAMPLES - WINDOW_SAMPLES;

/// openWakeWord **as `ariawakeword` computes it**, which is what its head was
/// trained and measured on (`src/oww.rs` there, parity test against this
/// type): each 80 ms piece through the mel model **alone** (its floor depends
/// on the whole batch), `x / 10 + 2`, embeddings of 76 frames, and decision
/// `k` reading the 16 embeddings whose last piece ends at or before the end of
/// its window — 0, 20, 40 or 60 ms earlier, the 100 ms grid crossing the 80 ms
/// one every 400 ms.
///
/// Pieces and embeddings are kept between decisions: about 2.5 ms per
/// decision instead of 22 ms from scratch.
struct Oww {
    mel: Session,
    embed: Session,
    head: Session,
    /// The stream from position `base`: one second of silence, then the sound.
    audio: Vec<f32>,
    base: usize,
    /// Mel frames of pieces `pieces_from ..`.
    pieces: std::collections::VecDeque<Vec<f32>>,
    pieces_from: usize,
    /// Embeddings `embeddings_from ..`.
    embeddings: std::collections::VecDeque<Vec<f32>>,
    embeddings_from: usize,
}

impl Oww {
    fn load(dir: &Path) -> Result<Self> {
        let environment = crate::asr::environment()?;
        let open = |name: &str| -> Result<Session> {
            let path = dir.join(name);
            if !path.is_file() {
                bail!(
                    "{} is missing — {OWW_DIR}/ holds melspectrogram.onnx and embedding_model.onnx \
                     (openWakeWord v0.5.1) and oww.onnx (ariawakeword)",
                    path.display()
                );
            }
            Session::builder(&environment)
                .map_err(|e| anyhow!("cannot build a session: {e}"))?
                .with_intra_threads(1)
                .map_err(|e| anyhow!("{e}"))?
                .commit_from_file(&path)
                .map_err(|e| anyhow!("cannot load {}: {e}", path.display()))
        };
        let mut oww = Self {
            mel: open("melspectrogram.onnx")?,
            embed: open("embedding_model.onnx")?,
            head: open("oww.onnx")?,
            audio: Vec::new(),
            base: 0,
            pieces: Default::default(),
            pieces_from: 0,
            embeddings: Default::default(),
            embeddings_from: 0,
        };
        oww.reset();
        Ok(oww)
    }

    fn reset(&mut self) {
        self.audio = vec![0.0; WINDOW_SAMPLES];
        self.base = 0;
        self.pieces.clear();
        self.pieces_from = 0;
        self.embeddings.clear();
        self.embeddings_from = 0;
    }

    /// The probability of decision `k` (≥ 1), whose window ends at
    /// `WINDOW_SAMPLES + k × STRIDE_SAMPLES`, once that much has been heard.
    fn probability(&mut self, k: usize) -> Result<f32> {
        let first = k * STRIDE_SAMPLES / PIECE;
        // Forget what no later decision reads.
        while self.embeddings_from < first && !self.embeddings.is_empty() {
            self.embeddings.pop_front();
            self.embeddings_from += 1;
        }
        while self.pieces_from < first && !self.pieces.is_empty() {
            self.pieces.pop_front();
            self.pieces_from += 1;
        }
        if self.embeddings.is_empty() {
            self.embeddings_from = first;
        }
        if self.pieces.is_empty() {
            self.pieces_from = first;
        }
        // The missing embeddings, in one batch.
        let from = self.embeddings_from + self.embeddings.len();
        let to = first + EMBEDDINGS;
        if from < to {
            let mut stack = Vec::with_capacity((to - from) * EMBED_FRAMES * MEL_BANDS);
            for j in from..to {
                for m in j..j + PIECES_PER_EMBEDDING {
                    self.piece(m)?;
                }
                let start = j - self.pieces_from;
                let mut frames = Vec::with_capacity(PIECES_PER_EMBEDDING * PIECE_VALUES);
                for p in self.pieces.range(start..start + PIECES_PER_EMBEDDING) {
                    frames.extend_from_slice(p);
                }
                stack.extend_from_slice(&frames[..EMBED_FRAMES * MEL_BANDS]);
            }
            let n = to - from;
            let input = Tensor::from_array(([n, EMBED_FRAMES, MEL_BANDS, 1], stack))
                .map_err(|e| anyhow!("cannot build the embedding input: {e}"))?;
            let outputs = self
                .embed
                .run(ort::inputs!["input_1" => input])
                .map_err(|e| anyhow!("openWakeWord embedding failed: {e}"))?;
            let (_, e) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("cannot read the embeddings: {e}"))?;
            if e.len() != n * EMBED_DIM {
                bail!("openWakeWord gave {} values for {n} embeddings", e.len());
            }
            self.embeddings.extend(e.chunks(EMBED_DIM).map(<[f32]>::to_vec));
        }
        // The sound before the first piece still to come is no longer read.
        let keep_from = (first * PIECE).saturating_sub(OFFSET);
        if keep_from > self.base {
            self.audio.drain(..(keep_from - self.base).min(self.audio.len()));
            self.base = keep_from;
        }

        let mut features = Vec::with_capacity(EMBEDDINGS * EMBED_DIM);
        let start = first - self.embeddings_from;
        for e in self.embeddings.range(start..start + EMBEDDINGS) {
            features.extend_from_slice(e);
        }
        let input = Tensor::from_array(([1usize, EMBEDDINGS, EMBED_DIM], features))
            .map_err(|e| anyhow!("cannot build the head input: {e}"))?;
        let outputs = self
            .head
            .run(ort::inputs!["empreintes" => input])
            .map_err(|e| anyhow!("openWakeWord head failed: {e}"))?;
        let (_, p) = outputs[OUTPUT]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("cannot read the probability: {e}"))?;
        Ok(p[0])
    }

    /// Make sure piece `m` is computed (pieces are computed in order).
    fn piece(&mut self, m: usize) -> Result<()> {
        if m < self.pieces_from + self.pieces.len() {
            return Ok(());
        }
        debug_assert_eq!(m, self.pieces_from + self.pieces.len());
        // Stream position of the piece's first sample; before 0, silence.
        let start = (m * PIECE) as isize - OFFSET as isize;
        let pcm: Vec<f32> = (0..PIECE_READ as isize)
            .map(|j| {
                let at = start + j;
                if at < self.base as isize {
                    return 0.0;
                }
                let i = (at as usize) - self.base;
                self.audio
                    .get(i)
                    .map_or(0.0, |x| (x * 32767.0).clamp(-32768.0, 32767.0))
            })
            .collect();
        let input = Tensor::from_array(([1usize, PIECE_READ], pcm))
            .map_err(|e| anyhow!("cannot build the mel input: {e}"))?;
        let outputs = self
            .mel
            .run(ort::inputs!["input" => input])
            .map_err(|e| anyhow!("openWakeWord mel failed: {e}"))?;
        let (_, mel) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("cannot read the mel: {e}"))?;
        if mel.len() != PIECE_VALUES {
            bail!("openWakeWord mel gave {} values for a piece, not {PIECE_VALUES}", mel.len());
        }
        self.pieces.push_back(mel.iter().map(|&x| x / 10.0 + 2.0).collect());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(threshold: f32, consecutive: usize) -> Trigger {
        Trigger {
            threshold,
            consecutive,
            refractory: 1.5,
        }
    }

    /// Five high windows in a row — one "Aria" — make **one** wake.
    #[test]
    fn one_word_makes_one_wake() {
        let p = [0.0, 0.9, 0.95, 0.97, 0.9, 0.8, 0.0, 0.0];
        assert_eq!(trigger(0.5, 1).events(&p).len(), 1);
    }

    #[test]
    fn smoothing_drops_an_isolated_window() {
        let p = [0.0, 0.9, 0.0, 0.0];
        assert_eq!(trigger(0.5, 1).events(&p).len(), 1);
        assert!(trigger(0.5, 2).events(&p).is_empty());
    }

    #[test]
    fn the_instant_is_the_end_of_the_window() {
        let p = [0.0, 0.0, 0.9, 0.9];
        let e = trigger(0.5, 2).events(&p);
        assert!((e[0] - (WINDOW_SECONDS + 3.0 * STEP_SECONDS)).abs() < 1e-6);
    }

    #[test]
    fn two_words_apart_make_two_wakes() {
        let mut p = vec![0.9; 3];
        p.extend(vec![0.0; 20]);
        p.extend(vec![0.9; 3]);
        assert_eq!(trigger(0.5, 1).events(&p).len(), 2);
    }

    #[test]
    fn the_defaults_are_plausible() {
        assert!(Settings::default().is_plausible());
        let mut wrong = Settings::default();
        wrong.consecutive = 0;
        assert!(!wrong.is_plausible());
    }

    /// The resource in the repository, read where `app/tauri.conf.json` bundles
    /// it from.
    fn shipped() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/resources")
            .join(RESOURCE)
    }

    /// The resource is the model the code expects: it loads, takes `mels`,
    /// answers `probabilite`, and does not wake on silence.
    ///
    /// Not `#[ignore]`: 250 KB, a few milliseconds. A replaced `.onnx` with
    /// another contract fails here, not in the room.
    #[test]
    fn the_shipped_model_loads_and_sleeps_through_silence() {
        let mut detector = Detector::load(&shipped(), &Settings::default()).expect("the shipped model");
        assert!(detector.fused(), "app/resources/wake/oww/ is shipped beside the model");
        let woke = detector
            .push(&vec![0.0; 3 * WINDOW_SAMPLES])
            .expect("inference");
        assert!(!woke);
        let p = detector.probability_of_ring().expect("inference");
        assert!((0.0..=1.0).contains(&p), "{p}");
    }

    /// One decision per 100 ms, whatever the size of the chunks handed in.
    #[test]
    fn decisions_follow_the_stride_not_the_chunks() {
        let mut detector = Detector::load(&shipped(), &Settings::default()).expect("the shipped model");
        let mut decisions = 0;
        for _ in 0..50 {
            detector.push(&[0.0; 320]).unwrap();
            if detector.since == 0 {
                decisions += 1;
            }
        }
        // 50 × 20 ms = 1 s = 10 strides.
        assert_eq!(decisions, 10);
    }
}
