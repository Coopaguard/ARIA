//! Where the voice chain is married to the core.
//!
//! The two halves are kept apart on purpose and meet only here. `aria-core`
//! does not depend on `aria-voice` — that would drag an ONNX runtime into the
//! crate whose whole point is building without one — so **the chain pushes in
//! and the core never reaches out**. This module is the push.
//!
//! | What was heard | What the core is told |
//! |---|---|
//! | the name, early in the utterance | `ModeTrigger::NameHeard` |
//! | a complete turn asking for quiet | `ModeTrigger::SpokenSleep` |
//! | a transcript | `push_transcript(text, final)` |
//! | a turn that ended | `turn_processed()` |
//!
//! # Three shapes that are not negotiable
//!
//! **The ASR never runs on the capture thread.** `handy_recorder::Sink` is
//! called on the delivery thread and must return promptly — a slow sink drops
//! audio, one that blocks fails the recorder outright. Transcription costs 200
//! to 300 ms. So the sink does nothing but hand the samples to a channel.
//!
//! **In hybrid, a transcript that does not name ARIA is not published.** The
//! text never leaves the machine either way, but decision 19 makes named-only a
//! privacy guarantee rather than mere silence, and a window quietly printing
//! everything said in the room is not that guarantee.
//!
//! **Sleep is checked before the turn is handed over.** "Merci Aria" ends the
//! exchange; it is not a request to act on.

pub mod calibration;
pub mod speaking;

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use aria_core::Core;
use aria_core::audio::{AudioMode, Ear, Load, ModeTrigger, VoiceState};
use aria_voice::calibration::{Calibration, FRAME};
use aria_voice::echo;
use aria_voice::engine::{Engine, Heard};
use aria_voice::handy_recorder::{self, AudioChunk, Recorder, RecorderConfig, Sink};
use aria_voice::name::Name;
use aria_voice::tts::{Playing, Silencer};
use aria_voice::turn::Turn;

/// How often the thread looks again while listening is switched off.
///
/// A poll rather than a subscription, and that is a deliberate trade: four
/// mutex reads a second against a second place where a missed event means the
/// microphone never comes back on.
const OFF_POLL: Duration = Duration::from_millis(250);

/// What keys the calibration: the input device that will actually be used.
///
/// Read before the chain starts rather than guessed afterwards, because
/// decision 38 stores the thresholds under it.
///
/// Falls back to the device **name** when the backend says its identifier is
/// not stable. An identifier that changes between reboots would lose the
/// calibration every time, silently, and the symptom — "it was fine
/// yesterday" — points nowhere near the cause.
pub fn default_input_id() -> String {
    let Ok(devices) = handy_recorder::list_input_devices() else {
        return "unknown".into();
    };
    devices
        .into_iter()
        .find(|d| d.is_default)
        .map(|d| if d.id_is_stable { d.id } else { d.name })
        .unwrap_or_else(|| "unknown".into())
}

/// How loud the microphone is, for the orb to draw.
///
/// Not the energy gate's reading and not a copy of it: the gate answers "is
/// this speech", in absolute RMS against a calibrated threshold, which is a
/// number that barely moves. This answers "how loud, as a person would
/// judge it" — and the orb needs the second one, because a meter that sits at
/// 0.01 whatever you do is a meter nobody believes.
///
/// So: RMS mapped through a floor and a ceiling chosen for the eye, then
/// smoothed asymmetrically — fast up, slow down. A meter that falls as fast
/// as it rises flickers on every consonant; one that falls slowly reads as a
/// voice.
#[derive(Default)]
struct Meter {
    level: f32,
}

impl Meter {
    /// The audible floor and ceiling, in RMS.
    ///
    /// Below the floor is a quiet room; the ceiling is a voice at a normal
    /// distance. Deliberately **not** the calibrated speech threshold: that
    /// one is about deciding, this one is about showing.
    const FLOOR: f32 = 0.004;
    const CEILING: f32 = 0.12;

    /// One reading, already attributed — see [`heard_rms`]. The meter shows
    /// the user, so it is fed **after** the echo gate, never the raw capture.
    fn push_rms(&mut self, rms: f32) -> f32 {
        // Logarithmic, because loudness is: a linear meter spends its whole
        // range on the loudest tenth and sits at zero for ordinary speech.
        let target = if rms <= Self::FLOOR {
            0.0
        } else {
            ((rms / Self::FLOOR).ln() / (Self::CEILING / Self::FLOOR).ln()).clamp(0.0, 1.0)
        };
        let rate = if target > self.level { 0.55 } else { 0.12 };
        self.level += (target - self.level) * rate;
        self.level
    }
}

/// How much of one frame is the **user**, for the orb's meter.
///
/// Asked for on 2026-11-17: the meter has to come *after* the echo gate. It was
/// fed the raw capture, so while ARIA spoke the orb's listening parts — the
/// ticks and the pupil — moved to ARIA's own voice coming back through the
/// room, and the two animations the orb now separates were joined again by the
/// microphone.
///
/// We do not cancel echo (decision 17), we **attribute** it, and the gate
/// already does that per 20 ms frame while ARIA speaks. So:
///
/// - ARIA silent: the frame is all the user's, as before;
/// - ARIA speaking: only a frame the gate calls [`echo::Verdict::Barge`] counts.
///   An echo, a quiet frame, and a frame the gate cannot judge yet
///   (`Unresolved`, the first two seconds of an answer) all show as nothing —
///   the meter would rather miss the start of an interruption than draw ARIA's
///   voice as the user's.
fn heard_rms(rms: f32, speaking: bool, verdict: Option<echo::Verdict>) -> f32 {
    if !speaking {
        return rms;
    }
    match verdict {
        Some(echo::Verdict::Barge) => rms,
        _ => 0.0,
    }
}

/// The fast half of barge-in: the energy envelope, frame by frame.
///
/// # Why there are two halves
///
/// Decision 17 asks for playback to stop *inside the frame*. Until this
/// existed the only signal was [`VoiceState::Listening`], published by [`act`]
/// — which only runs on a **closed segment**. So the person interrupting had
/// to finish their phrase *and* stay quiet for `silence_ms` before ARIA
/// noticed; somebody talking continuously for two seconds was talked over for
/// two seconds. The comment in [`speaking`] claimed the opposite, which is
/// roughly how it went unnoticed.
///
/// So, as in `unmute`, there are now two independent paths to an
/// interruption: this one, acoustic and immediate, and the transcript at the
/// next pause, slower and certain. `unmute` can afford to cut on energy alone
/// because a browser hands it an echo-cancelled microphone. Nobody hands us
/// one, so the verdict goes through [`echo::Live`], which refuses to cut
/// anything until it has established what the room does with ARIA's own voice.
struct Barge {
    gate: echo::Live,
    /// Samples that do not yet make a whole frame.
    pending: Vec<f32>,
    /// What an echo frame is turned down to before the ear gets it. See
    /// [`Barge::feed`].
    hush_rms: f32,
    /// What the gate saw while ARIA was speaking, logged when she stops.
    stretch: Stretch,
    /// The previous frame, so [`voicing`] sees 40 ms: one period of a low
    /// voice does not fit in 20 with room to compare.
    previous: Vec<f32>,
    /// Consecutive frames that are both [`echo::Verdict::Barge`] **and**
    /// voiced. This, not the gate's own count, is what cuts.
    run: usize,
}

/// What the gate judged during one stretch of ARIA speaking.
///
/// Logged at `info` when the stretch ends, because the one failure here is
/// silent: a voice that never clears `echo_margin_db` is simply never heard
/// as an interruption, and ARIA talks on until the user's sentence ends. The
/// numbers say by how much it missed — enough to set the margin for this room,
/// or to show that only real echo cancellation will do (2026-11-23).
#[derive(Default)]
struct Stretch {
    frames: usize,
    unjudged: usize,
    echo: usize,
    barge: usize,
    /// dB above the predicted echo, for every frame loud enough to judge.
    excess: Vec<f32>,
    cut: bool,
}

impl Stretch {
    fn note(&mut self, verdict: Option<echo::Verdict>, excess: Option<f32>) {
        self.frames += 1;
        match verdict {
            None => self.unjudged += 1,
            Some(echo::Verdict::Echo) => self.echo += 1,
            Some(echo::Verdict::Barge) => self.barge += 1,
            Some(echo::Verdict::Quiet) => {}
        }
        self.excess.extend(excess);
    }

    /// Say what the stretch looked like, and start a new one.
    fn close(&mut self, coupling: echo::Coupling, margin_db: f32) {
        let mut done = std::mem::take(self);
        // Under a quarter of a second is a waiting phrase or a stray frame.
        if done.frames < 12 {
            return;
        }
        done.excess.sort_by(f32::total_cmp);
        let at = |q: f32| {
            (!done.excess.is_empty())
                .then(|| done.excess[((done.excess.len() - 1) as f32 * q) as usize])
        };
        tracing::info!(
            ?coupling,
            margin_db,
            ms = done.frames * 20,
            unjudged = done.unjudged,
            echo = done.echo,
            barge = done.barge,
            cut = done.cut,
            excess_median = ?at(0.5),
            excess_p90 = ?at(0.9),
            excess_max = ?at(1.0),
            "porte d'écho, fin de phrase d'ARIA"
        );
    }
}

impl Barge {
    fn new(cal: Calibration) -> Self {
        Self {
            gate: echo::Live::for_calibration(cal),
            pending: Vec::with_capacity(FRAME * 2),
            // A quarter of the speech floor: well under what opens a segment,
            // and still a room rather than digital silence — the ASR
            // punctuates from context, and a run of exact zeros is not one
            // (decision 81).
            hush_rms: cal.speech_rms * 0.25,
            stretch: Stretch::default(),
            previous: vec![0.0; FRAME],
            run: 0,
        }
    }

    fn coupling(&self) -> echo::Coupling {
        self.gate.coupling()
    }

    /// Feed one capture chunk. `cut` is true on the frame that decides;
    /// `heard` is the loudest frame's share that is the user's, for the meter
    /// (see [`heard_rms`]), and `None` when the chunk closed no whole frame.
    ///
    /// The reference is read **per frame** rather than once per chunk. It is a
    /// clone of four hundred floats behind one uncontended lock, fifty times a
    /// second, and reading it once per chunk would leave the last frames of a
    /// chunk compared against a reference that stopped earlier — a drift of a
    /// few frames inside the very window the delay search is trying to measure.
    ///
    /// # The ear gets the gate's version of the sound
    ///
    /// `fed.samples` is what goes on to the segmenter and the ASR: the same
    /// frames, except those the gate calls [`echo::Verdict::Echo`], turned down
    /// under the speech floor. Until 2026-11-23 the ear heard the raw
    /// microphone, so ARIA's voice opened a segment, the user's reply extended
    /// it, and one transcript carried both — « Vous voudrez que je vois un
    /// grand et maman comme votre ami… Non, je veux un âne en personnage
    /// principal. » The word veto cannot catch that: it only throws away a turn
    /// that is *all* echo, and this one was half the user's.
    ///
    /// Only `Echo` is touched. `Barge` is the user and passes whole; a frame
    /// the gate cannot judge yet (`Unresolved`) passes too, and the word veto
    /// stays the defence there. With headphones the coupling is `Absent`, no
    /// frame is ever `Echo`, and nothing changes.
    fn feed(&mut self, samples: &[f32], speaker: &Silencer) -> Fed {
        self.pending.extend_from_slice(samples);
        let mut fed = Fed::default();
        while self.pending.len() >= FRAME {
            let mut frame: Vec<f32> = self.pending.drain(..FRAME).collect();
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
            let speaking = speaker.state() == Playing::Speaking;
            // The gate's own "cut" is energy alone; it is read as a verdict
            // per frame and counted here, on voiced frames only.
            self.gate.push(rms, &speaker.reference_envelope(), speaking);
            let verdict = self.gate.verdict();
            let voiced = verdict == Some(echo::Verdict::Barge) && {
                let mut window = std::mem::take(&mut self.previous);
                window.extend_from_slice(&frame);
                let score = voicing(&window);
                self.previous = window.split_off(FRAME);
                if score < VOICED {
                    tracing::trace!(score, "énergie sans voix (souffle ?) : pas une interruption");
                }
                score >= VOICED
            };
            if !verdict.is_some_and(|v| v == echo::Verdict::Barge) {
                self.previous.clear();
                self.previous.extend_from_slice(&frame);
            }
            self.run = if voiced { self.run + 1 } else { 0 };
            let cut = self.run == echo::BARGE_FRAMES;
            fed.cut |= cut;
            if speaking {
                self.stretch.note(verdict, self.gate.excess_db());
                self.stretch.cut |= cut;
            } else if self.stretch.frames > 0 {
                self.stretch.close(self.gate.coupling(), self.gate.margin_db());
            }
            let mine = heard_rms(rms, speaking, verdict);
            fed.heard = Some(fed.heard.map_or(mine, |held: f32| held.max(mine)));
            if let Some(scale) = hush_scale(rms, speaking, verdict, self.hush_rms) {
                frame.iter_mut().for_each(|s| *s *= scale);
                fed.hushed += 1;
            }
            fed.samples.extend_from_slice(&frame);
        }
        fed
    }
}

/// How periodic a stretch of sound is: the best normalised autocorrelation
/// peak over the lags of a speaking voice, 0 for noise, near 1 for a vowel.
///
/// # Why the barge-in needs it
///
/// The gate reasons on energy, and energy the loudspeaker does not explain is
/// not always a voice. Breathing out vape near the microphone is a loud,
/// broadband hiss the reference cannot account for, and it cut ARIA off
/// (2026-11-23). A voice has a pitch; a breath does not.
///
/// Three choices, each against a specific false reading:
///
/// - **a DC blocker**, cutting under about 50 Hz, for the low rumble of air on
///   the capsule. Not pre-emphasis: that was tried first, and it flattened the
///   voice's own fundamental while keeping the hiss — a voice with a breath
///   over it then scored as noise;
/// - **lags of 32 to 200 samples**, 500 Hz down to 80 Hz at 16 kHz, the range
///   of a speaking voice, child to low male;
/// - **a peak, not a maximum**: the winning lag must beat both neighbours. A
///   slow wave decreases steadily across the range and would otherwise score
///   at its first lag without being a pitch at all.
fn voicing(window: &[f32]) -> f32 {
    const MIN_LAG: usize = 32;
    const MAX_LAG: usize = 200;
    if window.len() < MAX_LAG * 2 {
        return 0.0;
    }
    // A DC blocker, `y[n] = x[n] - x[n-1] + 0.98·y[n-1]`: -3 dB near 50 Hz,
    // flat over a voice's fundamental.
    let mut previous = (window[0], 0.0f32);
    let x: Vec<f32> = window[1..]
        .iter()
        .map(|&s| {
            let y = s - previous.0 + 0.98 * previous.1;
            previous = (s, y);
            y
        })
        .collect();
    let r = |lag: usize| -> f32 {
        let (a, b) = (&x[..x.len() - lag], &x[lag..]);
        let num: f32 = a.iter().zip(b).map(|(p, q)| p * q).sum();
        let den = (a.iter().map(|p| p * p).sum::<f32>() * b.iter().map(|q| q * q).sum::<f32>()).sqrt();
        if den > 0.0 { num / den } else { 0.0 }
    };
    let scores: Vec<f32> = (MIN_LAG - 1..=MAX_LAG + 1).map(r).collect();
    scores
        .windows(3)
        .filter(|w| w[1] > w[0] && w[1] >= w[2])
        .map(|w| w[1])
        .fold(0.0, f32::max)
}

/// Whether a transcript holds something said, rather than a noise the ASR
/// wrote down: at least one word that is not a bare interjection. Parakeet
/// turns a breath into « Hm. » or « Ah. » as readily as into nothing.
fn has_words(transcript: &str) -> bool {
    const NOISES: [&str; 8] = ["hm", "hmm", "mh", "ah", "oh", "euh", "pff", "ha"];
    transcript
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| !NOISES.contains(&w.to_lowercase().as_str()))
}

/// Above this, a frame is a voice rather than a breath. See [`voicing`].
const VOICED: f32 = 0.45;

/// How much to turn a frame down before the ear hears it, if at all.
///
/// Only a frame the gate calls ARIA's own voice, while she is speaking, and
/// only when it is louder than `hush_rms`. Everything else — the user, a frame
/// not yet judged, a room with no echo — passes untouched. See [`Barge::feed`].
fn hush_scale(rms: f32, speaking: bool, verdict: Option<echo::Verdict>, hush_rms: f32) -> Option<f32> {
    (speaking && verdict == Some(echo::Verdict::Echo) && rms > hush_rms).then(|| hush_rms / rms)
}

/// What one capture chunk told the barge-in gate.
#[derive(Default)]
struct Fed {
    cut: bool,
    heard: Option<f32>,
    /// The whole frames of the chunk, as the ear should hear them.
    samples: Vec<f32>,
    /// How many of them were ARIA's own voice, turned down.
    hushed: usize,
}

/// The wake-word detector's settings, and the file they apply to.
///
/// The path is kept **apart** from `Settings::model` on purpose: writing the
/// resolved resource path back into it would put an install directory into
/// `aria.toml`, and the next installer moving it would break the detector.
pub struct Wake {
    pub settings: aria_voice::wake::Settings,
    pub model: std::path::PathBuf,
}

impl Wake {
    /// The file `aria.toml` names, or the one the installer shipped:
    /// `<resources>/wake/wake.onnx`, from `app/resources/wake/` through
    /// `bundle.resources` in `tauri.conf.json`. Replacing that file replaces
    /// the model.
    pub fn resolve(resources: Option<std::path::PathBuf>, settings: &aria_voice::wake::Settings) -> Self {
        let model = match (&settings.model, resources) {
            (Some(path), _) => path.clone(),
            (None, Some(dir)) => dir.join(aria_voice::wake::RESOURCE),
            // Not fatal: `Detector::load` will say no file is there, and the
            // transcript keeps the name.
            (None, None) => std::path::PathBuf::from(aria_voice::wake::RESOURCE),
        };
        Self {
            settings: settings.clone(),
            model,
        }
    }
}

/// Hands captured audio to the engine thread and returns immediately.
struct ChannelSink(Sender<Vec<f32>>);

impl Sink for ChannelSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        let real = chunk.valid_frames * chunk.channels as usize;
        // A closed receiver means the engine thread is gone; there is nothing
        // useful to do about it here, and the recorder is stopped elsewhere.
        let _ = self.0.send(chunk.samples[..real].to_vec());
    }
}

/// Starts listening, on its own thread.
///
/// Never fatal. A missing model or a denied microphone leaves the application
/// running with its voice switched off and says so in the log — an assistant
/// that refuses to start because a 2.4 GB download has not finished is an
/// assistant nobody gets to configure.
pub fn spawn(
    core: Arc<Core>,
    cal: Calibration,
    name: Name,
    wake: Wake,
    pending: calibration::Pending,
    speaker: Arc<OnceLock<Silencer>>,
    spoken: Arc<echo::SpokenLog>,
) {
    std::thread::Builder::new()
        .name("aria-voice".into())
        .spawn(move || {
            if let Err(e) = listen(core, cal, name, wake, pending, speaker, spoken) {
                tracing::error!("la chaîne vocale s'est arrêtée : {e:#}");
            }
        })
        .expect("the voice thread can be named");
}

fn listen(
    core: Arc<Core>,
    cal: Calibration,
    name: Name,
    wake: Wake,
    pending: calibration::Pending,
    // Filled by the speech thread once the player opens. A `OnceLock` and
    // not a parameter because the two threads are started in the order the
    // shell starts them, and neither owns the other.
    speaker: Arc<OnceLock<Silencer>>,
    // Written by the speech thread on the way to the loudspeaker. The slow
    // half of the echo defence, and the only one that works while the coupling
    // is still `Unresolved` — which is the first two seconds of every answer.
    spoken: Arc<echo::SpokenLog>,
) -> anyhow::Result<()> {
    let (tx, rx) = channel::<Vec<f32>>();
    let recorder: Recorder<ChannelSink> =
        Recorder::open_with_failure_handler(RecorderConfig::speech(), |error| {
            // Said out loud rather than swallowed: a microphone that dropped
            // mid-session looks exactly like an assistant that stopped caring.
            tracing::error!("le micro a lâché : {error}");
        })
        .map_err(|e| anyhow::anyhow!("cannot open the microphone: {e}"))?;
    tracing::info!(device = ?recorder.info(), "micro ouvert");

    let mut engine: Option<Engine> = None;
    let mut meter = Meter::default();
    let mut barge = Barge::new(cal);
    let mut capturing = false;
    let mut watch = Watch::default();

    loop {
        if core.audio_mode() == AudioMode::Off {
            if capturing {
                // Decision 19: off unloads. It is the only mode that saves
                // anything, and waking from it costs about 3.8 s.
                let _ = recorder.stop();
                engine = None;
                capturing = false;
                core.set_voice_state(VoiceState::Idle);
                tracing::info!("écoute coupée, modèles déchargés");
            }
            std::thread::sleep(OFF_POLL);
            continue;
        }

        if !capturing {
            // Said before the 3.8 s, not after. It is the one stretch where
            // ARIA is on screen and deaf, and an orb that looks idle through
            // it is what makes a new user speak into a chain that is not there
            // yet.
            //
            // Registered as **one of** the loads rather than setting the state
            // directly, since 2026-10-28. The speech thread may be reading a
            // model of its own at the same moment — Kyutai's is a hundred
            // megabytes, and a download the first time — and whichever of the
            // two finished first used to end the wait for both. The orb lit
            // up, and ARIA was listening with nothing to answer with.
            core.begin_load(Load::Hearing);
            let loaded = load_for(core.ear(), cal, name.clone(), &wake);
            // Cleared on both paths, and before the recorder is touched: the
            // claim is "the model is in memory", which is true here and has
            // nothing to do with whether capture then starts.
            core.finished_load(Load::Hearing);
            match loaded {
                Ok(loaded) => engine = Some(loaded),
                Err(e) => {
                    // Parking rather than retrying: a missing model does not
                    // appear by itself, and a retry loop would fill the log
                    // instead of the gap.
                    tracing::error!("chaîne vocale indisponible, écoute désactivée : {e:#}");
                    // Left idle rather than stuck loading: the orb must not
                    // spin for ever over something that will never arrive.
                    // `finished_load` above already did that, unless the voice
                    // is still coming — which is correct, because then there
                    // is still something to wait for.
                    return Ok(());
                }
            }
            recorder
                .start(ChannelSink(tx.clone()))
                .map_err(|e| anyhow::anyhow!("cannot start recording: {}", e.error))?;
            capturing = true;
            drain(&rx);
            tracing::info!("écoute active");
        }

        // The main agent's engine changed between Parakeet and a model that
        // hears (epic 10). The microphone stays open; only the ear is swapped,
        // and the old one is dropped **first** — 2.4 GB freed before anything
        // else is read, which is what the user asked for: unload, then load.
        // The same for the GPU switch, which moves Parakeet to or from the
        // graphics card (`aria_voice::asr::set_gpu`).
        if let Some(current) = engine.as_ref()
            && (current.transcribes() != (core.ear() == Ear::Transcript) || current.misplaced())
        {
            drop(engine.take());
            core.begin_load(Load::Hearing);
            let loaded = load_for(core.ear(), cal, name.clone(), &wake);
            core.finished_load(Load::Hearing);
            match loaded {
                Ok(loaded) => {
                    tracing::info!(
                        transcription = loaded.transcribes(),
                        "oreille rechargée pour le nouveau moteur"
                    );
                    engine = Some(loaded);
                }
                Err(e) => {
                    tracing::error!("oreille indisponible, écoute désactivée : {e:#}");
                    let _ = recorder.stop();
                    return Ok(());
                }
            }
            watch = Watch::default();
            drain(&rx);
        }

        let Some(engine) = engine.as_mut() else {
            continue;
        };
        match rx.recv_timeout(OFF_POLL) {
            Ok(samples) => {
                // A copy for a measurement, when one is waiting. Cheap when
                // none is, which is almost always: one uncontended lock.
                pending.feed(&samples);
                // The fast half of barge-in, and it goes **before** anything
                // that can block. `engine.push` may spend a quarter of a
                // second in the ASR writing a transcript early, and a cut read
                // on the far side of that is a cut a quarter of a second late.
                //
                // The meter is fed from the same pass, **after** the gate has
                // judged each frame (2026-11-17): what reaches the orb is the
                // user's share, not ARIA's voice coming back. With no speaker
                // yet, nothing plays, so the whole chunk is the user's.
                // What the ear gets: the gate's version once a voice exists,
                // the raw chunk before. See `Barge::feed`.
                let mut ear: Option<Vec<f32>> = None;
                let heard = match speaker.get() {
                    Some(speaker) => {
                        let fed = barge.feed(&samples, speaker);
                        if fed.hushed > 0 {
                            tracing::trace!(frames = fed.hushed, "ma propre voix, cachée à l'oreille");
                        }
                        watch.aria_spoke |= speaker.state() == Playing::Speaking;
                        watch.cut |= fed.cut;
                        if fed.cut {
                            tracing::info!(
                                coupling = ?barge.coupling(),
                                "on me parle dessus — je me tais"
                            );
                            speaker.silence();
                            // And the answer being written, not only the one
                            // being heard — see `Core::interrupt_agent`.
                            core.interrupt_agent();
                        }
                        ear = Some(fed.samples);
                        fed.heard
                    }
                    None if samples.is_empty() => None,
                    None => Some(
                        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32)
                            .sqrt(),
                    ),
                };
                if let Some(rms) = heard {
                    core.push_level(meter.push_rms(rms));
                }
                // Hybrid only acts on a named turn, so a turn the wake-word
                // detector did not hear the name in is not even transcribed.
                engine.set_needs_name(core.audio_mode() == AudioMode::Hybrid);
                if let Some(heard) = engine.push(ear.as_deref().unwrap_or(&samples))? {
                    if engine.transcribes() {
                        act(&core, engine, heard, &spoken, barge.coupling());
                    } else {
                        hear(&core, engine, heard, &mut watch);
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the capture channel closed");
            }
        }
    }
}

/// What the core is told about one pause.
fn act(
    core: &Core,
    engine: &mut Engine,
    heard: Heard,
    spoken: &echo::SpokenLog,
    coupling: echo::Coupling,
) {
    // Every pause leaves a trace, published or not — and that is the point.
    // In hybrid an unnamed turn goes nowhere, by design, so without this line
    // a chain that is working perfectly and a chain that is deaf look exactly
    // the same from outside: nothing happens either way. It cost an evening
    // once, and "pas de réaction au micro" is the only symptom either produces.
    //
    // The **fact** at debug, the **words** at trace. Decision 19 makes
    // named-only a privacy guarantee, and a log that prints every sentence
    // spoken in the room is not that guarantee — even on the operator's own
    // machine.
    // `heard.addressed` and not the post-veto `Allowed::name`, on purpose: this
    // line says what was *heard*, and it is traced before the veto so that an
    // ignored echo still leaves a pause in the log. The two differ exactly when
    // the next line fires, and that is the information one comes here for.
    tracing::debug!(
        named = heard.addressed,
        turn = ?heard.turn,
        prosody = heard.prosody,
        words = heard.transcript.split_whitespace().count(),
        "pause"
    );
    tracing::trace!("entendu : {}", heard.transcript);

    // ARIA's own voice is ruled out **first**, and every trigger below is read
    // off the result rather than off `heard`. Closed on 2026-11-13; the hole it
    // closes is named in decision 114.
    //
    // The veto used to sit further down, after the name had already been
    // applied, so an echo carrying "Aria" threw the turn away and **kept the
    // wake**: hybrid flipped to active on ARIA's own sentence, and then it
    // listened to the room without the name for the rest of the window. The
    // sleep request had the same shape — an echo of "merci Aria" could put ARIA
    // to sleep — and in active mode the echo was published to the window as
    // something the user had said.
    //
    // Hence [`Allowed`] rather than an early `return`: a trigger that cannot be
    // computed without passing the veto cannot be forgotten by the next person
    // who adds one.
    let Some(allowed) = allowed(&heard, spoken, coupling) else {
        // The **fact** at warning, the **words** at trace. A veto that misfires
        // drops a real request, so it has to be visible in a log nobody turned
        // up — but decision 19 is what keeps the sentence itself out of the
        // default level.
        tracing::warn!(
            ?coupling,
            turn = ?heard.turn,
            "transcript reconnu comme ma propre voix : tour ignoré"
        );
        tracing::trace!(transcript = %heard.transcript, "écho vétoé");
        // The voice state is deliberately left alone. ARIA may still be
        // speaking this very sentence, and announcing `Listening` here would
        // light the orb for a turn that never happened.
        engine.end_turn();
        return;
    };
    // A pause with no words in it — a breath, vape breathed out near the
    // microphone, a chair — is not someone talking. Before 2026-11-23 it was
    // published anyway, and `Listening` is what silences ARIA on the slow
    // path: she stopped mid-sentence for a breath. Nothing is published and
    // the state is left alone; a complete one also leaves the turn, so the
    // next real sentence does not start with it.
    if !has_words(&heard.transcript) {
        tracing::debug!(turn = ?heard.turn, "pause sans mots : ignorée");
        if heard.turn == Turn::Over {
            engine.end_turn();
        }
        return;
    }

    let named = allowed.name;

    if heard.turn == Turn::Over && !named && core.audio_mode() != AudioMode::Active {
        // The one case a user reads as a failure. Said plainly, with the fix
        // in it, because the alternative is concluding the microphone is dead.
        tracing::info!(
            "tour complet entendu, mais mon nom n'y était pas — en mode hybride je me tais. \
             Dis « Aria, … » ou passe en écoute active."
        );
    }
    if named {
        // The gate refuses this from `Off`, and rightly — but nothing is
        // listening there, so it cannot arrive. A refusal is worth a line
        // either way: it means a trigger was invented somewhere.
        match core.apply_audio(ModeTrigger::NameHeard) {
            Ok(change) => tracing::info!(?change, "mon nom a été entendu"),
            Err(e) => tracing::debug!("nom entendu, sans effet : {e}"),
        }
    }

    let name_only = allowed.address_only;

    // Hybrid without the name: the transcript was used locally to look for the
    // name and goes no further. That is what makes named-only a guarantee.
    let publish = named || core.audio_mode() == AudioMode::Active;
    if publish {
        // Not final when it is only the name: more is coming, and the window
        // shows a partial dimmed precisely so it can be replaced without
        // looking like two things were said.
        core.push_transcript(
            heard.transcript.clone(),
            heard.turn == Turn::Over && !name_only,
        );
    }

    if heard.turn != Turn::Over {
        if publish {
            core.set_voice_state(VoiceState::Listening);
        }
        return;
    }

    // A complete turn that is nothing but ARIA's name is an **address**, not a
    // request: someone getting its attention before saying what they want.
    //
    // Measured on 2026-10-28. A pause after the name split one sentence into
    // two complete turns — « Ariane. » then « Quel jour est-on ? » — and the
    // first went to the model on its own. It answered « Ariane ? Je ne suis
    // pas sûr de bien saisir. », which is the only thing it could say, and
    // said it out loud over someone still talking. Two model turns, one of
    // them nonsense, for one question.
    //
    // The end-of-turn detector is not at fault and must not be blamed: a name
    // said alone genuinely ends on a falling intonation and a full stop, so
    // both signals agree the turn is over. They are right. What was wrong was
    // treating a finished address as a finished request.
    //
    // So the mode is woken — that part already happened above — and the turn
    // is **not ended**. The stream keeps running, so what follows is
    // transcribed with the name still in front of it and arrives as one
    // sentence: « Ariane. Quel jour est-on ? ». Which is what was said.
    if name_only {
        tracing::info!("on m'appelle : j'attends la suite plutôt que de répondre au nom seul");
        core.set_voice_state(VoiceState::Listening);
        return;
    }

    // Checked before the turn is handed anywhere: "merci Aria" ends the
    // exchange, it is not a request.
    if allowed.sleep {
        match core.apply_audio(ModeTrigger::SpokenSleep) {
            Ok(change) => tracing::info!(?change, "mise en veille demandée à la voix"),
            Err(e) => tracing::debug!("demande de veille sans effet : {e}"),
        }
        core.set_voice_state(VoiceState::Idle);
        engine.end_turn();
        return;
    }

    if publish {
        tracing::info!(
            prosody = heard.prosody,
            at = heard.at_seconds,
            "tour complet : {}",
            heard.transcript
        );
        core.set_voice_state(VoiceState::Thinking);
        // Handed to the main agent, through the one door the keyboard also
        // uses — which is what keeps this module from knowing that an agent
        // exists. It does nothing if none is running, and the transcript has
        // been published either way.
        core.heard_turn(&heard.transcript);
        // Restarts the two-minute timer. The end of a *processed* turn, not of
        // the last noise — decision 19, and the failure it avoids is a
        // television keeping ARIA awake for ever.
        core.turn_processed(named);
    }
    engine.end_turn();
}

/// The ear the main agent's engine needs: Parakeet for one that reads, Smart
/// Turn alone for one that hears (epic 10).
fn load_for(
    ear: Ear,
    cal: Calibration,
    name: Name,
    wake: &Wake,
) -> anyhow::Result<Engine> {
    let mut engine = match ear {
        Ear::Transcript => Engine::load(cal, name)?,
        Ear::Audio => Engine::load_listener(cal, name)?,
    };
    engine.set_wake(&wake.model, &wake.settings);
    Ok(engine)
}

/// What happened around ARIA's own voice since the turn began — for an ear
/// with no transcript, where the word veto of decisions 113 to 116 has
/// nothing to compare.
#[derive(Default)]
struct Watch {
    /// Sound came out of the loudspeaker at some point in this turn.
    aria_spoke: bool,
    /// The acoustic gate ([`echo::Live`]) judged something to be the user
    /// talking over it.
    cut: bool,
}

/// A pause, for an ear that hands the agent sound rather than words — epic 10.
///
/// # Hybrid works here only through the wake-word detector
///
/// There is no transcript to find the name in. Decided by the user on
/// 2026-11-18: hybrid comes back with the detector of epic 09, and since
/// 2026-11-22 it is here — `heard.addressed` is its verdict, alone, since there
/// are no words to confirm it with. A wake heard while ARIA was speaking and
/// that the echo gate did not call an interruption is her own voice, and wakes
/// nothing. Without a detector, a finished turn outside active listening is
/// dropped, with the remedy in the log.
///
/// # The echo, acoustically
///
/// Without words, ARIA's own voice can only be recognised by **when** it was
/// heard. A turn captured while the loudspeaker was playing goes to the agent
/// only if [`echo::Live`] judged it an interruption. While the gate has not
/// yet learnt the room it cuts nothing, so nothing heard over ARIA goes
/// through: a missed interruption is better than ARIA answering itself, which
/// is the trade decision 80 already made.
fn hear(core: &Core, engine: &mut Engine, heard: Heard, watch: &mut Watch) {
    tracing::debug!(turn = ?heard.turn, prosody = heard.prosody, "pause (oreille audio)");

    if core.audio_mode() != AudioMode::Active {
        let my_own_voice = watch.aria_spoke && !watch.cut;
        if heard.addressed && !my_own_voice {
            match core.apply_audio(ModeTrigger::NameHeard) {
                Ok(change) => tracing::info!(?change, "mon nom a été entendu"),
                Err(e) => tracing::debug!("nom entendu, sans effet : {e}"),
            }
        }
        if core.audio_mode() != AudioMode::Active {
            if heard.turn == Turn::Over {
                if engine.hears_name() {
                    tracing::info!(
                        "tour complet entendu, mais mon nom n'y était pas — en mode hybride je me \
                         tais. Dis « Aria, … » ou passe en écoute active."
                    );
                } else {
                    tracing::info!(
                        "tour entendu, mais le mode hybride ne fonctionne pas sans le détecteur de \
                         mot de réveil : sans transcription je ne reconnais pas mon nom. Passe en \
                         écoute active (raccourci ou orbe)."
                    );
                }
                engine.end_turn();
                *watch = Watch::default();
            }
            return;
        }
    }

    if heard.turn != Turn::Over {
        core.set_voice_state(VoiceState::Listening);
        return;
    }

    let finished = std::mem::take(watch);
    if finished.aria_spoke && !finished.cut {
        tracing::warn!(
            "tour entendu pendant que je parlais, sans interruption reconnue : ignoré — sans \
             doute ma propre voix"
        );
        engine.end_turn();
        return;
    }
    let Some(audio) = heard.audio else {
        engine.end_turn();
        return;
    };
    tracing::info!(
        secondes = audio.len() as f32 / aria_voice::wav::SAMPLE_RATE as f32,
        prosody = heard.prosody,
        "tour complet, envoyé en audio"
    );
    core.set_voice_state(VoiceState::Thinking);
    core.heard_audio(audio);
    core.turn_processed(false);
    engine.end_turn();
}

/// What a pause may still trigger, once ARIA's own voice has been ruled out.
///
/// A struct rather than three reads of [`Heard`], and that *is* the fix of
/// 2026-11-13: a trigger that cannot be obtained without having passed the veto
/// cannot be applied before it. The next trigger someone adds will have to come
/// through here too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Allowed {
    /// The name was heard, early enough to be an address.
    name: bool,
    /// A complete turn asking for quiet.
    sleep: bool,
    /// A complete turn that is nothing but the name.
    address_only: bool,
}

/// Rule out ARIA hearing itself, and say what is left.
///
/// `None` is the veto, and it means **nothing** about this pause has an effect:
/// no wake, no sleep, no publication, no turn.
///
/// # Asked on every pause, not only on a complete turn
///
/// The veto used to be reached only after the `Turn::Over` return, so a partial
/// pause that was pure echo was still published to the window and still piled
/// into the turn in flight. Here it is asked first, and the common case costs
/// nothing: [`echo::SpokenLog::vetoes`] returns on an empty log, which is what
/// it is whenever ARIA has not spoken in the last eight seconds.
///
/// # What it deliberately does not catch
///
/// **An echo clipped to the name alone.** [`echo::is_self_echo`] carries a floor
/// of two words, and lowering it for this case would break the thing it protects:
/// "Aria" said alone over ARIA's own sentence is also exactly how a user
/// interrupts. The acoustic gate is the defence there, in 60 ms, and it does not
/// need the words.
fn allowed(heard: &Heard, spoken: &echo::SpokenLog, coupling: echo::Coupling) -> Option<Allowed> {
    // The evidence is the words, and the words ARIA spoke are known exactly
    // because it chose them. It matters most where the fast gate is silent: for
    // the first two seconds of an answer the coupling is `Unresolved` and
    // `Live` cuts nothing at all — which was the window in which ARIA could
    // hear itself, transcribe itself and answer itself, at the speed of the
    // voice chain.
    if spoken.vetoes(&heard.transcript, coupling) {
        return None;
    }
    Some(Allowed {
        name: heard.addressed,
        sleep: heard.sleep_request,
        address_only: heard.address_only,
    })
}

/// Throw away whatever piled up while nothing was listening.
///
/// Without this the first turn after waking begins with the tail of the last
/// one, and the end-of-turn detector is handed a window with two unrelated
/// voices in it.
fn drain(rx: &Receiver<Vec<f32>>) {
    while !matches!(
        rx.try_recv(),
        Err(TryRecvError::Empty | TryRecvError::Disconnected)
    ) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sentence ARIA actually said, and the echo of it.
    const SAID: &str = "Il fera vingt-trois degrés demain à Lyon";

    fn heard(transcript: &str) -> Heard {
        Heard {
            transcript: transcript.to_string(),
            turn: Turn::Over,
            prosody: 0.9,
            addressed: true,
            sleep_request: true,
            address_only: false,
            at_seconds: 1.0,
            audio: None,
        }
    }

    fn log(said: &str) -> echo::SpokenLog {
        let log = echo::SpokenLog::new();
        log.push(said);
        log
    }

    /// The hole of decision 114, as a test: an echo carrying the name used to
    /// throw the turn away and **keep the wake**.
    #[test]
    fn an_echo_of_my_own_sentence_triggers_nothing_at_all() {
        let coupling = echo::Coupling::Locked {
            lag: 22,
            coupling_db: -14.8,
        };
        assert_eq!(allowed(&heard(SAID), &log(SAID), coupling), None);
    }

    /// And the same words with nothing coming back are the user repeating them.
    /// This is the half that must not become a veto: `Absent` means headphones,
    /// a muted output or an echo canceller upstream, and a wrongful veto drops
    /// a real request in silence.
    #[test]
    fn the_same_words_are_the_user_when_nothing_comes_back() {
        let allowed = allowed(&heard(SAID), &log(SAID), echo::Coupling::Absent)
            .expect("rien ne revient dans le micro : ce n'est pas un écho");
        assert!(allowed.name);
        assert!(allowed.sleep);
    }

    /// A real turn passes its three triggers through untouched.
    #[test]
    fn a_turn_that_is_not_my_voice_keeps_its_triggers() {
        let mut spoken = heard("Aria, quel temps fera-t-il demain");
        spoken.sleep_request = false;
        let allowed = allowed(&spoken, &log(SAID), echo::Coupling::Unresolved).expect("pas un écho");
        assert_eq!(
            allowed,
            Allowed {
                name: true,
                sleep: false,
                address_only: false
            }
        );
    }

    /// The case left open on purpose, asserted so it is a decision and not a
    /// surprise: an echo clipped to the name alone is **not** vetoed, because
    /// "Aria" alone over ARIA's own sentence is also how a user interrupts.
    #[test]
    fn a_name_on_its_own_is_never_vetoed() {
        let mut clipped = heard("Aria");
        clipped.address_only = true;
        let coupling = echo::Coupling::Locked {
            lag: 22,
            coupling_db: -14.8,
        };
        let allowed = allowed(&clipped, &log("Aria est prête"), coupling)
            .expect("deux mots au moins, sinon le veto casse le barge-in");
        assert!(allowed.address_only);
    }

    /// Nothing spoken recently means no claim to make, whatever the coupling.
    #[test]
    fn with_an_empty_log_there_is_nothing_to_compare() {
        let coupling = echo::Coupling::Locked {
            lag: 22,
            coupling_db: -14.8,
        };
        assert!(allowed(&heard(SAID), &echo::SpokenLog::new(), coupling).is_some());
    }
}

#[cfg(test)]
mod meter {
    use super::*;

    /// ARIA silent: the microphone is the user, all of it.
    #[test]
    fn with_aria_silent_the_whole_frame_is_the_user() {
        assert_eq!(heard_rms(0.08, false, None), 0.08);
        assert_eq!(heard_rms(0.08, false, Some(echo::Verdict::Echo)), 0.08);
    }

    /// 40 ms of a synthetic voice: a 140 Hz fundamental and four harmonics.
    fn vowel(f0: f32) -> Vec<f32> {
        (0..2 * FRAME)
            .map(|n| {
                let t = n as f32 / 16_000.0;
                (1..=5)
                    .map(|k| (2.0 * std::f32::consts::PI * f0 * k as f32 * t).sin() / k as f32)
                    .sum::<f32>()
                    * 0.1
            })
            .collect()
    }

    /// 40 ms of white noise — a breath, as the microphone sees one.
    fn hiss(seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..2 * FRAME)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32 * 0.2 - 0.1
            })
            .collect()
    }

    /// 2026-11-23: breathing vape out near the microphone cut ARIA off. A
    /// voice has a pitch, a breath does not — low voice to child's.
    #[test]
    fn a_voice_is_voiced_and_a_breath_is_not() {
        for f0 in [90.0, 140.0, 220.0, 320.0] {
            let score = voicing(&vowel(f0));
            assert!(score >= VOICED, "{f0} Hz scored {score}");
        }
        for seed in [1, 7, 42, 1234] {
            let score = voicing(&hiss(seed));
            assert!(score < VOICED, "noise {seed} scored {score}");
        }
        // Air rumbling on the capsule: slow, and with no pitch in range.
        let rumble: Vec<f32> = (0..2 * FRAME)
            .map(|n| (2.0 * std::f32::consts::PI * 30.0 * n as f32 / 16_000.0).sin() * 0.2)
            .collect();
        assert!(voicing(&rumble) < VOICED, "rumble scored {}", voicing(&rumble));
        // A voice with a breath over it is still a voice.
        let mixed: Vec<f32> = vowel(140.0).iter().zip(hiss(3)).map(|(v, h)| v + h * 0.3).collect();
        assert!(voicing(&mixed) >= VOICED, "voice under hiss scored {}", voicing(&mixed));
    }

    /// The slow path's half of the same fault: a pause the ASR wrote down as
    /// nothing, or as a bare noise, is not a turn.
    #[test]
    fn a_noise_written_down_is_not_words() {
        assert!(!has_words(""));
        assert!(!has_words("Hm."));
        assert!(!has_words("Euh... ah."));
        assert!(has_words("Ah, non."));
        assert!(has_words("Aria."));
        assert!(has_words("Finalement, un renard."));
    }

    /// 2026-11-23: ARIA's voice reached the ASR, opened a segment, and the
    /// user's reply landed in the same transcript. Only her echo is turned
    /// down — never the user, never a frame the gate has not judged.
    #[test]
    fn only_aria_s_own_voice_is_hidden_from_the_ear() {
        use echo::Verdict::{Barge, Echo, Quiet};
        let hush = 0.01;
        let scale = hush_scale(0.08, true, Some(Echo), hush).expect("her echo is turned down");
        assert!((0.08 * scale - hush).abs() < 1e-6, "to the hush level, not to zero");

        assert_eq!(hush_scale(0.08, true, Some(Barge), hush), None, "the user passes whole");
        assert_eq!(hush_scale(0.08, true, None, hush), None, "an unjudged frame passes");
        assert_eq!(hush_scale(0.08, true, Some(Quiet), hush), None);
        assert_eq!(hush_scale(0.08, false, Some(Echo), hush), None, "ARIA silent: all the user's");
        assert_eq!(hush_scale(0.005, true, Some(Echo), hush), None, "already under the floor");
    }

    /// The point of 2026-11-17: ARIA's own voice coming back through the room
    /// must not light the orb's listening parts.
    #[test]
    fn while_aria_speaks_only_an_interruption_reaches_the_meter() {
        assert_eq!(heard_rms(0.08, true, Some(echo::Verdict::Barge)), 0.08);
        assert_eq!(heard_rms(0.08, true, Some(echo::Verdict::Echo)), 0.0);
        assert_eq!(heard_rms(0.08, true, Some(echo::Verdict::Quiet)), 0.0);
    }

    /// The first two seconds of an answer, before the gate has learnt the
    /// room: unknown is shown as nothing rather than as the user.
    #[test]
    fn an_unjudged_frame_while_aria_speaks_shows_nothing() {
        assert_eq!(heard_rms(0.08, true, None), 0.0);
    }

    /// The meter itself still rises fast and falls slowly.
    #[test]
    fn the_meter_rises_fast_and_falls_slowly() {
        let mut meter = Meter::default();
        let up = meter.push_rms(0.1);
        assert!(up > 0.4, "{up}");
        let down = meter.push_rms(0.0);
        assert!(down > up * 0.8, "{down} après {up}");
    }
}
