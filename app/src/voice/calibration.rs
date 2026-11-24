//! Measuring the microphone, through the chain that already holds it.
//!
//! The first tier of decision 38. `speech_rms` is the one calibration number
//! that depends on the room and the gain rather than on the voice, and getting
//! it wrong is **silent from both ends**: too high and nothing is ever heard,
//! too low and the gate never finds a pause, so it never closes a segment and
//! never transcribes.
//!
//! # Why this is not a second recorder
//!
//! `dictate --noise` opens its own, and that is right for a command line run
//! with the application closed. Here the voice chain already owns the
//! microphone. Opening a second capture on the same device is a request the
//! backend may refuse, and on a machine where it does not refuse it is two
//! streams competing for the same buffer — a measurement of the wrong thing.
//!
//! So a request is left where the listening loop will see it, the loop
//! collects the frames it is already receiving, and the answer comes back on a
//! channel. The loop does not change shape for this: it only hands a copy of
//! each buffer to a collector that is usually not there.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aria_voice::calibration::{Calibration, FRAME};
use serde::Serialize;

/// What a measurement found, and what it suggests doing about it.
#[derive(Clone, Debug, Serialize)]
pub struct Noise {
    /// The input device the numbers belong to. The calibration is stored under
    /// it, never globally — decision 38.
    pub device: String,
    pub frames: usize,
    pub median: f32,
    pub p90: f32,
    /// The worst of the background. What a threshold has to clear.
    pub p99: f32,
    /// The shipped default, for the screen to compare against.
    pub default_threshold: f32,
    pub suggested: f32,
    /// One sentence, in French, saying what this means.
    pub verdict: String,
}

/// A measurement in flight. Left where the listening loop will find it.
pub struct Request {
    until: Instant,
    collected: Vec<f32>,
    answer: std::sync::mpsc::Sender<Noise>,
}

/// The slot the loop checks and the interface fills.
#[derive(Clone, Default)]
pub struct Pending(Arc<Mutex<Option<Request>>>);

impl Pending {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for a measurement and wait for it.
    ///
    /// Returns an error when nothing is listening — which is the honest
    /// answer, and the one the screen needs: a button that silently does
    /// nothing because the microphone is off is a button people press twice.
    pub fn measure(&self, seconds: f32, device: String) -> Result<Noise, String> {
        let (answer, wait) = std::sync::mpsc::channel();
        {
            let mut slot = self.0.lock().expect("calibration poisoned");
            if slot.is_some() {
                return Err("une mesure est déjà en cours".into());
            }
            *slot = Some(Request {
                until: Instant::now() + Duration::from_secs_f32(seconds),
                collected: Vec::new(),
                answer,
            });
        }
        // The margin is for a chain that is loading its models: the loop will
        // not look at the slot until it is capturing again.
        wait.recv_timeout(Duration::from_secs_f32(seconds + 10.0))
            .map(|mut noise| {
                noise.device = device;
                noise
            })
            .map_err(|_| {
                self.0.lock().expect("calibration poisoned").take();
                "le micro n'écoute pas — ouvre le canal vocal d'abord".to_string()
            })
    }

    /// Hand the loop's samples to a measurement, if one is waiting.
    ///
    /// Called on every buffer. Cheap when nothing is pending, which is almost
    /// always: one uncontended lock and a branch.
    pub fn feed(&self, samples: &[f32]) {
        let mut slot = self.0.lock().expect("calibration poisoned");
        let Some(request) = slot.as_mut() else {
            return;
        };
        request.collected.extend_from_slice(samples);
        if Instant::now() < request.until {
            return;
        }
        let request = slot.take().expect("just checked");
        let _ = request.answer.send(report(&request.collected));
    }
}

/// The percentiles, and what they say.
fn report(samples: &[f32]) -> Noise {
    let mut rms: Vec<f32> = samples
        .chunks(FRAME)
        .filter(|frame| frame.len() == FRAME)
        .map(|frame| (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt())
        .collect();
    rms.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in audio"));

    let at = |q: f32| {
        if rms.is_empty() {
            0.0
        } else {
            rms[((rms.len() - 1) as f32 * q) as usize]
        }
    };
    let p99 = at(0.99);
    // Above the noise, below a voice. Three times the loudest background frame
    // is what kept a fan, a fridge and a keyboard out on the bench; the floor
    // exists because a threshold under it is a gate that never closes.
    let suggested = (p99 * 3.0).max(0.002);
    let default_threshold = Calibration::default().speech_rms;

    let verdict = if rms.is_empty() {
        "Rien n'a été capturé — le micro est-il ouvert ?".to_string()
    } else if suggested > default_threshold * 1.5 {
        "Ton micro est trop bruyant pour le seuil par défaut : la porte ne trouverait \
         jamais de silence, donc rien ne serait transcrit."
            .to_string()
    } else if suggested < default_threshold * 0.5 {
        "Micro très propre. Le défaut marche ; plus bas rendrait l'écoute plus vive.".to_string()
    } else {
        "Le seuil par défaut convient.".to_string()
    };

    Noise {
        device: String::new(),
        frames: rms.len(),
        median: at(0.50),
        p90: at(0.90),
        p99,
        default_threshold,
        suggested,
        verdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean microphone: the shipped default is far above the floor, and the
    /// verdict says so instead of proposing a change nobody needs.
    #[test]
    fn a_quiet_room_is_told_the_default_is_fine() {
        let quiet: Vec<f32> = (0..FRAME * 100)
            .map(|i| if i % 7 == 0 { 0.0008 } else { -0.0006 })
            .collect();
        let noise = report(&quiet);
        assert_eq!(noise.frames, 100);
        assert!(noise.p99 < noise.default_threshold);
        assert!(noise.verdict.contains("propre"), "{}", noise.verdict);
    }

    /// A noisy one: the symptom is a gate that never closes, and the verdict
    /// names it rather than printing a number and leaving the reader to guess.
    #[test]
    fn a_noisy_room_is_told_why_nothing_would_be_transcribed() {
        let noisy: Vec<f32> = (0..FRAME * 50)
            .map(|i| if i % 2 == 0 { 0.05 } else { -0.05 })
            .collect();
        let noise = report(&noisy);
        assert!(noise.suggested > noise.default_threshold);
        assert!(
            noise.verdict.contains("jamais de silence"),
            "{}",
            noise.verdict
        );
    }

    /// A threshold must never be proposed below the floor that makes the gate
    /// close at all, whatever the room.
    #[test]
    fn the_suggestion_never_goes_under_the_floor() {
        assert_eq!(report(&[0.0; FRAME * 10]).suggested, 0.002);
    }

    #[test]
    fn nothing_captured_says_so_rather_than_suggesting_a_number() {
        let noise = report(&[]);
        assert_eq!(noise.frames, 0);
        assert!(noise.verdict.contains("Rien n'a été capturé"));
    }
}
