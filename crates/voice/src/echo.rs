//! Echo: ARIA must not answer itself.
//!
//! The loop is short and fatal. ARIA speaks, the microphone hears the speakers,
//! the ASR transcribes ARIA's own sentence, the turn detector calls it a
//! finished turn, and the agent answers itself — forever, at the speed of the
//! voice chain.
//!
//! **This module detects; it does not cancel.** That is a decision, not a
//! shortcut. A canceller (WebRTC AEC3 and its adaptive filter) exists to let two
//! voices overlap intelligibly, and ARIA never needs that: the right answer to
//! being interrupted is to stop talking. Detect the interruption, stop playback
//! within one frame, and the echo is gone 20 ms later — with the microphone
//! clean for everything that follows. Cancellation would buy back only the first
//! syllable, in exchange for a C++ toolchain and an adaptive filter to debug.
//!
//! What makes detection cheap here is that **we own the playback path**: the
//! samples going to the speakers are ours, so there is an exact reference. The
//! work is three steps, all on the 20 ms energy envelope rather than the
//! waveform, because a room smears phase but preserves the shape of the
//! envelope:
//!
//!  1. **lock** the delay between reference and microphone ([`lock_delay`]);
//!  2. **learn** the coupling — how much of what is played comes back in
//!     ([`coupling_db`]), which depends on the speaker volume, the microphone
//!     gain and the room, so it cannot be a constant;
//!  3. **compare** each microphone frame against the echo that coupling
//!     predicts. Energy the reference does not explain is somebody else
//!     talking ([`Gate::classify`]).
//!
//! The acoustic test can be fooled — by a speakerphone, by a mains hum, by a
//! volume change mid-sentence. [`is_self_echo`] is the last line of defence,
//! after the ASR: a transcript that repeats what ARIA just said is dropped
//! whatever the gate thought.
//!
//! **Both halves are wired as of 2026-11-04.** [`Live`] cuts playback through
//! the shell, and [`SpokenLog`] closes the other half: the shell records every
//! sentence on its way to the speakers, and the capture thread asks
//! [`SpokenLog::vetoes`] before a transcript is handed to the agent. Until that
//! day the veto existed and nobody called it, so echo loud enough to clear
//! `speech_rms` reached the ASR and ARIA could answer itself.

/// The crate-wide 20 ms frame, re-exported so this module reads on its own.
/// One number, one meaning: the gate and the echo detector count in the same
/// units or their thresholds silently mean different things.
pub use crate::calibration::FRAME;

use crate::calibration::Calibration;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long after the reference falls silent its echo can still be heard. A
/// living room runs 150 to 250 ms; the predictor takes the loudest reference
/// frame over this span so a reverb tail is never mistaken for a new voice.
pub const TAIL_FRAMES: usize = 10;

/// Consecutive frames above the margin before barge-in is declared. One frame is
/// a door slam; three in a row is a voice.
pub const BARGE_FRAMES: usize = 3;

/// What a microphone frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing loud enough to be anything.
    Quiet,
    /// Explained by what the speakers are playing. Drop it: do not transcribe
    /// it, and do not show it to the turn detector.
    Echo,
    /// Energy the reference does not account for. Somebody is talking over
    /// ARIA: stop playback now.
    Barge,
}

/// RMS per [`FRAME`], which is the only view of the signal this module uses.
pub fn envelope(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(FRAME)
        .map(|frame| (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt())
        .collect()
}

fn db(ratio: f32) -> f32 {
    20.0 * ratio.max(1e-9).log10()
}

/// How far the microphone lags the reference, in frames, and how well the two
/// envelopes agree there.
///
/// The lag is **not** the acoustic delay alone: it also carries however long the
/// output stream took to start relative to the input stream. That is fine, and
/// it is the same quantity the live gate needs — what matters is where the
/// reference sits in the microphone's timeline, not why.
///
/// Correlation is computed on mean-removed envelopes (Pearson), so a constant
/// noise floor in either signal cannot manufacture agreement.
pub fn lock_delay(mic_env: &[f32], ref_env: &[f32], max_lag_frames: usize) -> (usize, f32) {
    let mut best = (0usize, f32::MIN);
    for lag in 0..=max_lag_frames.min(mic_env.len().saturating_sub(1)) {
        let span = (mic_env.len() - lag).min(ref_env.len());
        // Half the reference, at least a second. A lag that slides most of the
        // way off the end leaves a sliver of overlap, and a correlation over a
        // sliver is free to be high by accident — measured at 0.84 over 27
        // frames once the search range was widened to ten seconds.
        if span < (ref_env.len() / 2).max(50) {
            break;
        }
        let mic = &mic_env[lag..lag + span];
        let reference = &ref_env[..span];
        let mic_mean = mic.iter().sum::<f32>() / span as f32;
        let ref_mean = reference.iter().sum::<f32>() / span as f32;
        let mut cov = 0.0;
        let mut mic_var = 0.0;
        let mut ref_var = 0.0;
        for (m, r) in mic.iter().zip(reference) {
            let (dm, dr) = (m - mic_mean, r - ref_mean);
            cov += dm * dr;
            mic_var += dm * dm;
            ref_var += dr * dr;
        }
        let rho = cov / (mic_var * ref_var).sqrt().max(1e-12);
        if rho > best.1 {
            best = (lag, rho);
        }
    }
    best
}

/// How much of what the speakers play comes back into the microphone, in dB.
/// Always negative, typically −15 to −35 dB on a laptop.
///
/// Taken as the **25th percentile** of the per-frame ratio, not the mean, and
/// not over every frame. Only frames where the reference is loud enough to
/// measure count, because a quiet reference measures the microphone's noise
/// floor instead. And a low percentile rather than a middle one because
/// somebody talking only ever makes the ratio *larger*: the low quarter is the
/// part of the recording where nothing but echo was happening, which is exactly
/// what has to be estimated — and it keeps the estimate valid even when a
/// quarter of the window is double-talk.
pub fn coupling_db(mic_env: &[f32], ref_env: &[f32], lag: usize) -> Option<f32> {
    let loudest = ref_env.iter().copied().fold(0.0f32, f32::max);
    let floor = loudest * 0.2;
    let mut ratios: Vec<f32> = Vec::new();
    for (t, &r) in ref_env.iter().enumerate() {
        if r < floor {
            continue;
        }
        if let Some(&m) = mic_env.get(t + lag) {
            ratios.push(db(m / r));
        }
    }
    if ratios.len() < 5 {
        return None;
    }
    ratios.sort_by(f32::total_cmp);
    Some(ratios[ratios.len() / 4])
}

/// The gate itself: a locked delay, a learned coupling, and two thresholds.
#[derive(Debug, Clone, Copy)]
pub struct Gate {
    /// From [`lock_delay`].
    pub lag: usize,
    /// From [`coupling_db`].
    pub coupling_db: f32,
    /// How far above the predicted echo counts as another voice. Calibrated by
    /// `bin/echo.rs`: it has to clear the worst echo-only frame, or ARIA
    /// interrupts itself.
    pub margin_db: f32,
    /// Below this RMS there is nothing to classify.
    pub speech_rms: f32,
}

/// How well the two envelopes must agree before a reference is believed.
///
/// Below this there is no lock, and a coupling computed anyway would be a
/// number with no meaning behind it.
pub const MIN_LOCK: f32 = 0.5;

impl Gate {
    /// The gate for a machine where nothing played comes back.
    ///
    /// **Headphones are the common case**, and they are the case a voice
    /// assistant is most often used in. Measured on 2026-10-26: the correlation
    /// came back at −0.02 and the microphone had recorded a conversation in the
    /// room rather than the file being played, because there was no acoustic
    /// path at all.
    ///
    /// So "no coupling" is a **normal state, not a failure**, and the runtime
    /// must not refuse to work in it the way the measuring probe rightly does.
    /// Every loud frame is then a [`Verdict::Barge`] — which is not a
    /// degradation but exactly right: on headphones, somebody speaking while
    /// ARIA speaks *is* an interruption, so stopping playback is the wanted
    /// behaviour and costs nothing.
    ///
    /// To be re-decided whenever the input or output device changes, since
    /// plugging a headset in or out is precisely what turns one case into the
    /// other.
    pub fn without_echo(speech_rms: f32) -> Self {
        Self {
            lag: 0,
            // Far below anything a room produces, so the predicted echo always
            // falls back to the speech floor and every loud frame clears a
            // margin of zero.
            coupling_db: -120.0,
            margin_db: 0.0,
            speech_rms,
        }
    }

    /// Per-frame verdict and the statistic behind it: how many dB the microphone
    /// exceeds the echo that the reference predicts.
    ///
    /// Indexed by microphone frame. Frames before the reference starts, or after
    /// it ends plus its tail, come back `Quiet` or `Barge` on their own merit —
    /// with no reference to explain them, loud is somebody talking.
    pub fn classify(&self, mic_env: &[f32], ref_env: &[f32]) -> Vec<(Verdict, f32)> {
        mic_env
            .iter()
            .enumerate()
            .map(|(t, &mic)| self.frame(t, mic, ref_env))
            .collect()
    }

    /// The verdict for **one** microphone frame, at index `t` of a stream the
    /// reference is indexed against.
    ///
    /// Split out of [`Gate::classify`] for the live path, which has one frame
    /// and a decision to make now, where the bench has a recording and a
    /// report to write. **One implementation**, because two would be two sets
    /// of thresholds to keep in agreement — and it is the bench that
    /// calibrates the ones the live gate then uses.
    pub fn frame(&self, t: usize, mic: f32, ref_env: &[f32]) -> (Verdict, f32) {
        if mic < self.speech_rms {
            return (Verdict::Quiet, 0.0);
        }
        let coupling = 10f32.powf(self.coupling_db / 20.0);
        // The loudest reference frame over the tail, not the aligned one: the
        // echo of a syllable outlives the syllable.
        let centre = t as isize - self.lag as isize;
        let from = (centre - TAIL_FRAMES as isize).max(0) as usize;
        let to = (centre.max(0) as usize).min(ref_env.len().saturating_sub(1));
        let reference = if from > to || ref_env.is_empty() || centre < 0 {
            0.0
        } else {
            ref_env[from..=to].iter().copied().fold(0.0f32, f32::max)
        };
        let predicted = (reference * coupling).max(self.speech_rms);
        let excess = db(mic / predicted);
        if excess < self.margin_db {
            (Verdict::Echo, excess)
        } else {
            (Verdict::Barge, excess)
        }
    }

    /// First microphone frame of the first run of [`BARGE_FRAMES`] consecutive
    /// `Barge` verdicts — when playback should have stopped.
    pub fn barge_onset(verdicts: &[(Verdict, f32)]) -> Option<usize> {
        let mut run = 0usize;
        for (t, &(verdict, _)) in verdicts.iter().enumerate() {
            if verdict == Verdict::Barge {
                run += 1;
                if run == BARGE_FRAMES {
                    return Some(t + 1 - BARGE_FRAMES);
                }
            } else {
                run = 0;
            }
        }
        None
    }
}

/// How much loud reference it takes before "nothing comes back" is a
/// conclusion rather than an absence of evidence. Half a second.
pub const LEARN_FRAMES: usize = 25;

/// How many frames the two envelopes must share before a delay search means
/// anything.
///
/// [`lock_delay`] refuses to slide the reference more than halfway off the end,
/// so with less than this it only ever tries a lag of zero — and a correlation
/// at a lag it was never allowed to leave is not a lock.
///
/// Two seconds, which is roughly the first sentence of an answer. **So the
/// start of every answer is covered by the slow path and not by this one**, and
/// that is a feature rather than a gap: `unmute` refuses to let its own VAD
/// interrupt during the first three seconds, for the same reason — the thing
/// that suppresses the echo has not settled yet.
pub const LOCK_FRAMES: usize = 100;

/// How far the microphone may lag the reference: 500 ms, which covers the
/// acoustic path and whatever head start one audio stream had over the other.
pub const MAX_LAG_FRAMES: usize = 25;

/// How much recent past the live gate keeps, in frames.
///
/// Eight seconds, matching the window the player keeps as a reference. Longer
/// would be a window aligned against nothing.
pub const WINDOW_FRAMES: usize = 400;

/// How often a locked gate measures again.
///
/// The volume changes mid-session, and a coupling learned at one volume
/// predicts the wrong echo at another — which is how ARIA ends up either deaf
/// to an interruption or convinced of one that did not happen. Eight seconds is
/// far longer than a lock takes and far shorter than a conversation.
pub const RELOCK_FRAMES: usize = 400;

/// What the room does with what the speakers play.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Coupling {
    /// Not established yet. **Nothing is cut on energy in this state** — which
    /// is the whole safety property: being wrong here would make ARIA stop
    /// talking at its own voice, and that is worse than the latency this gate
    /// removes. The slow path, a transcript at the next pause, keeps working
    /// throughout.
    Unresolved,
    /// Nothing played comes back, and then every loud frame is somebody
    /// talking.
    ///
    /// **Three causes produce this state, not two**, and the third was missing
    /// here until it was met on 2026-11-04. Headphones, which is the common
    /// case for a voice assistant; loudspeakers that are muted or routed
    /// somewhere else; and **an echo canceller on the capture path, outside our
    /// process** — Windows "audio enhancements", a laptop DSP, a conferencing
    /// driver. Measured that day on an Intel Smart Sound microphone array: two
    /// effect APOs active, the played bursts coming back 45 to 97 dB down with
    /// no stable delay, and **exact digital zero** between them, which no real
    /// microphone produces. The same machine's headset microphone, on a capture
    /// path with no effect chain, returned −14.8 dB from the same speakers.
    ///
    /// The handling is right for all three — nothing of ARIA's voice reaches
    /// the microphone, so every loud frame is an interruption — but a diagnosis
    /// that names only two sends whoever reads it looking for a muted speaker
    /// that is playing perfectly well.
    Absent,
    /// Measured: the microphone lags by `lag` frames and returns `coupling_db`
    /// of what is played.
    Locked { lag: usize, coupling_db: f32 },
}

/// The gate, driven frame by frame while ARIA talks.
///
/// # Why this exists, and what it replaces
///
/// Decision 17 asks for playback to stop *inside the frame* when the user
/// speaks over ARIA. Until this existed, the only signal that could trigger it
/// was `VoiceState::Listening`, published by the shell when the chain had a
/// **closed segment with a transcript** — so the interrupter had to finish
/// their phrase *and* go quiet for `silence_ms` before ARIA noticed. Someone
/// talking continuously for two seconds was talked over for two seconds. The
/// comment in the shell claimed the opposite, which is how it survived.
///
/// So this is the fast signal, and `unmute` is what it is modelled on: two
/// independent paths to an interruption, one acoustic and immediate, one from
/// the transcript and certain. Theirs cuts on the pause predictor dropping
/// below 0.4, or on the first word out of the ASR. Ours cuts on
/// [`BARGE_FRAMES`] of unexplained energy, or on the transcript as before.
///
/// The difference that matters: a browser hands `unmute` an echo-cancelled
/// microphone. Nobody hands us one. That is what [`Coupling`] is for, and why
/// `Unresolved` cuts nothing at all.
pub struct Live {
    mic: Vec<f32>,
    speech_rms: f32,
    margin_db: f32,
    coupling: Coupling,
    /// Frames since the coupling was last measured.
    since: usize,
    /// Consecutive [`Verdict::Barge`] frames.
    run: usize,
    last: Option<Verdict>,
    /// How far the last judged frame stood above the predicted echo, in dB.
    last_excess: Option<f32>,
}

impl Live {
    pub fn new(speech_rms: f32, margin_db: f32) -> Self {
        Self {
            mic: Vec::with_capacity(WINDOW_FRAMES),
            speech_rms,
            margin_db,
            coupling: Coupling::Unresolved,
            since: 0,
            run: 0,
            last: None,
            last_excess: None,
        }
    }

    /// The gate for one calibration, which is where both numbers already live.
    pub fn for_calibration(cal: Calibration) -> Self {
        Self::new(cal.speech_rms, cal.echo_margin_db)
    }

    pub fn coupling(&self) -> Coupling {
        self.coupling
    }

    /// The last frame verdict, or `None` when there was nothing to attribute it
    /// to: no reference, or a coupling still unresolved.
    pub fn verdict(&self) -> Option<Verdict> {
        self.last
    }

    /// How far the last judged frame stood above the predicted echo, in dB —
    /// `None` for a quiet or unjudged one. The number the margin is compared
    /// with, kept so a room where interruptions never land can say by how much
    /// they miss.
    pub fn excess_db(&self) -> Option<f32> {
        self.last_excess
    }

    pub fn margin_db(&self) -> f32 {
        self.margin_db
    }

    /// One microphone frame, against what has recently gone to the speakers.
    ///
    /// `reference` is the played envelope in the same 20 ms frames, **ending
    /// now**. The two are aligned by their *ends* and not their starts, because
    /// the player keeps a rolling window: its index zero slides, so an absolute
    /// alignment would quietly drift out after eight seconds of talking.
    ///
    /// `speaking` is whether sound is actually coming out. Frames are taken in
    /// either way, so the envelopes stay aligned and a lock survives a pause;
    /// they are only *acted on* when it is true, because a loud microphone
    /// against a silent reference is somebody taking their turn, not somebody
    /// interrupting.
    ///
    /// Returns `true` on the frame that completes [`BARGE_FRAMES`] in a row —
    /// once, so the caller can cut and move on.
    pub fn push(&mut self, mic_rms: f32, reference: &[f32], speaking: bool) -> bool {
        self.mic.push(mic_rms);
        if self.mic.len() > WINDOW_FRAMES {
            let excess = self.mic.len() - WINDOW_FRAMES;
            self.mic.drain(..excess);
        }
        if !speaking {
            self.run = 0;
            self.last = None;
            self.last_excess = None;
            return false;
        }

        let shared = self.mic.len().min(reference.len());
        if shared == 0 {
            self.run = 0;
            self.last = None;
            self.last_excess = None;
            return false;
        }
        let reference = &reference[reference.len() - shared..];

        self.since += 1;
        if self.coupling == Coupling::Unresolved || self.since >= RELOCK_FRAMES {
            // The borrow of `self.mic` ends with this call, which is why the
            // measurement is a free function over slices rather than a method:
            // it has to read the buffer while its result is written back into
            // the same struct.
            if let Some(found) = measure(
                &self.mic[self.mic.len() - shared..],
                reference,
                self.speech_rms,
            ) {
                self.coupling = found;
                self.since = 0;
            }
        }

        let gate = match self.coupling {
            Coupling::Unresolved => {
                self.run = 0;
                self.last = None;
                self.last_excess = None;
                return false;
            }
            Coupling::Absent => Gate::without_echo(self.speech_rms),
            Coupling::Locked { lag, coupling_db } => Gate {
                lag,
                coupling_db,
                margin_db: self.margin_db,
                speech_rms: self.speech_rms,
            },
        };

        let (verdict, excess) = gate.frame(shared - 1, mic_rms, reference);
        self.last = Some(verdict);
        self.last_excess = (verdict != Verdict::Quiet).then_some(excess);
        if verdict == Verdict::Barge {
            self.run += 1;
        } else {
            self.run = 0;
        }
        self.run == BARGE_FRAMES
    }
}

/// What the room does, from two envelopes that end at the same moment.
///
/// `None` means "not yet", never "nothing": the caller keeps whatever it had,
/// and while it has nothing it cuts nothing.
fn measure(mic: &[f32], reference: &[f32], speech_rms: f32) -> Option<Coupling> {
    let loudest = reference.iter().copied().fold(0.0f32, f32::max);
    if loudest < speech_rms {
        return None; // nothing has been played loudly enough to learn from
    }
    let played = reference.iter().filter(|&&r| r >= loudest * 0.2).count();
    if played < LEARN_FRAMES {
        return None;
    }

    // The **positive** test for headphones, and it has to be positive: a failed
    // lock and no acoustic path look identical from the correlation alone. Here
    // the speakers have been loud for half a second and the microphone never
    // once crossed its own speech floor, which only happens when there is no
    // path at all. Measured on 2026-10-26: the correlation came back at -0.02,
    // because the microphone had recorded the room rather than the file being
    // played.
    if mic.iter().copied().fold(0.0f32, f32::max) < speech_rms {
        return Some(Coupling::Absent);
    }

    if mic.len() < LOCK_FRAMES {
        return None;
    }
    let (lag, rho) = lock_delay(mic, reference, MAX_LAG_FRAMES);
    if rho < MIN_LOCK {
        return None;
    }
    coupling_db(mic, reference, lag).map(|coupling_db| Coupling::Locked { lag, coupling_db })
}

/// Does this transcript look like ARIA reading its own sentence back?
///
/// The veto after the ASR, and the reason it exists: the acoustic gate reasons
/// about energy, so a speakerphone, a volume change mid-sentence or a reference
/// that drifted out of lock can all slip an echo through. Here the comparison is
/// on words, which is a different kind of evidence — and the words ARIA spoke
/// are known exactly, because it chose them.
///
/// Deliberately not an edit distance: the ASR drops and mangles words in an echo,
/// so what survives is a *subset*. The test is therefore containment — how much
/// of what was heard appears in what was said — with a floor of two words so
/// that "oui" or "d'accord" cannot be vetoed by coincidence.
pub fn is_self_echo(heard: &str, spoken: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let heard = words(heard);
    if heard.len() < 2 {
        return false;
    }
    let spoken = words(spoken);
    let shared = heard.iter().filter(|w| spoken.contains(w)).count();
    shared * 10 >= heard.len() * 6
}

/// How long a sentence stays worth comparing a transcript against.
///
/// Eight seconds, the same span as [`WINDOW_FRAMES`] and as the rolling
/// reference the player keeps — one number, one meaning. The bound is not
/// tidiness: past it, a transcript that matches something ARIA said is far
/// likelier to be the user saying it back than an echo of it, and vetoing that
/// would swallow a real request without a trace anyone would find.
pub const SPOKEN_MEMORY: Duration = Duration::from_secs(8);

/// How many sentences are kept at most, whatever the clock says. A bound on
/// the memory, not on the reasoning: [`SPOKEN_MEMORY`] is what decides.
const SPOKEN_KEPT: usize = 32;

/// What ARIA has just said, so a transcript can be matched against it.
///
/// The half of this module that was missing until 2026-11-04. [`is_self_echo`]
/// compares *words*, which is a different kind of evidence from the energy
/// [`Live`] reasons about — but it needs to know what was spoken, and **the
/// thread that knows is not the thread that hears**. This is the one thing
/// shared between them: the speech thread writes on the way to the
/// loudspeaker, the capture thread reads before handing a transcript to the
/// agent.
///
/// A `Mutex` rather than a channel, because a reader wants *the last few
/// seconds* and not the next message — and because the write sits on the path
/// to a synthesis that takes hundreds of milliseconds, where one uncontended
/// lock does not register.
#[derive(Debug, Default)]
pub struct SpokenLog {
    said: Mutex<VecDeque<(Instant, String)>>,
}

impl SpokenLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one sentence on its way to the speakers.
    ///
    /// Called **before** the synthesis rather than after it: an echo can be
    /// transcribed while the sentence is still being spoken, so a log written
    /// afterwards would arrive after the transcript it exists to veto.
    pub fn push(&self, text: &str) {
        self.push_at(Instant::now(), text);
    }

    /// Is this transcript ARIA hearing itself? Three conditions, all required.
    ///
    /// 1. **The coupling is not [`Coupling::Absent`].** When nothing played
    ///    comes back — headphones, a muted output, an echo canceller upstream —
    ///    a transcript resembling ARIA's sentence *cannot* be an echo of it, so
    ///    the veto could only ever be a false positive. It stays disarmed.
    /// 2. **Something was said within [`SPOKEN_MEMORY`].** With nothing recent
    ///    to compare against there is no claim to make.
    /// 3. **[`is_self_echo`] recognises the words**, which carries its own
    ///    floor of two words and six tenths containment.
    ///
    /// The recent sentences are compared **as one text**, because an echo does
    /// not respect the boundary between two sentences ARIA spoke in a row.
    ///
    /// A poisoned lock answers `false`. That direction is deliberate: letting
    /// an echo through costs one wrong answer that the user hears and can
    /// interrupt, while a wrongful veto drops a real request in silence.
    pub fn vetoes(&self, heard: &str, coupling: Coupling) -> bool {
        self.vetoes_at(Instant::now(), heard, coupling)
    }

    /// The clock seam, so the tests can age a sentence without sleeping.
    fn push_at(&self, at: Instant, text: &str) {
        let Ok(mut said) = self.said.lock() else {
            return;
        };
        said.push_back((at, text.to_string()));
        Self::trim(&mut said, at);
    }

    fn vetoes_at(&self, now: Instant, heard: &str, coupling: Coupling) -> bool {
        if coupling == Coupling::Absent {
            return false;
        }
        let Ok(mut said) = self.said.lock() else {
            return false;
        };
        Self::trim(&mut said, now);
        if said.is_empty() {
            return false;
        }
        let spoken: Vec<&str> = said.iter().map(|(_, text)| text.as_str()).collect();
        is_self_echo(heard, &spoken.join(" "))
    }

    fn trim(said: &mut VecDeque<(Instant, String)>, now: Instant) {
        while said
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > SPOKEN_MEMORY)
        {
            said.pop_front();
        }
        while said.len() > SPOKEN_KEPT {
            said.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_echo_needs_two_words_and_most_of_them() {
        let spoken = "Il fera vingt-quatre degrés ce matin et vingt-sept cette après-midi.";
        // What the microphone gives back is a mangled subset, never a copy.
        assert!(is_self_echo("il fera vingt quatre degrés ce matin", spoken));
        assert!(is_self_echo("vingt sept cette après midi", spoken));
        // A real request that happens to share a word is not an echo.
        assert!(!is_self_echo("et pour demain matin à Lyon", spoken));
        // One word is never enough to veto on.
        assert!(!is_self_echo("matin", spoken));
    }

    /// The headphone case, which is the common one and was nearly left out.
    #[test]
    fn with_no_echo_path_every_voice_interrupts() {
        let gate = Gate::without_echo(0.01);
        // Nothing is playing — the reference is empty — and somebody speaks.
        let mic = vec![0.0005, 0.05, 0.06, 0.0004];
        let verdicts = gate.classify(&mic, &[]);
        assert_eq!(verdicts[0].0, Verdict::Quiet);
        assert_eq!(verdicts[1].0, Verdict::Barge);
        assert_eq!(verdicts[2].0, Verdict::Barge);
        assert_eq!(verdicts[3].0, Verdict::Quiet);

        // And it stays Barge even while the reference is loud, because on
        // headphones a loud reference reaches nobody's microphone.
        let playing = vec![0.3f32; 4];
        assert_eq!(gate.classify(&mic, &playing)[1].0, Verdict::Barge);
    }

    #[test]
    fn a_locked_delay_is_the_delay_that_was_applied() {
        let reference: Vec<f32> = (0..200)
            .map(|t| if (40..90).contains(&t) { 0.3 } else { 0.001 })
            .collect();
        let lag = 17;
        let mut mic = vec![0.001f32; lag];
        mic.extend(reference.iter().map(|r| r * 0.05));
        let (found, rho) = lock_delay(&mic, &reference, 50);
        assert_eq!(found, lag);
        assert!(rho > 0.99, "{rho}");
        // 0.05 in amplitude is −26 dB of coupling, and that is what comes back.
        let coupling = coupling_db(&mic, &reference, found).expect("enough loud frames");
        assert!((coupling + 26.0).abs() < 1.0, "{coupling}");
    }

    /// A deterministic, uneven envelope.
    ///
    /// Uneven on purpose: a flat envelope correlates with anything, so a lock
    /// found on one would prove nothing about the search that found it.
    fn speechlike(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let i = i as f32;
                0.15 + 0.25 * ((i * 0.7).sin() * (i * 0.13).cos()).abs()
            })
            .collect()
    }

    /// Headphones: the speakers are loud, the microphone hears nothing of it.
    ///
    /// The case `Gate::without_echo` was written for, now reached on its own
    /// rather than chosen by hand. Once there, somebody speaking *is* an
    /// interruption, and ARIA stops.
    #[test]
    fn headphones_make_every_voice_an_interruption() {
        let reference = speechlike(120);
        let mut live = Live::new(0.01, 12.0);
        for t in 0..60 {
            assert!(!live.push(0.0005, &reference[..=t], true), "frame {t}");
        }
        assert_eq!(live.coupling(), Coupling::Absent);

        let mut barged = None;
        for t in 60..70 {
            if live.push(0.3, &reference[..=t], true) && barged.is_none() {
                barged = Some(t);
            }
        }
        assert_eq!(barged, Some(60 + BARGE_FRAMES - 1));
    }

    /// Speakers: the microphone hears a delayed, quieter copy of the answer.
    ///
    /// The failure this forbids is the one that matters most — ARIA hearing
    /// itself, calling it an interruption, and falling silent every time it
    /// opens its mouth. Nothing but echo goes in, so nothing may come out.
    #[test]
    fn a_locked_room_does_not_interrupt_itself() {
        let reference = speechlike(200);
        let lag = 7usize;
        let mut live = Live::new(0.01, 12.0);

        for t in 0..reference.len() {
            let echo = if t >= lag {
                reference[t - lag] * 0.2
            } else {
                0.0
            };
            assert!(
                !live.push(echo + 0.003, &reference[..=t], true),
                "ARIA interrupted itself at frame {t}"
            );
        }
        match live.coupling() {
            Coupling::Locked {
                lag: found,
                coupling_db,
            } => {
                assert_eq!(found, lag, "the delay search missed");
                // 0.2 in amplitude is −14 dB, plus the noise floor.
                assert!((coupling_db + 14.0).abs() < 2.0, "{coupling_db}");
            }
            other => panic!("no lock: {other:?}"),
        }
    }

    /// And the other half: a second voice over that same echo is heard.
    ///
    /// A gate that never interrupts passes the test above by doing nothing.
    /// This is what tells the two apart.
    #[test]
    fn a_voice_over_the_echo_is_an_interruption() {
        let reference = speechlike(200);
        let lag = 7usize;
        let mut live = Live::new(0.01, 12.0);
        let mut barged = None;

        for t in 0..reference.len() {
            let echo = if t >= lag {
                reference[t - lag] * 0.2
            } else {
                0.0
            };
            let voice = if (150..160).contains(&t) { 0.5 } else { 0.0 };
            if live.push(echo + voice + 0.003, &reference[..=t], true) && barged.is_none() {
                barged = Some(t);
            }
        }
        assert_eq!(barged, Some(150 + BARGE_FRAMES - 1));
    }

    /// Nothing has been played, so nothing has been learned.
    ///
    /// The safety property in one test: an unresolved gate cuts nothing, ever.
    /// Being wrong here would stop ARIA mid-sentence for a reason the user
    /// cannot see, which is worse than the latency this gate removes — and the
    /// transcript path still catches the interruption at the next pause.
    #[test]
    fn an_unresolved_gate_cuts_nothing() {
        let mut live = Live::new(0.01, 12.0);
        for _ in 0..20 {
            assert!(!live.push(0.3, &[], true));
        }
        assert_eq!(live.coupling(), Coupling::Unresolved);
        assert_eq!(live.verdict(), None);
    }

    /// A loud microphone while ARIA is silent is somebody taking their turn.
    ///
    /// Frames are still fed, so the envelopes stay aligned and the lock
    /// survives the pause — but there is nothing playing to interrupt.
    #[test]
    fn a_silent_player_is_never_interrupted() {
        let reference = speechlike(120);
        let mut live = Live::new(0.01, 12.0);
        for t in 0..60 {
            live.push(0.0005, &reference[..=t], true);
        }
        assert_eq!(live.coupling(), Coupling::Absent);
        for t in 60..90 {
            assert!(!live.push(0.3, &reference[..=t], false), "frame {t}");
        }
    }

    const SAID: &str = "Il fera vingt-quatre degrés ce matin et vingt-sept cette après-midi.";
    /// A room that returns what it is played — the state in which an echo can
    /// reach the microphone at all.
    const ROOM: Coupling = Coupling::Locked {
        lag: 22,
        coupling_db: -14.8,
    };

    /// The loop this whole module exists to break: ARIA's own sentence coming
    /// back through the ASR and being answered as a request.
    #[test]
    fn an_echo_of_what_was_just_said_is_vetoed() {
        let log = SpokenLog::new();
        log.push(SAID);
        assert!(log.vetoes("il fera vingt quatre degrés ce matin", ROOM));
    }

    /// And the other half: a gate that vetoes everything would pass the test
    /// above by doing nothing. This is what tells the two apart.
    #[test]
    fn a_real_request_is_not_vetoed() {
        let log = SpokenLog::new();
        log.push(SAID);
        assert!(!log.vetoes("et pour demain matin à Lyon", ROOM));
        // One shared word is never enough, whatever the room does.
        assert!(!log.vetoes("matin", ROOM));
    }

    /// **The case the veto matters most in**, and the one the acoustic gate
    /// cannot cover: for the first two seconds of every answer the coupling is
    /// `Unresolved`, so `Live` cuts nothing by design. The words are then the
    /// only evidence there is.
    #[test]
    fn an_unresolved_coupling_still_vetoes_on_words() {
        let log = SpokenLog::new();
        log.push(SAID);
        assert!(log.vetoes("vingt sept cette après midi", Coupling::Unresolved));
    }

    /// Headphones: nothing played reaches the microphone, so a transcript that
    /// repeats ARIA is the user repeating ARIA. Vetoing it would drop a real
    /// request, and nobody would ever find out why.
    #[test]
    fn nothing_is_vetoed_when_no_sound_comes_back() {
        let log = SpokenLog::new();
        log.push(SAID);
        assert!(!log.vetoes("il fera vingt quatre degrés ce matin", Coupling::Absent));
    }

    /// An old sentence stops being an explanation.
    ///
    /// Built forward from one instant rather than backward from `now`, so the
    /// arithmetic cannot underflow on a freshly started process.
    #[test]
    fn a_sentence_falls_out_of_memory() {
        let log = SpokenLog::new();
        let spoke = Instant::now();
        log.push_at(spoke, SAID);
        let heard = "il fera vingt quatre degrés ce matin";
        assert!(log.vetoes_at(spoke + Duration::from_secs(1), heard, ROOM));
        assert!(!log.vetoes_at(spoke + SPOKEN_MEMORY + Duration::from_secs(1), heard, ROOM));
    }

    /// Nothing said, nothing to attribute a transcript to.
    #[test]
    fn an_empty_log_vetoes_nothing() {
        let log = SpokenLog::new();
        assert!(!log.vetoes("il fera vingt quatre degrés ce matin", ROOM));
    }

    /// An echo does not respect the boundary between two sentences ARIA spoke
    /// in a row, so neither does the comparison.
    #[test]
    fn the_veto_spans_two_consecutive_sentences() {
        let log = SpokenLog::new();
        log.push("Il fera vingt-quatre degrés ce matin.");
        log.push("Vingt-sept cette après-midi.");
        assert!(log.vetoes("degrés ce matin vingt sept cette après midi", ROOM));
    }
}
