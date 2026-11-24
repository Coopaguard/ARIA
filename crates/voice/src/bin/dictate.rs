//! T1 — dictation, and the bench equipment T2 needs.
//!
//! ```bash
//! cargo run --release -p aria-voice --bin dictate
//! cargo run --release -p aria-voice --bin dictate -- --save
//! cargo run --release -p aria-voice --bin dictate -- --wav samples/utterance-01.wav
//! cargo run --release -p aria-voice --bin dictate -- --bench 2.7
//! ```
//!
//! `--save` is what makes T2 possible: a turn detector is chosen by replaying the
//! same utterances, not by asking someone to speak into a microphone twenty times.

use anyhow::{Result, anyhow, bail};
use aria_voice::calibration::{Calibration, FRAME};
use aria_voice::wav;
use handy_recorder::{CollectingSink, Recorder, RecorderConfig};
use parakeet_rs::{ExecutionConfig, ParakeetTDT, TimestampMode, Transcriber};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// How many times `--bench` transcribes its synthetic buffer.
const BENCH_RUNS: usize = 5;

enum Mode {
    /// Speak into the microphone, one utterance at a time.
    Interactive,
    /// Transcribe a file already on disk.
    File(PathBuf),
    /// Time the compute path on a synthetic tone, no microphone, no human.
    Bench(f32),
    /// Measure what this microphone does when nobody is speaking.
    Noise(f32),
}

struct Args {
    model: PathBuf,
    mode: Mode,
    /// Where captured utterances are written, if anywhere.
    save: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut model = None;
    let mut mode = Mode::Interactive;
    let mut save = None;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--wav" => {
                i += 1;
                let path = argv.get(i).ok_or_else(|| anyhow!("--wav wants a path"))?;
                mode = Mode::File(PathBuf::from(path));
            }
            "--bench" => {
                // The value is optional: 2.7 s is the length of the first real
                // dictation, so the numbers stay comparable.
                let seconds = argv.get(i + 1).and_then(|v| v.parse::<f32>().ok());
                if seconds.is_some() {
                    i += 1;
                }
                mode = Mode::Bench(seconds.unwrap_or(2.7));
            }
            "--noise" => {
                let seconds = argv.get(i + 1).and_then(|v| v.parse::<f32>().ok());
                if seconds.is_some() {
                    i += 1;
                }
                mode = Mode::Noise(seconds.unwrap_or(5.0));
            }
            "--save" => {
                // The directory is optional: without one, utterances land in the
                // project's `samples/`, which is where every bench here looks.
                match argv.get(i + 1) {
                    Some(value) if !value.starts_with("--") => {
                        i += 1;
                        save = Some(PathBuf::from(value));
                    }
                    _ => save = Some(aria_voice::samples_dir()),
                }
            }
            "-h" | "--help" => {
                println!("{}", include_str!("dictate-help.txt"));
                std::process::exit(0);
            }
            other if other.starts_with("--") => bail!("unknown argument: {other}"),
            positional => model = Some(PathBuf::from(positional)),
        }
        i += 1;
    }

    Ok(Args {
        model: model.unwrap_or_else(default_model_dir),
        mode,
        save,
    })
}

/// Models live outside the repository — they are 2.4 GB and they are not ours.
///
/// The library's own resolution, so this bench and the product cannot drift
/// apart on where the weights are.
fn default_model_dir() -> PathBuf {
    aria_voice::asr::default_model_dir()
}

fn prompt(line: &str) -> Result<String> {
    print!("{line}");
    std::io::stdout().flush()?;
    let mut buffer = String::new();
    std::io::stdin().lock().read_line(&mut buffer)?;
    Ok(buffer.trim().to_string())
}

/// Loads the model, timed: this is the cost of waking the voice chain, and the
/// standby policy will be argued from this number.
fn load(dir: &Path) -> Result<ParakeetTDT> {
    if !dir.join("encoder-model.onnx").is_file() {
        bail!(
            "no model in {}\n\
             Expected: encoder-model.onnx, encoder-model.onnx.data, \
             decoder_joint-model.onnx, vocab.txt\n\
             From: https://huggingface.co/altunenes/parakeet-rs, folder parakeet-tdt-0.6b-v3",
            dir.display()
        );
    }
    println!("modèle  {}", dir.display());
    let started = Instant::now();
    // The environment is explicit on purpose, and the crate insists: it is the
    // handle through which an execution provider is chosen later (CUDA, DirectML)
    // without touching anything else here.
    let environment = parakeet_rs::ort::init()
        .build()
        .map_err(|e| anyhow!("ONNX Runtime refused to start: {e}"))?;
    let execution = ExecutionConfig::new().with_environment(environment);
    let asr = ParakeetTDT::from_pretrained(dir, Some(execution))
        .map_err(|e| anyhow!("loading the model failed: {e}"))?;
    println!("chargé  {} ms", started.elapsed().as_millis());
    Ok(asr)
}

/// One transcription, and what it cost.
fn transcribe(asr: &mut ParakeetTDT, samples: Vec<f32>) -> Result<(String, u128)> {
    let started = Instant::now();
    let result = asr
        .transcribe_samples(samples, wav::SAMPLE_RATE, 1, Some(TimestampMode::Sentences))
        .map_err(|e| anyhow!("transcription failed: {e}"))?;
    Ok((
        result.text.trim().to_string(),
        started.elapsed().as_millis().max(1),
    ))
}

fn report(seconds: f32, text: &str, ms: u128) {
    println!(
        "  {seconds:.1} s d'audio, transcrites en {ms} ms — {:.0}× temps réel",
        seconds * 1000.0 / ms as f32
    );
    if text.is_empty() {
        println!("  « » — rien reconnu\n");
    } else {
        println!("  « {text} »\n");
    }
}

/// What this microphone does when nobody is speaking.
///
/// The missing half of decision 38. `speech_rms` is the one calibration number
/// that depends on the room and the gain rather than on the voice, and until
/// now there was no way to read it: the symptom of getting it wrong is
/// **silence**, from both ends. Too high and nothing is ever heard; too low and
/// the gate never finds a pause, so it never closes a segment, never
/// transcribes, and the log says `no pause in a whole turn's worth of audio`.
///
/// No model is loaded. This is arithmetic on the capture stream, so it runs in
/// a second and needs no 2.4 GB download.
fn noise(seconds: f32) -> Result<()> {
    let recorder: Recorder<CollectingSink> =
        Recorder::open(RecorderConfig::speech()).map_err(|e| anyhow!("micro : {e}"))?;
    let info = recorder.info();
    println!(
        "micro   {} ({} Hz → {} Hz)",
        info.device.name, info.device_format.sample_rate, info.format.sample_rate
    );
    println!("\nNe dis RIEN pendant {seconds:.0} secondes. Mesure en cours…");

    recorder
        .start(CollectingSink::new())
        .map_err(|e| anyhow!("{}", e.error))?;
    std::thread::sleep(std::time::Duration::from_secs_f32(seconds));
    let stopped = recorder.stop().map_err(|e| anyhow!("arrêt : {e}"))?;
    if !stopped.is_complete() {
        eprintln!(
            "  (incomplet : {:?}, {} trames perdues)",
            stopped.end_reason, stopped.dropped_frames
        );
    }
    let captured = stopped.sink.into_samples();

    if captured.is_empty() {
        bail!("rien n'a été capturé — le micro est-il autorisé ?");
    }

    // Per 20 ms frame, which is the unit the gate actually judges.
    let mut rms: Vec<f32> = captured
        .chunks(FRAME)
        .filter(|frame| frame.len() == FRAME)
        .map(|frame| (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt())
        .collect();
    rms.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in audio"));
    let at = |q: f32| rms[((rms.len() - 1) as f32 * q) as usize];

    let floor = at(0.50);
    let loud = at(0.99);
    println!("\n{} trames de 20 ms", rms.len());
    println!("  médiane   {floor:.5}");
    println!("  p90       {:.5}", at(0.90));
    println!("  p99       {loud:.5}   ← le pire du bruit de fond");

    // Above the noise, below a voice. A factor of three over the loudest
    // background frame is what kept a fan, a fridge and a keyboard out on the
    // bench; the floor exists because a threshold under it is a gate that
    // never closes.
    let suggested = (loud * 3.0).max(0.002);
    let default = Calibration::default().speech_rms;
    println!("\nseuil par défaut : {default:.4}");
    println!("seuil conseillé  : {suggested:.4}");

    if suggested > default * 1.5 {
        println!(
            "\n→ Ton micro est trop bruyant pour le défaut. Avec {default:.4}, la porte ne\n  \
             trouve jamais de silence : elle n'achève aucun segment, donc rien n'est\n  \
             transcrit. C'est ce que dit « no pause in a whole turn's worth of audio »."
        );
    } else if suggested < default * 0.5 {
        println!("\n→ Micro très propre. Le défaut marche ; plus bas rendrait l'écoute plus vive.");
    } else {
        println!(
            "\n→ Le défaut convient : le bruit de fond n'explique pas une absence de réaction."
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = parse_args()?;

    match args.mode {
        Mode::File(ref path) => {
            let mut asr = load(&args.model)?;
            let samples = wav::read(path)?;
            println!("fichier {}", path.display());
            let (text, ms) = transcribe(&mut asr, samples.clone())?;
            report(wav::seconds(&samples), &text, ms);
        }

        Mode::Noise(seconds) => noise(seconds)?,

        Mode::Bench(seconds) => {
            let mut asr = load(&args.model)?;
            // No microphone, no human: a synthetic buffer measures the compute
            // path. It does not measure the decoder's work — a tone produces
            // almost no tokens — so it reads as a floor, and the encoder is what
            // dominates it.
            let frames = (seconds * wav::SAMPLE_RATE as f32) as usize;
            let tone: Vec<f32> = (0..frames)
                .map(|i| {
                    let t = i as f32 / wav::SAMPLE_RATE as f32;
                    0.05 * (std::f32::consts::TAU * 220.0 * t).sin()
                })
                .collect();

            println!(
                "banc    {seconds:.1} s de signal synthétique, {BENCH_RUNS} passes, profil {}",
                if cfg!(debug_assertions) {
                    "DEBUG"
                } else {
                    "release"
                }
            );
            let mut times = Vec::new();
            for run in 1..=BENCH_RUNS {
                // The text is printed, not discarded: a pure tone must transcribe
                // to nothing. Anything else would mean the feature cache is
                // handing back a previous utterance — a correctness bug, not a
                // timing one.
                let (text, ms) = transcribe(&mut asr, tone.clone())?;
                println!(
                    "  passe {run}  {ms:>5} ms  {:>5.0}× temps réel  {}",
                    seconds * 1000.0 / ms as f32,
                    if text.is_empty() {
                        "— rien reconnu, comme attendu".to_string()
                    } else {
                        format!("ATTENTION, texte sur un signal pur : « {text} »")
                    }
                );
                times.push(ms);
            }
            times.sort_unstable();
            let median = times[times.len() / 2];
            println!(
                "  médiane {median} ms — {:.0}× temps réel",
                seconds * 1000.0 / median as f32
            );
        }

        Mode::Interactive => {
            print!("{}", aria_voice::describe_host());
            println!();
            let mut asr = load(&args.model)?;

            let recorder: Recorder<CollectingSink> =
                Recorder::open_with_failure_handler(RecorderConfig::speech(), |error| {
                    eprintln!("\nle micro a lâché : {error}");
                })
                .map_err(|e| anyhow!("cannot open the microphone: {e}"))?;
            println!("micro   {:?}", recorder.info());
            if let Some(dir) = &args.save {
                println!("garde   {}", dir.display());
            }
            println!();

            let mut index = 1usize;
            loop {
                if prompt("⏎ pour parler, q pour quitter > ")?.eq_ignore_ascii_case("q") {
                    break;
                }

                recorder
                    .start(CollectingSink::new())
                    .map_err(|e| anyhow!("cannot start recording: {}", e.error))?;
                prompt("j'écoute… ⏎ pour arrêter")?;
                let stopped = recorder
                    .stop()
                    .map_err(|e| anyhow!("cannot stop recording: {e}"))?;

                if !stopped.is_complete() {
                    // Said plainly rather than swallowed: dropped frames explain
                    // a truncated transcript, and guessing at that wastes an
                    // afternoon.
                    eprintln!(
                        "  (incomplet : {:?}, {} trames perdues)",
                        stopped.end_reason, stopped.dropped_frames
                    );
                }

                let samples = stopped.sink.into_samples();
                let seconds = wav::seconds(&samples);
                if seconds < 0.2 {
                    println!("  trop court ({seconds:.2} s) — rien à transcrire\n");
                    continue;
                }

                if let Some(dir) = &args.save {
                    let path = dir.join(format!("utterance-{index:02}.wav"));
                    wav::write(&path, &samples)?;
                    println!("  → {}", path.display());
                    index += 1;
                }

                let (text, ms) = transcribe(&mut asr, samples)?;
                report(seconds, &text, ms);
            }

            recorder
                .close()
                .map_err(|e| anyhow!("closing the microphone failed: {e}"))?;
        }
    }

    Ok(())
}
