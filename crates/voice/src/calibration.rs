//! The five numbers that are not constants.
//!
//! Every value here was a hard-coded guess until it was measured, and the
//! measurements said the same thing each time: **they belong to the person and
//! their hardware, not to the code**. Decision 38.
//!
//! | Value | Depends on |
//! |---|---|
//! | [`speech_rms`](Calibration::speech_rms) | microphone, gain, room noise |
//! | [`silence`](Calibration::silence) | the speaker — a slow one pauses past 300 ms mid-sentence |
//! | [`min_voiced`](Calibration::min_voiced) | the noise floor, really: it is what rejects a keystroke |
//! | [`turn_threshold`](Calibration::turn_threshold) | how the voice falls on a suspended word |
//! | [`echo_margin_db`](Calibration::echo_margin_db) | speakers, volume, room |
//!
//! **The defaults ship and work.** They are measured, not invented, and
//! calibration improves them rather than enabling anything — nobody should have
//! to read twenty sentences before the assistant starts.
//!
//! Stored per **input device**, never globally: a threshold measured on a
//! headset is wrong on laptop speakers, and a chain that silently degrades when
//! a headset is plugged in is a fault nobody ever diagnoses.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 20 ms at 16 kHz, and the one frame size the whole crate works in.
///
/// Short enough that barge-in is noticed under 100 ms, long enough that one
/// glottal pulse decides nothing.
pub const FRAME: usize = 320;

/// What the voice chain was tuned to, for one input device.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calibration {
    /// Speech floor in RMS. Crude on purpose: the gate only has to find pauses,
    /// the turn detector decides whether one ends a turn.
    ///
    /// The default is almost certainly wrong for any given microphone, and the
    /// measurement says how wrong: one speaker's voice peaked at 0.05–0.08 while
    /// a keystroke on the same microphone hit 0.26.
    pub speech_rms: f32,

    /// How much silence closes a segment, in ms. Below this, a breath cuts a
    /// sentence.
    ///
    /// Milliseconds rather than a `Duration` because this file is TOML that a
    /// person is meant to read and edit — decision 24 — and a serialised
    /// `Duration` lands as `{ secs = 0, nanos = 300000000 }`.
    pub silence_ms: u64,

    /// How much **voiced** audio a segment needs to be worth judging.
    ///
    /// Measured on the voiced span alone, and that is the whole point: the first
    /// version measured the segment with its confirmed silence included, so
    /// every segment cleared the floor by construction and it filtered nothing.
    /// The Enter keypress went through at the head of twelve recordings out of
    /// twenty, each scored 0.98.
    pub min_voiced_ms: u64,

    /// Above this probability the turn is over — subject to the transcript
    /// having a terminal mark at all. See [`crate::turn`].
    ///
    /// 0.85 measured on twenty labelled utterances. It is not a delicate
    /// setting: the combined rule holds 17/20 flat from 0.05 to 0.80 and 18/20
    /// from 0.85 to 0.95, so this sits on a plateau rather than on a slope.
    pub turn_threshold: f32,

    /// How many dB above the predicted echo counts as another voice.
    ///
    /// Has to clear the worst echo-only frame or ARIA interrupts itself.
    ///
    /// **Measured on 2026-11-04**, the first real acoustic path the project has
    /// had: laptop loudspeakers into a worn headset microphone, leaning over
    /// them on purpose to get the unfavourable case. Lock at 22 frames
    /// (440 ms), correlation 0.94, coupling −14.8 dB, and the worst echo-only
    /// frame 4.9 dB above what the coupling predicts — so 8 dB is that worst
    /// frame plus the 3 dB of safety `bin/echo.rs` adds. Swept against a
    /// double-talk recording of the same posture: **zero false positives over
    /// 249 echo-only frames, and six consecutive voice frames**, twice
    /// [`crate::echo::BARGE_FRAMES`].
    ///
    /// It was 12 dB before, and 12 dB was not broken — it left five consecutive
    /// frames, so it still cut. It was simply generous while nothing had been
    /// measured, and generosity here is paid in interruptions that go
    /// unnoticed: an interrupting voice had to clear the echo by 12 dB.
    pub echo_margin_db: f32,
}

impl Default for Calibration {
    fn default() -> Self {
        Self {
            speech_rms: 0.01,
            silence_ms: 300,
            min_voiced_ms: 250,
            turn_threshold: 0.85,
            echo_margin_db: 8.0,
        }
    }
}

impl Calibration {
    /// Is this set of values sane enough to use?
    ///
    /// The guard behind decision 38: a calibration must never make the chain
    /// worse than the shipped default. Six labelled utterances can easily
    /// produce a degenerate threshold — every one read with the same
    /// intonation, say — and the honest answer then is to keep the default and
    /// say so, not to apply a number because it came out of a measurement.
    pub fn is_plausible(&self) -> bool {
        (0.0005..0.2).contains(&self.speech_rms)
            && (150..1500).contains(&self.silence_ms)
            && (80..800).contains(&self.min_voiced_ms)
            && (0.3..0.99).contains(&self.turn_threshold)
            && (3.0..30.0).contains(&self.echo_margin_db)
    }

    /// How much silence closes a segment.
    pub fn silence(&self) -> Duration {
        Duration::from_millis(self.silence_ms)
    }

    /// How much voiced audio a segment needs to be worth judging.
    pub fn min_voiced(&self) -> Duration {
        Duration::from_millis(self.min_voiced_ms)
    }

    /// Milliseconds as a whole number of [`FRAME`]s, which is how the gate counts.
    pub(crate) fn frames(ms: u64) -> usize {
        (ms as usize * crate::wav::SAMPLE_RATE as usize / 1000) / FRAME
    }

    pub(crate) fn samples(ms: u64) -> usize {
        ms as usize * crate::wav::SAMPLE_RATE as usize / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_default_passes_its_own_guard() {
        assert!(Calibration::default().is_plausible());
    }

    #[test]
    fn a_degenerate_calibration_is_refused() {
        // What six utterances read with one intonation would produce: nothing
        // separates, so the sweep pushes the threshold to an extreme.
        let runaway = Calibration {
            turn_threshold: 0.999,
            ..Default::default()
        };
        assert!(!runaway.is_plausible());

        // And a microphone whose floor was measured during a lorry going past.
        let deaf = Calibration {
            speech_rms: 0.5,
            ..Default::default()
        };
        assert!(!deaf.is_plausible());
    }

    #[test]
    fn three_hundred_milliseconds_is_fifteen_frames() {
        assert_eq!(Calibration::frames(300), 15);
        assert_eq!(Calibration::samples(300), 4800);
    }
}
