//! Loading Parakeet, in one place.
//!
//! Three binaries need the same 2.4 GB model and the same explicit ONNX
//! environment; three copies of that is three places to fix the day an
//! execution provider is chosen.

use anyhow::{Result, anyhow, bail};
use parakeet_rs::{ExecutionConfig, ExecutionProvider, ParakeetTDT};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Where the model lives — outside the repository, because it is 2.4 GB and it
/// is not ours. `ARIA_ASR_MODEL` overrides it.
pub fn default_model_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ARIA_ASR_MODEL") {
        return PathBuf::from(dir);
    }
    crate::models_root().join("parakeet-tdt-0.6b-v3")
}

/// Loads the TDT model, with the message that says what to download when it is
/// not there — the failure a newcomer hits first.
pub fn load(dir: &Path) -> Result<ParakeetTDT> {
    if !dir.join("encoder-model.onnx").is_file() {
        bail!(
            "no model in {}\n\
             Expected: encoder-model.onnx, encoder-model.onnx.data, \
             decoder_joint-model.onnx, vocab.txt\n\
             From: https://huggingface.co/altunenes/parakeet-rs, folder parakeet-tdt-0.6b-v3",
            dir.display()
        );
    }
    let gpu = gpu();
    // Always asked for the CPU, and the card's provider registered by us in
    // the configure hook (decision 131): `parakeet-rs` registers OpenVINO on
    // its default device, the CPU, and every provider without
    // `error_on_failure`. The stack is the loaded runtime's — `runtime.rs`.
    let mut config = ExecutionConfig::new()
        .with_environment(environment()?)
        .with_execution_provider(ExecutionProvider::Cpu);
    let stack = crate::runtime::stack();
    let on_card = gpu && crate::runtime::ready();
    let mut device = "le processeur".to_string();
    if on_card {
        config = config.with_custom_configure(move |builder| crate::runtime::register(builder, stack));
        device = format!("la carte graphique ({stack:?})");
    }
    let started = std::time::Instant::now();
    let model = match ParakeetTDT::from_pretrained(dir, Some(config)) {
        Ok(model) => model,
        // A card's provider may refuse the whole model rather than leave the
        // nodes it cannot run to the CPU: OpenVINO did, on 2026-11-27, and the
        // ear was gone. Slower beats deaf.
        Err(e) if on_card => {
            tracing::warn!("{device} refuse Parakeet, il est chargé sur le processeur : {e}");
            device = "le processeur, la carte l'ayant refusé".to_string();
            let cpu = ExecutionConfig::new()
                .with_environment(environment()?)
                .with_execution_provider(ExecutionProvider::Cpu);
            ParakeetTDT::from_pretrained(dir, Some(cpu))
                .map_err(|e| anyhow!("loading the model failed: {e}"))?
        }
        Err(e) => return Err(anyhow!("loading the model failed: {e}")),
    };
    tracing::info!(gpu, ms = started.elapsed().as_millis(), "Parakeet chargé sur {device}");
    Ok(model)
}

/// Whether the next [`load`] puts Parakeet on the graphics card.
///
/// Process-wide rather than a parameter, because the setting belongs to the
/// application and the loads happen deep in the voice thread, which already
/// re-reads what it needs before each one. Smart Turn is not concerned: its
/// model is the `-cpu` build, quantised for the processor, and it takes 16 ms.
static GPU: AtomicBool = AtomicBool::new(true);

pub fn set_gpu(on: bool) {
    GPU.store(on, Ordering::Relaxed);
}

pub fn gpu() -> bool {
    GPU.load(Ordering::Relaxed)
}

/// The one ONNX Runtime environment this process is allowed.
///
/// `ort::init().build()` refuses a second call outright — *"only one
/// environment is allowed per process"* — and ARIA runs **two** ONNX models: the
/// turn detector and the ASR. Found by running both in one binary, which is
/// exactly what the voice loop will do.
///
/// It is also the handle through which an execution provider is chosen later
/// (CUDA, DirectML) for everything at once, without touching any call site.
pub fn environment() -> Result<parakeet_rs::ort::environment::Environment> {
    static ENV: OnceLock<Mutex<Option<parakeet_rs::ort::environment::Environment>>> =
        OnceLock::new();
    let mut guard = ENV
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("the environment is only ever built, never panicked through");
    if guard.is_none() {
        // The runtime DLL first: with `load-dynamic`, which one is a choice
        // (decision 131), and it must be made before anything touches ONNX.
        crate::runtime::init()?;
        *guard = Some(
            parakeet_rs::ort::init()
                .build()
                .map_err(|e| anyhow!("ONNX Runtime refused to start: {e}"))?,
        );
    }
    Ok(guard.as_ref().expect("just built").clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parakeet_rs::Transcriber;

    /// The same utterance on the processor, then on the graphics card.
    ///
    /// `#[ignore]` because it reads Parakeet's 2.4 GB twice. Any 16 kHz mono
    /// WAV will do; the GPU is only worth its switch if this says so.
    ///
    /// ```powershell
    /// $env:ARIA_BENCH_WAV = "C:\...\phrase.wav"
    /// cargo test -p aria-voice --lib gpu_against_cpu -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "charge Parakeet deux fois"]
    fn gpu_against_cpu() {
        let wav = std::env::var("ARIA_BENCH_WAV").expect("ARIA_BENCH_WAV");
        let audio = crate::wav::read(Path::new(&wav)).expect("lu");
        println!("{:.1} s d'audio", crate::wav::seconds(&audio));
        for gpu in [false, true] {
            set_gpu(gpu);
            let started = std::time::Instant::now();
            let mut model = load(&default_model_dir()).expect("chargé");
            let loaded = started.elapsed();
            let mut runs = Vec::new();
            let mut text = String::new();
            // The first run pays the runtime's own warm-up; it is shown apart.
            for _ in 0..6 {
                let started = std::time::Instant::now();
                text = model
                    .transcribe_samples(audio.clone(), crate::wav::SAMPLE_RATE, 1, None)
                    .expect("transcrit")
                    .text;
                runs.push(started.elapsed().as_millis());
            }
            let mut steady = runs[1..].to_vec();
            steady.sort();
            // Every utterance has its own length. A provider that tunes per
            // shape (cuDNN's algorithm search) pays again on each one, and a
            // bench that repeats one file never sees it.
            let lengths: Vec<u128> = [0.55f32, 0.7, 0.85]
                .iter()
                .map(|share| {
                    let cut = audio[..(audio.len() as f32 * share) as usize].to_vec();
                    let started = std::time::Instant::now();
                    model
                        .transcribe_samples(cut, crate::wav::SAMPLE_RATE, 1, None)
                        .expect("transcrit");
                    started.elapsed().as_millis()
                })
                .collect();
            println!("  autres longueurs : {lengths:?} ms");
            println!(
                "{} : chargé en {} ms, 1re passe {} ms, médiane {} ms\n  « {} »",
                if gpu { "carte graphique" } else { "processeur" },
                loaded.as_millis(),
                runs[0],
                steady[steady.len() / 2],
                text.trim()
            );
        }
    }
}
