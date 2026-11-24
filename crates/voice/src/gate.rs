//! The energy gate: where the pauses are.
//!
//! It is not a voice activity detector and does not try to be. Its only job is
//! to say *here is a pause* so the turn detector has somewhere to be asked a
//! question — **the model is an arbiter, not a sensor**, and that is why Handy
//! ships a VAD at all.
//!
//! Written as a push state machine rather than a function over a buffer,
//! because that is the shape the live loop needs: frames arrive from the
//! microphone one at a time and nothing may be re-scanned. [`segments`] feeds a
//! whole buffer through the same machine, so the bench and the loop cannot
//! drift apart — there is one implementation.
//!
//! Two details are load-bearing, and both cost a day to find:
//!
//! - **a segment ends where the silence was *confirmed*, pause included.**
//!   Trimming back to the last loud frame removes the very cue the turn model
//!   is asked to judge: a suspended intonation trails off quietly. It scored
//!   three genuine cut-offs at 0.90 to 0.98;
//! - **the duration floor applies to the voiced span only.** Applied to the
//!   whole segment it filters nothing, since every segment carries the
//!   confirmed silence and so clears 300 ms by construction.

use crate::calibration::{Calibration, FRAME};

/// A stretch of speech, in sample offsets from the start of the stream,
/// **including** the silence that closed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub from: usize,
    pub to: usize,
}

impl Segment {
    pub fn samples(&self) -> usize {
        self.to - self.from
    }
}

/// Finds pauses in a stream of 20 ms frames.
#[derive(Debug)]
pub struct Segmenter {
    cal: Calibration,
    quiet_needed: usize,
    min_voiced_samples: usize,
    /// How many frames have been pushed. The stream clock.
    frames: usize,
    /// Frame index where the open segment started, if one is open.
    start: Option<usize>,
    last_speech: usize,
    quiet: usize,
}

impl Segmenter {
    pub fn new(cal: Calibration) -> Self {
        Self {
            cal,
            quiet_needed: Calibration::frames(cal.silence_ms).max(1),
            min_voiced_samples: Calibration::samples(cal.min_voiced_ms),
            frames: 0,
            start: None,
            last_speech: 0,
            quiet: 0,
        }
    }

    /// Samples consumed so far — the caller's cursor into its own buffer.
    pub fn position(&self) -> usize {
        self.frames * FRAME
    }

    /// Frames of quiet since the open segment last carried speech, or `None`
    /// when no segment is open.
    ///
    /// Zero means the last frame was speech. **One means this is the first
    /// quiet frame**, which is where [`crate::engine`] starts transcribing
    /// instead of waiting for the silence to be confirmed — the remaining
    /// [`Segmenter::quiet_needed`] frames are dead time otherwise, and
    /// transcription costs about as much as they last.
    ///
    /// Deliberately not `self.quiet` exposed bare. That counter is **not**
    /// reset when a segment closes, so read on its own it reports a stale run
    /// for every quiet frame that follows one — and a speculation keyed on it
    /// would fire in an empty room, once per frame, for ever. Gating on
    /// `start` is what makes the reading mean "inside a turn".
    pub fn quiet_run(&self) -> Option<usize> {
        self.start.map(|_| self.quiet)
    }

    /// How many quiet frames it takes to close a segment.
    ///
    /// Derived from [`Calibration::silence_ms`], and read rather than
    /// recomputed by the caller: two places deriving the same number from the
    /// same setting is two places to get it wrong when the setting changes.
    pub fn quiet_needed(&self) -> usize {
        self.quiet_needed
    }

    /// Feed one frame of [`FRAME`] samples. Returns a segment when this frame
    /// is the one that confirmed the silence.
    ///
    /// A short final frame is accepted so a stream can end anywhere.
    pub fn push(&mut self, frame: &[f32]) -> Option<Segment> {
        let energy: f32 = frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32;
        let speech = energy.sqrt() > self.cal.speech_rms;
        let i = self.frames;
        self.frames += 1;

        match (speech, self.start) {
            (true, None) => {
                self.start = Some(i);
                self.last_speech = i;
                self.quiet = 0;
                None
            }
            (true, Some(_)) => {
                self.last_speech = i;
                self.quiet = 0;
                None
            }
            (false, Some(from)) => {
                self.quiet += 1;
                if self.quiet < self.quiet_needed {
                    return None;
                }
                self.start = None;
                self.close(from, (i + 1) * FRAME)
            }
            (false, None) => None,
        }
    }

    /// Close whatever is still open, because the stream ended.
    pub fn flush(&mut self) -> Option<Segment> {
        let from = self.start.take()?;
        self.close(from, self.frames * FRAME)
    }

    fn close(&self, from_frame: usize, to: usize) -> Option<Segment> {
        let voiced = (self.last_speech + 1 - from_frame) * FRAME;
        (voiced >= self.min_voiced_samples).then_some(Segment {
            from: from_frame * FRAME,
            to,
        })
    }
}

/// Every segment in a complete buffer, through the same state machine the live
/// loop uses.
pub fn segments(samples: &[f32], cal: Calibration) -> Vec<Segment> {
    let mut segmenter = Segmenter::new(cal);
    let mut out: Vec<Segment> = samples
        .chunks(FRAME)
        .filter_map(|f| segmenter.push(f))
        .collect();
    out.extend(segmenter.flush());
    // The last segment may run past the buffer when the stream ended inside a
    // frame; the caller slices with it.
    for segment in &mut out {
        segment.to = segment.to.min(samples.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, level: f32) -> Vec<f32> {
        (0..frames * FRAME)
            .map(|i| level * if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect()
    }

    #[test]
    fn a_keystroke_is_not_a_turn() {
        let cal = Calibration::default();
        // 100 ms of loud transient, then two seconds of nothing: exactly the
        // Enter keypress that opened twelve recordings out of twenty and scored
        // 0.98 each time.
        let mut audio = tone(5, 0.3);
        audio.extend(tone(100, 0.0));
        assert!(
            segments(&audio, cal).is_empty(),
            "a 100 ms transient cleared the voiced floor"
        );
    }

    #[test]
    fn a_segment_keeps_the_silence_that_closed_it() {
        let cal = Calibration::default();
        let mut audio = tone(50, 0.1); // 1 s of speech
        audio.extend(tone(50, 0.0)); // then quiet
        let found = segments(&audio, cal);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].from, 0);
        // 50 frames of speech + the 15 that confirmed the silence.
        assert_eq!(found[0].to, 65 * FRAME);
    }

    #[test]
    fn a_breath_does_not_split_a_sentence() {
        let cal = Calibration::default();
        let mut audio = tone(40, 0.1);
        audio.extend(tone(5, 0.0)); // 100 ms gap, under the 300 ms rule
        audio.extend(tone(40, 0.1));
        audio.extend(tone(40, 0.0));
        assert_eq!(segments(&audio, cal).len(), 1);
    }

    #[test]
    fn pushing_frames_one_by_one_agrees_with_the_buffer() {
        let cal = Calibration::default();
        let mut audio = tone(30, 0.1);
        audio.extend(tone(20, 0.0));
        audio.extend(tone(30, 0.1));
        audio.extend(tone(20, 0.0));

        let mut live = Segmenter::new(cal);
        let mut streamed: Vec<Segment> = audio.chunks(FRAME).filter_map(|f| live.push(f)).collect();
        streamed.extend(live.flush());
        assert_eq!(streamed, segments(&audio, cal));
        assert_eq!(streamed.len(), 2);
    }

    /// What the speculation is keyed on, and the reading that would break it.
    ///
    /// [`Segmenter::quiet_run`] exists so `engine` can transcribe at the first
    /// quiet frame rather than 300 ms later. Two failures matter: a run
    /// reported outside a turn would transcribe an empty room once per frame,
    /// and a run that never reads exactly 1 would never transcribe at all.
    #[test]
    fn the_quiet_run_only_counts_inside_a_turn() {
        let cal = Calibration::default();
        let mut gate = Segmenter::new(cal);

        // An empty room. No segment is open, so there is no pause to be early
        // for — whatever the internal counter happens to hold.
        for _ in 0..50 {
            assert!(gate.push(&tone(1, 0.0)).is_none());
            assert_eq!(gate.quiet_run(), None);
        }

        // Speech opens one, and reads zero for as long as it is loud.
        for _ in 0..20 {
            assert!(gate.push(&tone(1, 0.1)).is_none());
            assert_eq!(gate.quiet_run(), Some(0));
        }

        // The first quiet frame, and it is the only one that reads 1.
        assert!(gate.push(&tone(1, 0.0)).is_none());
        assert_eq!(gate.quiet_run(), Some(1), "this is where the ASR starts");
        for expected in 2..gate.quiet_needed() {
            assert!(gate.push(&tone(1, 0.0)).is_none());
            assert_eq!(gate.quiet_run(), Some(expected));
        }

        // The frame that confirms the silence closes the segment, and there is
        // no longer a turn to be inside of.
        assert!(gate.push(&tone(1, 0.0)).is_some());
        assert_eq!(gate.quiet_run(), None);
    }

    /// A breath that turns back into speech must void the guess.
    ///
    /// Without this reading, a transcript written one frame into a 100 ms gap
    /// would be spent at the *next* pause — a sentence short of what was
    /// actually said, and the end-of-turn decision made on it.
    #[test]
    fn a_resumed_sentence_voids_the_guess() {
        let cal = Calibration::default();
        let mut gate = Segmenter::new(cal);
        for _ in 0..20 {
            gate.push(&tone(1, 0.1));
        }
        gate.push(&tone(1, 0.0));
        assert_eq!(gate.quiet_run(), Some(1));
        gate.push(&tone(1, 0.1));
        assert_eq!(
            gate.quiet_run(),
            Some(0),
            "speech resumed: nothing to spend"
        );
    }
}
