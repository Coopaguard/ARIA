//! WAV in and out, at the one format the whole chain agrees on: 16 kHz, mono.
//!
//! This is bench equipment, not product code. It exists so that a turn detector,
//! an ASR or a whole pipeline can be judged by **replaying the same utterances**
//! instead of asking someone to speak into a microphone twenty times.
//!
//! 16-bit PCM on disk, `f32` in memory. The quantisation is inaudible to an ASR
//! and it makes the files readable by every other tool we might want to compare
//! against — which is the entire point of writing them down.

use anyhow::{Context, Result, bail};
use std::path::Path;

pub const SAMPLE_RATE: u32 = 16_000;

/// Writes 16 kHz mono samples in `[-1, 1]`.
pub fn write(path: &Path, samples: &[f32]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("cannot write {}", path.display()))?;
    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        writer.write_sample((clamped * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Reads a 16 kHz mono file back into `f32`. Refuses anything else rather than
/// resampling silently: a bench that quietly changes its input is worse than one
/// that stops.
pub fn read(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let spec = reader.spec();

    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 {
        bail!(
            "{} is {} Hz, {} channel(s) — wanted {SAMPLE_RATE} Hz mono.\n\
             Convert it: ffmpeg -i <in> -ar {SAMPLE_RATE} -ac 1 <out>.wav",
            path.display(),
            spec.sample_rate,
            spec.channels
        );
    }

    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .context("malformed float samples")?,
        hound::SampleFormat::Int => {
            let scale = match spec.bits_per_sample {
                16 => i16::MAX as f32,
                b => bail!("{b}-bit integer samples are not handled; use 16-bit or float"),
            };
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .context("malformed integer samples")?
                .into_iter()
                .map(|s| s as f32 / scale)
                .collect()
        }
    };
    Ok(samples)
}

/// Seconds of audio in a 16 kHz mono buffer.
pub fn seconds(samples: &[f32]) -> f32 {
    samples.len() as f32 / SAMPLE_RATE as f32
}
