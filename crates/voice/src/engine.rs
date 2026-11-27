//! The chain, assembled: frames in, "here is what was heard" out.
//!
//! This is the piece that makes the rest callable. It owns the models and the
//! buffer, and it has **no opinion about what to do** with what it hears — that
//! is `aria-core`'s gate, and the split is what keeps the core building without
//! an ONNX runtime.
//!
//! ```text
//! frames → gate (pauses) → detector (prosody) → ASR (text) → decide → Heard
//! ```
//!
//! # What it deliberately does not do
//!
//! **It does not run the echo gate**, and the reason has changed. It used to be
//! "there is no TTS yet, so there is nothing to compare against". There is a
//! TTS now, and a gate driven frame by frame ([`crate::echo::Live`]) — but it
//! runs in the shell's capture loop, beside this, not inside it. Cutting
//! playback needs the `tts::Silencer`, which belongs to whoever owns the
//! loudspeakers, and this type makes no decisions.
//!
//! The gate is applied to each frame **before** this type sees it, since
//! 2026-11-23 (decision 135): the capture loop turns the frames the gate calls
//! ARIA's own voice down under the speech floor, so her echo no longer opens a
//! segment. Done there rather than here because the reference belongs to the
//! loudspeaker's owner. A frame the gate cannot judge yet still comes through,
//! and the word veto stays the defence for it.
//!
//! **It does not decide anything.** [`Heard`] reports; the caller acts.
//!
//! # The name is heard in the sound first
//!
//! Since 2026-11-22 the wake-word detector ([`crate::wake`]) runs on every
//! frame, beside the segmenter. `Heard::addressed` is its verdict, confirmed by
//! the transcript unless `wake::Settings::confirm` is off, and in hybrid a turn
//! it did not wake on is **not transcribed at all**. With the detector off or
//! missing, the transcript decides alone, as before.

use crate::calibration::{Calibration, FRAME};
use crate::name::{self, Name};
use crate::{gate, turn};
use anyhow::{Result, anyhow};
use parakeet_rs::{ParakeetTDT, Transcriber};

/// How much audio one turn may accumulate before the front is dropped.
///
/// A bound, not a feature: without it a microphone left open in a noisy room
/// grows the buffer until the process dies, and the ASR cost grows with it
/// because every pause re-transcribes the turn so far.
const MAX_TURN_SECONDS: usize = 60;

/// Silence kept while nobody has started speaking, before the stream starts
/// again from nothing.
///
/// Since 2026-11-27. The minute above was counted from the last turn, silence
/// included: 36 s of an empty room, then a 25 s request, and the minute ran out
/// in the middle of it — the turn was restarted and only « La troisième. »
/// reached the agent. Silence before the first word is worth nothing to any
/// model, so it is not allowed to eat the minute.
const IDLE_SECONDS: usize = 5;

/// Whether the stream may start again from nothing: no word of a turn in it,
/// and longer than [`IDLE_SECONDS`].
fn idle_overflow(stream_len: usize, in_flight: bool) -> bool {
    !in_flight && stream_len > IDLE_SECONDS * crate::wav::SAMPLE_RATE as usize
}

/// What the chain heard at a pause.
#[derive(Clone, Debug)]
pub struct Heard {
    /// The turn so far, as the ASR writes it — punctuation included, which is
    /// half of the end-of-turn decision.
    pub transcript: String,
    /// Over, or still going.
    pub turn: turn::Turn,
    /// Smart Turn's probability, kept so the interface and the logs can show
    /// *why*, and so a false positive can be counted later.
    pub prosody: f32,
    /// The name was heard, early enough to be an address rather than a mention.
    pub addressed: bool,
    /// A complete turn asking for quiet. Only ever true when `turn` is over —
    /// "merci Aria, et peux-tu aussi…" must put nothing to sleep.
    pub sleep_request: bool,
    /// A complete turn that is **nothing but** the name.
    ///
    /// An address, not a request: someone getting ARIA's attention before
    /// saying what they want. The caller wakes the mode and keeps listening
    /// rather than answering — see [`crate::name::Name::is_address_only`],
    /// which carries what this cost to find.
    pub address_only: bool,
    /// Where the pause fell, in seconds since this turn started.
    pub at_seconds: f32,
    /// The turn so far, as sound — only from an engine that does not
    /// transcribe ([`Engine::load_listener`]), where it is what the agent
    /// receives. `None` otherwise, so a transcribing chain copies nothing.
    pub audio: Option<Vec<f32>>,
}

/// A transcript written before the pause that justified it was confirmed.
///
/// See [`Engine::speculate`] for why it exists and [`Engine::judge`] for how it
/// is spent.
struct Guess {
    /// Stream offset the transcription covered, the padding excluded. Kept for
    /// the log: "which pause was this for" is the only question worth asking
    /// of a guess that turned out wrong.
    voiced_to: usize,
    text: String,
}

/// A finished turn, waiting out the extra silence [`crate::turn::patience`]
/// gave it.
struct Held {
    heard: Heard,
    /// Quiet frames still to go before it is handed on.
    frames_left: usize,
}

/// The card's own ONNX Runtime — CUDA or OpenVINO — fetched on a thread of
/// its own when GPU inference is on and it is missing (decision 131).
///
/// After the ear has loaded, never before: it is only used from the next
/// start, so making the user wait 1.5 GB to talk would buy nothing.
fn fetch_the_cards_runtime() {
    let _ = std::thread::Builder::new()
        .name("aria-runtime".into())
        .spawn(crate::runtime::ensure_wanted);
}

/// The loaded voice chain.
pub struct Engine {
    cal: Calibration,
    name: Name,
    segmenter: gate::Segmenter,
    detector: turn::Detector,
    /// `None` for a listener: a local model that hears takes the audio itself
    /// (epic 10), and Parakeet's 2.4 GB stay on disk.
    asr: Option<ParakeetTDT>,
    /// Audio since the turn started.
    stream: Vec<f32>,
    /// Samples that do not yet make a whole frame.
    pending: Vec<f32>,
    /// A transcript written at the first quiet frame, before the silence
    /// that closes the segment had been confirmed.
    ///
    /// `Some` only ever survives an unbroken quiet run, because any frame
    /// of speech clears it — which is what makes spending it at a close
    /// safe without matching offsets. See [`Engine::speculate`].
    speculated: Option<Guess>,
    /// Where the first speech of this turn began, for a listener.
    turn_from: Option<usize>,
    /// Where Parakeet was loaded: [`crate::asr::gpu`] at the time.
    asr_on_gpu: bool,
    /// The wake-word detector ([`crate::wake`]). `None` when it is switched
    /// off or failed to load: the name is then looked for in the transcript
    /// alone, as before.
    wake: Option<crate::wake::Detector>,
    wake_settings: crate::wake::Settings,
    /// Where in [`Self::stream`] the detector fired during this turn.
    wakes: Vec<usize>,
    /// A turn judged over, given more silence before it is handed on — see
    /// [`crate::turn::patience`]. Dropped the moment speech resumes.
    held: Option<Held>,
    /// The caller only acts on a named turn (hybrid): without a wake, the ASR
    /// is not even asked.
    needs_name: bool,
}

impl Engine {
    /// Loads both models. About 3.8 s, measured — the caller has to show that.
    ///
    /// **And downloads them first if they are not there**, since 2026-11-13:
    /// 2.41 GB on a first start, which is minutes rather than seconds and the
    /// caller has to show *that* too. It is the same `Load::Hearing` wait, only
    /// much longer, and the log names each file as it starts.
    ///
    /// Asked here rather than inside [`crate::asr::load`], deliberately: a
    /// function that reads a file from disk must not quietly turn into one that
    /// fetches 2.4 GB from the network. The three binaries of this crate keep
    /// the old behaviour — a message saying what to go and get — because a
    /// developer tool that starts a huge download because a path was wrong is
    /// worse than one that refuses.
    pub fn load(cal: Calibration, name: Name) -> Result<Self> {
        crate::models::ensure(|weight, index, total| {
            tracing::info!(
                "modèle manquant {index}/{total} : {} depuis {}",
                weight.remote,
                weight.repo
            );
        })?;
        let detector = turn::Detector::load(&turn::Detector::default_model_path())?;
        let asr = crate::asr::load(&crate::asr::default_model_dir())?;
        fetch_the_cards_runtime();
        Ok(Self::assemble(cal, name, detector, Some(asr)))
    }

    /// The chain without Parakeet: pauses and end of turn, and the audio of
    /// the turn handed over instead of a transcript — epic 10.
    ///
    /// Only Smart Turn is read (8 MB). What is lost with the transcript is
    /// said where it is lost: the end of a turn is decided by **prosody
    /// alone**, since punctuation was half of [`turn::decide`]; and nothing
    /// here can hear the name, so the caller cannot run hybrid listening.
    pub fn load_listener(cal: Calibration, name: Name) -> Result<Self> {
        crate::models::ensure_weights(crate::models::listener(), |weight, index, total| {
            tracing::info!(
                "modèle manquant {index}/{total} : {} depuis {}",
                weight.remote,
                weight.repo
            );
        })?;
        let detector = turn::Detector::load(&turn::Detector::default_model_path())?;
        fetch_the_cards_runtime();
        Ok(Self::assemble(cal, name, detector, None))
    }

    fn assemble(
        cal: Calibration,
        name: Name,
        detector: turn::Detector,
        asr: Option<ParakeetTDT>,
    ) -> Self {
        let mut engine = Self {
            segmenter: gate::Segmenter::new(cal),
            cal,
            name,
            detector,
            asr,
            stream: Vec::with_capacity(MAX_TURN_SECONDS * crate::wav::SAMPLE_RATE as usize),
            pending: Vec::with_capacity(FRAME * 2),
            speculated: None,
            turn_from: None,
            asr_on_gpu: crate::asr::gpu(),
            wake: None,
            wake_settings: crate::wake::Settings {
                enabled: false,
                ..Default::default()
            },
            wakes: Vec::new(),
            needs_name: false,
            held: None,
        };
        engine.warm_up();
        engine
    }

    /// Load the wake-word detector, or drop it.
    ///
    /// **Never fatal.** A model that does not load leaves the name to the
    /// transcript, which is how ARIA heard it before — said in the log, so a
    /// detector that silently is not there does not pass for a deaf one.
    pub fn set_wake(&mut self, model: &std::path::Path, settings: &crate::wake::Settings) {
        self.wakes.clear();
        self.wake_settings = settings.clone();
        if !settings.enabled {
            self.wake = None;
            tracing::info!("mot de réveil acoustique désactivé : le nom est cherché dans le transcript");
            return;
        }
        match crate::wake::Detector::load(model, settings) {
            Ok(detector) => {
                tracing::info!(
                    modèle = %model.display(),
                    seuil = settings.threshold,
                    k = settings.consecutive,
                    confirmation = settings.confirm,
                    "mot de réveil acoustique chargé"
                );
                self.wake = Some(detector);
            }
            Err(e) => {
                tracing::error!("mot de réveil acoustique indisponible, le transcript seul décide : {e:#}");
                self.wake = None;
            }
        }
    }

    /// Whether the name can be heard without a transcript — what makes hybrid
    /// listening possible for an ear that hands over sound (epic 10).
    pub fn hears_name(&self) -> bool {
        self.wake.is_some()
    }

    /// Whether the caller will only act on a named turn. In that case, with a
    /// detector, a turn it did not wake on is never transcribed.
    pub fn set_needs_name(&mut self, needs: bool) {
        self.needs_name = needs;
    }

    /// The detector fired during the turn whose speech began at `from`.
    ///
    /// A wake before the first speech of the turn is a false one on the room's
    /// noise, and must not name the sentence that follows it.
    fn woke_since(&self, from: usize) -> bool {
        let from = from.saturating_sub(crate::wav::SAMPLE_RATE as usize / 5);
        self.wakes.iter().any(|&at| at >= from)
    }

    /// No transcription is worth writing: the caller wants a name, the
    /// detector is the judge of it, and it has not fired.
    fn skips_asr(&self) -> bool {
        self.needs_name && self.wake.is_some() && self.wakes.is_empty()
    }

    /// Whether this chain writes transcripts, or hands over sound.
    pub fn transcribes(&self) -> bool {
        self.asr.is_some()
    }

    /// Whether Parakeet sits on the other side of the GPU switch from where
    /// it was loaded — the voice thread then reloads it. A listener has no
    /// Parakeet, so it never does.
    pub fn misplaced(&self) -> bool {
        self.asr.is_some() && self.asr_on_gpu != crate::asr::gpu()
    }

    /// Ask both models one question nobody is waiting for.
    ///
    /// # Why having loaded them is not the same as being ready
    ///
    /// Loading builds the graph; it does not run it. ONNX Runtime leaves part
    /// of its work to the **first `run`** — allocating the memory arena, and,
    /// for a 2.4 GB encoder whose weights live in an external
    /// `encoder-model.onnx.data`, actually reading them in. Until this
    /// existed, that bill was paid by **the user's first sentence**, after the
    /// loading bar had already said everything was ready.
    ///
    /// So it is paid here instead, behind [`Load::Hearing`], where there is
    /// already a bar on screen and nobody is waiting on an answer.
    ///
    /// A second of quiet is enough: what is being exercised is the runtime, not
    /// the model's judgement, and the answers are thrown away. A failure is
    /// **not** fatal — a model that loaded and cannot be warmed is still a
    /// model that works, and refusing to listen over a discarded result would
    /// be the wrong trade.
    fn warm_up(&mut self) {
        let started = std::time::Instant::now();
        let quiet = vec![0f32; crate::wav::SAMPLE_RATE as usize];
        if let Err(e) = self.detector.prosody(&quiet) {
            tracing::warn!("le détecteur de tour n'a pas pu être préchauffé : {e:#}");
        }
        if let Some(asr) = self.asr.as_mut()
            && let Err(e) = asr.transcribe_samples(quiet, crate::wav::SAMPLE_RATE, 1, None)
        {
            tracing::warn!("l'ASR n'a pas pu être préchauffé : {e}");
        }
        tracing::info!(
            ms = started.elapsed().as_millis(),
            "modèles préchauffés : la première phrase ne paiera pas la première inférence"
        );
    }

    /// Feed captured samples. 16 kHz mono, any length.
    ///
    /// Returns once per pause the gate finds. Costs nothing between pauses: the
    /// models are only asked a question where there is one to answer.
    pub fn push(&mut self, samples: &[f32]) -> Result<Option<Heard>> {
        self.pending.extend_from_slice(samples);
        let mut heard = None;
        while self.pending.len() >= FRAME {
            let frame: Vec<f32> = self.pending.drain(..FRAME).collect();
            self.stream.extend_from_slice(&frame);
            // Before the segmenter, so a wake at the very end of a segment is
            // known when that segment is judged.
            if let Some(wake) = self.wake.as_mut()
                && wake.push(&frame)?
            {
                tracing::debug!(at = self.stream.len(), "nom entendu par le détecteur");
                self.wakes.push(self.stream.len());
            }
            match self.segmenter.push(&frame) {
                Some(segment) => heard = Some(self.judge(segment)?),
                // No segment closed. If this is the frame where the speaker
                // went quiet, the transcription starts now rather than in
                // 300 ms. See `speculate`.
                None => {
                    self.speculate()?;
                    if let Some(over) = self.wait_out() {
                        heard = Some(over);
                    }
                }
            }
        }
        self.trim();
        Ok(heard)
    }

    /// The turn was handed off. Start a fresh one.
    pub fn end_turn(&mut self) {
        self.stream.clear();
        self.pending.clear();
        self.speculated = None;
        self.turn_from = None;
        self.wakes.clear();
        self.held = None;
        self.segmenter = gate::Segmenter::new(self.cal);
    }

    /// Change what ARIA answers to, without reloading 2.4 GB.
    pub fn set_name(&mut self, name: Name) {
        self.name = name;
    }

    /// Change the thresholds. The segmenter restarts, because its counters are
    /// derived from them — applying new numbers to an in-flight segment would
    /// mix two calibrations in one decision.
    pub fn set_calibration(&mut self, cal: Calibration) {
        self.cal = cal;
        self.segmenter = gate::Segmenter::new(cal);
        // And the guess with it: it was padded with the old `silence_ms`, so
        // spending it under the new one would be one calibration answering a
        // question asked by another.
        self.speculated = None;
    }

    /// Write the transcript at the **first** quiet frame instead of waiting for
    /// the silence to be confirmed.
    ///
    /// # The dead time this spends
    ///
    /// The gate needs [`Calibration::silence_ms`] of quiet — 300 ms by default
    /// — before it will call a pause a pause. Transcription costs 200 to
    /// 300 ms. Run one after the other, the second starts when the first ends
    /// and the user waits for both. Run together, the second is free: by the
    /// time the gate has its silence, the text is already in hand.
    ///
    /// Nothing is risked by being early, because **the words cannot change**.
    /// What sits between the first quiet frame and the confirmed one is silence
    /// by definition — that is what the gate spent those frames establishing.
    /// If speech resumes instead, the guess is dropped and a quarter of a
    /// second of a core was wasted, which is the whole downside.
    ///
    /// This is the same move Kyutai's own stack makes in `unmute`: on detecting
    /// a pause it pushes silence into its streaming ASR to force out the words
    /// still held by the model's lookahead, rather than letting real time
    /// deliver them. Different mechanism, same refusal — **a pipeline must not
    /// idle waiting for a clock.**
    ///
    /// # What it costs the loop
    ///
    /// The transcription runs here, on the voice thread, synchronously. For
    /// those 250 ms captured frames pile up in the channel rather than being
    /// consumed — none are lost, the sink never blocks — so the energy gate
    /// sees them late and a barge-in starting inside that window is noticed up
    /// to 250 ms late. That is the same 250 ms being saved at the other end,
    /// and it buys the simplicity of not sharing the model across threads.
    /// The quiet frame at which the guess is taken, and **why it is not the
    /// first one**.
    ///
    /// Measured on 2026-11-03 over the twenty-three calibration samples, by
    /// transcribing each one at every quiet rank and comparing against the
    /// confirmed call. Taking the guess at the first quiet frame rewrote
    /// **eight transcripts out of twenty-three** — not punctuation, words:
    /// « Donne-moi la l. » for « Donne-moi la liste. », « Ariad dans VScotte »
    /// for « Aria dans VS Code ». The count only reaches zero at rank 13.
    ///
    /// The cause is not the padding, it is the truncation. A word's last
    /// phonemes fall **below `speech_rms` while still being speech** — the
    /// release of a stop, the tail of a fricative — so a single quiet frame
    /// is not the end of the word, it is the middle of one. That is the very
    /// reason the gate wants [`gate::Segmenter::quiet_needed`] frames before
    /// it calls a pause a pause, and no amount of invented padding can put
    /// back audio that was never read.
    ///
    /// So the figure is a **duration**, not a fraction of the threshold: the
    /// tail lasts what it lasts, whatever `silence_ms` is set to. If the gate
    /// asks for less quiet than this, the guess simply never fires.
    const SPECULATE_AFTER_MS: usize = 260;

    fn speculate_after(&self) -> usize {
        let frames = Self::SPECULATE_AFTER_MS / 20;
        frames.min(self.segmenter.quiet_needed().saturating_sub(1)).max(1)
    }

    fn speculate(&mut self) -> Result<()> {
        if self.asr.is_none() || self.skips_asr() {
            // Nothing to write early when nothing is written at all.
            return Ok(());
        }
        match self.segmenter.quiet_run() {
            // Speech, or no turn in progress. Either way whatever was guessed
            // belongs to a pause that is not going to happen.
            Some(0) | None => {
                self.speculated = None;
                Ok(())
            }
            // Late enough that the word is really over -- see
            // [`Engine::speculate_after`]. One quiet frame meant nothing.
            Some(k) if k == self.speculate_after() => {
                let voiced_to = self.segmenter.position();
                let text = self.transcribe(self.padded(voiced_to))?;
                self.speculated = Some(Guess { voiced_to, text });
                Ok(())
            }
            // Still quiet, already guessed. A second call would transcribe the
            // same audio for the same answer.
            Some(_) => Ok(()),
        }
    }

    /// `stream[..to]`, followed by the silence that is about to be there.
    ///
    /// The padding is **a repeat of the last frame**, not zeros, and that is
    /// not fussiness. A closed segment keeps the silence that closed it, so the
    /// confirmed call hears 300 ms of the room's own noise floor at the end;
    /// digital zero is not that. Parakeet punctuates from context and the
    /// terminal mark is half of the end-of-turn decision
    /// ([`crate::turn::decide`]) — so if the two calls are handed different
    /// endings, the fast one is not the same answer arriving sooner, it is a
    /// different answer. Repeating the observed frame is the cheapest way to
    /// hand it the ending it would have had.
    fn padded(&self, to: usize) -> Vec<f32> {
        let mut audio = self.stream[..to].to_vec();
        let missing = self.segmenter.quiet_needed().saturating_sub(1);
        let tail = &self.stream[to.saturating_sub(FRAME)..to];
        for _ in 0..missing {
            audio.extend_from_slice(tail);
        }
        audio
    }

    /// One call to the ASR, in one place.
    fn transcribe(&mut self, audio: Vec<f32>) -> Result<String> {
        let Some(asr) = self.asr.as_mut() else {
            return Ok(String::new());
        };
        Ok(asr
            .transcribe_samples(audio, crate::wav::SAMPLE_RATE, 1, None)
            .map_err(|e| anyhow!("transcription failed: {e}"))?
            .text
            .trim()
            .to_string())
    }

    fn judge(&mut self, segment: gate::Segment) -> Result<Heard> {
        // A new pause supersedes a held one: it judges the whole turn again.
        self.held = None;
        let end = segment.to.min(self.stream.len());

        // The last 8 s of the **stream** ending at the pause, never the segment
        // on its own: an isolated phrase leaves six seconds of zeros after it,
        // so the model sees speech that stopped long ago and answers
        // "finished". That is how three genuine cut-offs scored 0.97.
        let prosody = self
            .detector
            .prosody(turn::Detector::window(&self.stream, end))?;

        // The whole turn so far, not the segment: the punctuation of a sentence
        // depends on what came before it, and the name may have been said in an
        // earlier breath.
        //
        // Usually already written, at the first quiet frame of this very
        // pause — see `speculate`. Spending it here is what makes the gate's
        // 300 ms of silence cost nothing. `take` because a guess is good for
        // exactly one pause.
        // A listener has no words: the turn is the sound, and prosody alone
        // decides whether it is over.
        let spoke_from = *self.turn_from.get_or_insert(segment.from);
        let woke = self.woke_since(spoke_from);
        // Hybrid, and the detector did not hear the name: the turn is not
        // transcribed at all, so the ASR no longer runs on every sentence said
        // in the room. Prosody alone ends it, as for a listener.
        if self.asr.is_none() || (self.needs_name && self.wake.is_some() && !woke) {
            // From the first speech of the turn, not from when the turn was
            // opened: the stream also holds the silence before anyone spoke,
            // and every second of it is prefill the model pays for. A fifth of
            // a second ahead is kept, for an onset the gate caught late.
            let from = spoke_from.saturating_sub(crate::wav::SAMPLE_RATE as usize / 5);
            let decided = if prosody >= self.cal.turn_threshold {
                turn::Turn::Over
            } else {
                turn::Turn::Continuing
            };
            let listener = self.asr.is_none();
            return Ok(Heard {
                transcript: String::new(),
                turn: decided,
                prosody,
                // Without words the detector decides alone — `confirm` has
                // nothing to confirm with.
                addressed: listener && woke,
                sleep_request: false,
                address_only: false,
                at_seconds: end as f32 / crate::wav::SAMPLE_RATE as f32,
                audio: (listener && decided == turn::Turn::Over)
                    .then(|| self.stream[from.min(end)..end].to_vec()),
            });
        }

        let transcript = match self.speculated.take() {
            Some(guess) => {
                tracing::trace!(voiced_to = guess.voiced_to, "transcription déjà écrite");
                guess.text
            }
            None => self.transcribe(self.stream[..end].to_vec())?,
        };

        let decided = turn::decide(prosody, self.cal.turn_threshold, &transcript);
        let spoken_ms = (end.saturating_sub(spoke_from) * 1000 / crate::wav::SAMPLE_RATE as usize) as u64;
        let patience = match decided {
            turn::Turn::Over => turn::patience(&transcript, spoken_ms),
            turn::Turn::Continuing => std::time::Duration::ZERO,
        };
        // With a detector, the sound says whether the name was said — alone by
        // default since 2026-11-23 (sensitivity first, the transcript hears
        // « A rien »); with `confirm` on, the transcript must agree. Without a
        // detector, the transcript alone.
        let addressed = match self.wake {
            Some(_) if self.wake_settings.confirm => woke && self.name.addressed(&transcript),
            Some(_) => woke,
            None => self.name.addressed(&transcript),
        };
        let heard = Heard {
            audio: None,
            addressed,
            sleep_request: decided == turn::Turn::Over
                && name::asks_for_sleep(&transcript, &self.name),
            address_only: decided == turn::Turn::Over && self.name.is_address_only(&transcript),
            turn: decided,
            prosody,
            at_seconds: end as f32 / crate::wav::SAMPLE_RATE as f32,
            transcript,
        };
        if patience.is_zero() {
            return Ok(heard);
        }
        // Over, but not yet: shown as a partial now, handed on as over once the
        // extra silence has passed — or dropped if the person speaks again,
        // and then the next pause judges the whole turn afresh.
        tracing::debug!(
            ms = patience.as_millis() as u64,
            spoken_ms,
            "tour fini en apparence : un peu plus de silence avant de répondre"
        );
        let partial = Heard {
            turn: turn::Turn::Continuing,
            sleep_request: false,
            address_only: false,
            ..heard.clone()
        };
        self.held = Some(Held {
            heard,
            frames_left: Calibration::frames(patience.as_millis() as u64).max(1),
        });
        Ok(partial)
    }

    /// Count down a held turn's extra silence; hand it on when it is spent.
    ///
    /// Called on every frame that closed no segment. A segment open again is
    /// the person speaking: the hold is dropped, and the next pause judges the
    /// whole turn with what they added.
    fn wait_out(&mut self) -> Option<Heard> {
        let held = self.held.as_mut()?;
        if self.segmenter.quiet_run().is_some() {
            tracing::debug!("la parole reprend : le tour retenu continue");
            self.held = None;
            return None;
        }
        held.frames_left = held.frames_left.saturating_sub(1);
        if held.frames_left > 0 {
            return None;
        }
        self.held.take().map(|held| held.heard)
    }

    /// Keep the buffer bounded without disturbing the segmenter's clock.
    ///
    /// Dropping from the front would shift every offset the segmenter has
    /// already issued, so the turn is simply restarted — which is the honest
    /// behaviour anyway: a minute without a single closed segment is not a
    /// turn.
    ///
    /// The message used to say "no pause in a whole turn's worth of audio",
    /// which reads as *continuous sound* and sent a diagnosis in exactly the
    /// wrong direction. This fires on an **empty** minute just as readily: the
    /// stream grows with silence too, and a room where nobody speaks reaches a
    /// minute on the dot, every minute. The two are told apart below.
    fn trim(&mut self) {
        // Nothing of a turn yet: no segment open, none judged, none held, no
        // name heard. Every offset is then in the segmenter alone, and a fresh
        // one is the same clock started again — nothing is shifted.
        let in_flight = self.turn_from.is_some()
            || self.held.is_some()
            || self.segmenter.quiet_run().is_some()
            || !self.wakes.is_empty()
            || self.speculated.is_some();
        if idle_overflow(self.stream.len(), in_flight) {
            self.end_turn();
            return;
        }
        if self.stream.len() > MAX_TURN_SECONDS * crate::wav::SAMPLE_RATE as usize {
            // The cheap discriminator, and it is worth the one pass: a minute
            // of silence and a minute of unbroken noise produce the same
            // symptom and have opposite causes — a threshold too high, or one
            // too low.
            let loud = self
                .stream
                .chunks(crate::calibration::FRAME)
                .filter(|frame| {
                    frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32
                        > self.cal.speech_rms * self.cal.speech_rms
                })
                .count();
            let frames = self.stream.len() / crate::calibration::FRAME;
            tracing::warn!(
                seconds = MAX_TURN_SECONDS,
                loud_frames = loud,
                frames,
                threshold = self.cal.speech_rms,
                "une minute sans aucun segment fermé — le tour redémarre. \
                 Peu de trames au-dessus du seuil : personne n'a parlé, ou le seuil est trop \
                 haut. Beaucoup : le seuil est trop bas et la porte ne se referme jamais."
            );
            self.end_turn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The silence before a request no longer eats its minute; the words of a
    /// turn are never dropped for it.
    #[test]
    fn silence_before_the_first_word_does_not_count() {
        let second = crate::wav::SAMPLE_RATE as usize;
        assert!(!idle_overflow(IDLE_SECONDS * second, false));
        assert!(idle_overflow(IDLE_SECONDS * second + 1, false));
        assert!(!idle_overflow(50 * second, true), "a turn in progress keeps its audio");
    }

    /// The measurement [`Engine::warm_up`] rests on: how much of the first
    /// inference is the runtime waking up rather than the model thinking.
    ///
    /// `#[ignore]` because it reads 2.4 GB from disk, which a machine without
    /// the model cannot do and which nobody wants in a normal `cargo test`.
    ///
    /// ```sh
    /// cargo test -p aria-voice --lib engine -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "loads the 2.4 GB ASR model and the turn detector from disk"]
    fn the_first_inference_costs_more_than_the_next_ones() {
        let mut detector =
            turn::Detector::load(&turn::Detector::default_model_path()).expect("the turn detector");
        let mut asr = crate::asr::load(&crate::asr::default_model_dir()).expect("the ASR model");

        // A second of quiet, which is what the warm-up itself uses. What is
        // timed is the runtime's first run, not the answer.
        let quiet = vec![0f32; crate::wav::SAMPLE_RATE as usize];

        let mut turn_ms = Vec::new();
        let mut asr_ms = Vec::new();
        for _ in 0..3 {
            let at = Instant::now();
            detector.prosody(&quiet).expect("prosody");
            turn_ms.push(at.elapsed().as_millis());

            let at = Instant::now();
            asr.transcribe_samples(quiet.clone(), crate::wav::SAMPLE_RATE, 1, None)
                .expect("transcription");
            asr_ms.push(at.elapsed().as_millis());
        }

        println!("détecteur de tour  {turn_ms:?} ms");
        println!("ASR                {asr_ms:?} ms");
        println!(
            "économisé sur la première phrase : {} ms",
            (turn_ms[0] + asr_ms[0]).saturating_sub(turn_ms[2] + asr_ms[2])
        );
    }

    /// The one risk the speculation carries, written as the test that catches
    /// it.
    ///
    /// [`Engine::speculate`] hands the ASR the turn up to the first quiet
    /// frame plus a repeat of that frame. [`Engine::judge`] would have handed
    /// it the same turn plus the room's own silence. The **words** cannot
    /// differ — what lies between the two is silence, which is precisely what
    /// the gate spent those frames establishing.
    ///
    /// The **punctuation** can, and that is not cosmetic: the terminal mark is
    /// half of [`turn::decide`], so a guess that drops a full stop turns a
    /// finished turn into a continuing one and ARIA sits waiting for a
    /// sentence that already ended. So the assertion is on
    /// [`turn::looks_unfinished`] agreeing, not on the strings matching — a
    /// comma that moves changes nothing, a full stop that vanishes changes
    /// everything. Text differences are printed rather than failed, because
    /// they are worth seeing and not worth blocking on.
    ///
    /// `#[ignore]` because it reads the 2.4 GB model, which is this
    /// repository's rule for every test that does.
    ///
    /// ```sh
    /// cargo test -p aria-voice --lib speculating -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "loads the 2.4 GB ASR model and reads the calibration samples"]
    fn speculating_early_writes_the_same_sentence() {
        let cal = Calibration::default();
        let mut asr = crate::asr::load(&crate::asr::default_model_dir()).expect("the ASR model");
        let quiet_needed = gate::Segmenter::new(cal).quiet_needed();
        let rank = (Engine::SPECULATE_AFTER_MS / 20)
            .min(quiet_needed.saturating_sub(1))
            .max(1);

        let dir = crate::samples_dir();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("no samples in {}: {e}", dir.display()))
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|x| x == "wav"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no .wav in {}", dir.display());

        let mut checked = 0usize;
        let mut disagreed = Vec::new();
        for path in &files {
            let samples = crate::wav::read(path).expect("a readable wav");

            // The same walk the live loop makes, including voiding the guess
            // when speech resumes — a breath one frame into a gap must not be
            // spent at the next pause.
            let mut segmenter = gate::Segmenter::new(cal);
            let mut voiced_to: Option<usize> = None;
            let mut closed = None;
            for frame in samples.chunks(FRAME) {
                let segment = segmenter.push(frame);
                match segmenter.quiet_run() {
                    Some(0) => voiced_to = None,
                    Some(k) if k == rank => voiced_to = Some(segmenter.position()),
                    _ => {}
                }
                if segment.is_some() {
                    closed = segment;
                    break;
                }
            }
            let (Some(voiced_to), Some(segment)) = (voiced_to, closed) else {
                continue;
            };

            let mut early_audio = samples[..voiced_to].to_vec();
            let tail = &samples[voiced_to.saturating_sub(FRAME)..voiced_to];
            for _ in 0..quiet_needed.saturating_sub(rank) {
                early_audio.extend_from_slice(tail);
            }
            let end = segment.to.min(samples.len());

            let early = say(&mut asr, early_audio);
            let confirmed = say(&mut asr, samples[..end].to_vec());
            checked += 1;

            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if early != confirmed {
                println!("{name}\n  tôt      : {early}\n  confirmé : {confirmed}");
            }
            // The criterion is **the text**, not just the end-of-turn
            // decision. The weaker one let four samples through at rank 1
            // while eight had actually been rewritten: the early transcript
            // is what gets sent to the agent, so a word lost there is not a
            // latency trade-off, it is a wrong sentence.
            if early != confirmed {
                disagreed.push(format!("{name}: « {early} » vs « {confirmed} »"));
            }
        }

        println!("{checked} échantillons, {} désaccords", disagreed.len());
        assert!(
            disagreed.is_empty(),
            "the early transcript is not the confirmed one:\n{}",
            disagreed.join("\n")
        );
        assert!(checked > 0, "no sample produced a pause to speculate on");
    }

    fn say(asr: &mut parakeet_rs::ParakeetTDT, audio: Vec<f32>) -> String {
        asr.transcribe_samples(audio, crate::wav::SAMPLE_RATE, 1, None)
            .expect("transcription")
            .text
            .trim()
            .to_string()
    }
}
