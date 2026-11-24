//! Log-mel features, Whisper convention, because that is what Smart Turn v3 eats.
//!
//! Its graph asks for `[batch, 80, 800]`: 80 mel bands over 800 frames of 10 ms,
//! an **8-second window**. Nothing documents the convention behind those numbers,
//! so it is reproduced here from the reference implementation, step for step:
//!
//! - 400-point periodic Hann window, hop 160, reflect-padded by 200 so the STFT
//!   is centred — that is what yields 801 frames for 8 s, of which the last is
//!   dropped;
//! - power spectrum, then an 80-band mel filter bank on the Slaney scale with
//!   Slaney normalisation (`librosa`'s `htk=False`, `norm="slaney"`);
//! - `log10` clamped at 1e-10, then the two normalisations that are easy to
//!   forget and impossible to notice: floor at `max - 8`, then `(x + 4) / 4`.
//!
//! **A wrong convention raises no error.** It returns plausible, useless numbers.
//! The only honest check is behavioural: finished sentences must score high and
//! cut-off ones low, on labelled audio.

use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const N_FFT: usize = 400;
pub const HOP: usize = 160;
pub const N_MELS: usize = 80;
const N_FREQS: usize = N_FFT / 2 + 1;
const SAMPLE_RATE: f32 = 16_000.0;

/// Smart Turn v3's window, in frames and in samples.
pub const WINDOW_FRAMES: usize = 800;
pub const WINDOW_SAMPLES: usize = WINDOW_FRAMES * HOP;

pub struct Mel {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// `N_MELS × N_FREQS`, row-major.
    filters: Vec<f32>,
}

impl Default for Mel {
    fn default() -> Self {
        Self::new()
    }
}

impl Mel {
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        // Periodic, not symmetric: `torch.hann_window(400)` divides by N, not N-1.
        let window = (0..N_FFT)
            .map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / N_FFT as f32).cos())
            .collect();
        Self {
            fft,
            window,
            filters: mel_filters(),
        }
    }

    /// Features for exactly [`WINDOW_SAMPLES`] of audio, as `N_MELS × WINDOW_FRAMES`
    /// row-major by mel band. Shorter input is zero-padded on the left by the
    /// caller, which is what "the first seconds of a conversation" look like.
    pub fn features(&self, samples: &[f32]) -> Vec<f32> {
        assert_eq!(samples.len(), WINDOW_SAMPLES, "window must be 8 s");
        self.features_over(samples, WINDOW_FRAMES)
    }

    /// The same features over a window of any length: `frames * HOP` samples
    /// in, `N_MELS × frames` out.
    ///
    /// The wake-word detector reads one second, ten times a second, and the
    /// eight-second FFT would be eighty times the useful work. **The same code,
    /// not a copy**: the detector is trained in `ariawakeword` against this
    /// crate, and the last step normalises by the window's own maximum, so the
    /// window's length is part of the convention.
    pub fn features_over(&self, samples: &[f32], frames: usize) -> Vec<f32> {
        assert_eq!(samples.len(), frames * HOP, "window must be {frames} frames of {HOP}");

        let pad = N_FFT / 2;
        let mut padded = Vec::with_capacity(samples.len() + 2 * pad);
        padded.extend((1..=pad).rev().map(|i| samples[i]));
        padded.extend_from_slice(samples);
        padded.extend((1..=pad).map(|i| samples[samples.len() - 1 - i]));

        let mut frame = self.fft.make_input_vec();
        let mut spectrum = self.fft.make_output_vec();
        // Frequency-major: the mel product below reads one frequency row at a
        // time across every frame.
        let mut power = vec![0f32; N_FREQS * frames];
        for t in 0..frames {
            let start = t * HOP;
            for i in 0..N_FFT {
                frame[i] = padded[start + i] * self.window[i];
            }
            self.fft
                .process(&mut frame, &mut spectrum)
                .expect("sizes are fixed at construction");
            for (f, value) in spectrum.iter().enumerate() {
                power[f * frames + t] = value.norm_sqr();
            }
        }

        let mut out = vec![0f32; N_MELS * frames];
        for m in 0..N_MELS {
            let row = &self.filters[m * N_FREQS..(m + 1) * N_FREQS];
            let dst = &mut out[m * frames..(m + 1) * frames];
            for (f, &weight) in row.iter().enumerate() {
                if weight == 0.0 {
                    continue;
                }
                let band = &power[f * frames..(f + 1) * frames];
                for (o, &p) in dst.iter_mut().zip(band) {
                    *o += weight * p;
                }
            }
            for value in dst.iter_mut() {
                *value = value.max(1e-10).log10();
            }
        }

        let max = out.iter().copied().fold(f32::MIN, f32::max);
        for value in &mut out {
            *value = (value.max(max - 8.0) + 4.0) / 4.0;
        }
        out
    }
}

/// Slaney mel scale: linear below 1 kHz, logarithmic above.
fn hz_to_mel(hz: f32) -> f32 {
    const F_SP: f32 = 200.0 / 3.0;
    const MIN_LOG_HZ: f32 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f32.ln() / 27.0;
    if hz < MIN_LOG_HZ {
        hz / F_SP
    } else {
        min_log_mel + (hz / MIN_LOG_HZ).ln() / logstep
    }
}

fn mel_to_hz(mel: f32) -> f32 {
    const F_SP: f32 = 200.0 / 3.0;
    const MIN_LOG_HZ: f32 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f32.ln() / 27.0;
    if mel < min_log_mel {
        mel * F_SP
    } else {
        MIN_LOG_HZ * ((mel - min_log_mel) * logstep).exp()
    }
}

/// Triangular filters between `N_MELS + 2` equally spaced mel points, each scaled
/// by `2 / (hi - lo)` — Slaney normalisation, which keeps band energy comparable
/// across the spectrum.
fn mel_filters() -> Vec<f32> {
    let (f_min, f_max) = (0.0f32, 8_000.0f32);
    let (mel_min, mel_max) = (hz_to_mel(f_min), hz_to_mel(f_max));
    let points: Vec<f32> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(mel_min + (mel_max - mel_min) * i as f32 / (N_MELS + 1) as f32))
        .collect();
    let freqs: Vec<f32> = (0..N_FREQS)
        .map(|i| i as f32 * SAMPLE_RATE / N_FFT as f32)
        .collect();

    let mut filters = vec![0f32; N_MELS * N_FREQS];
    for m in 0..N_MELS {
        let (lo, centre, hi) = (points[m], points[m + 1], points[m + 2]);
        let enorm = 2.0 / (hi - lo);
        for (f, &freq) in freqs.iter().enumerate() {
            let rising = (freq - lo) / (centre - lo);
            let falling = (hi - freq) / (hi - centre);
            filters[m * N_FREQS + f] = rising.min(falling).max(0.0) * enorm;
        }
    }
    filters
}
