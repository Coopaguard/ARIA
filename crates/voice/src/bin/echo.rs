//! T2 — echo: measuring what the microphone hears of the speakers.
//!
//! Three numbers decide whether [`aria_voice::echo`] works, and none of them can
//! be guessed, because all three belong to the room, the speakers and the
//! microphone gain rather than to the code:
//!
//!  - the **delay** between what is played and what comes back;
//!  - the **coupling**, in dB: how loud ARIA is in its own microphone;
//!  - the **excess** spread during echo alone, which sets the margin. The margin
//!    has to clear the worst echo-only frame, or ARIA interrupts itself — and
//!    nothing else in the chain would explain why.
//!
//! ```bash
//! cargo run --release -p aria-voice --bin echo                             # devices only
//! cargo run --release -p aria-voice --bin echo -- --find                   # which output is heard
//! cargo run --release -p aria-voice --bin echo -- --bursts --out <n>       # echo alone
//! cargo run --release -p aria-voice --bin echo -- --bursts --out <n> --barge # talk over it
//! cargo run --release -p aria-voice --bin echo -- --play <wav> --transcribe # prove the loop
//! cargo run --release -p aria-voice --bin echo -- --mic <wav> --ref <wav>  # re-read offline
//! ```
//!
//! **`--bursts` is the way in**, and `--play` is for replaying a particular
//! recording. The burst train needs no file, it is reproducible between runs,
//! and nobody wants their assistant replaying a recording of them while it
//! configures itself.
//!
//! # What it measured, the first time it had a room
//!
//! 2026-11-04, laptop loudspeakers into a worn headset microphone, leaning
//! over them on purpose: lock at 22 frames (440 ms), correlation 0.94,
//! coupling **−14.8 dB**, worst echo-only frame 4.9 dB, so a margin of
//! **8 dB** — which is now the shipped `echo_margin_db`. Decision 82.
//!
//! **Pick the output device deliberately.** `--find` exists because the system
//! default is often not the thing making noise in the room: on that machine it
//! was a monitor, and the first two measurements were of a loudspeaker nobody
//! could hear.

use anyhow::{Result, anyhow, bail};
use aria_voice::{echo, wav};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use handy_recorder::{CollectingSink, Recorder, RecorderConfig};
use std::path::PathBuf;

/// Longest delay searched for, in frames — ten seconds.
///
/// Far past any sound card, which was the first guess at one second, and that
/// guess cost two failed measurements: a USB headset can take **seconds** to
/// start its stream after one is opened, so the acoustic delay is not what sets
/// this bound — the difference between two independent start times is.
///
/// At runtime the bound is small again, because the reference is clocked by us
/// rather than by a device waking up. This width is a property of *measuring*,
/// not of the echo.
const MAX_LAG_FRAMES: usize = 500;
/// Recording kept running this long after playback ends, to catch the tail.
const TAIL_MS: u64 = 700;
/// Speech floor, shared with the turn detector so one number means one thing.
const SPEECH_RMS: f32 = 0.01;
/// Below this peak RMS a recording is not quiet, it is **empty**.
///
/// A ratio alone cannot tell those apart, and on 2026-11-04 it did not: the
/// quiet head of a silent capture read as 0, the clamp in `floor.max(1e-6)`
/// turned the division into 0/0, and `--find` announced `← ENTENDUE` at
/// +12.7 dB on a device that was playing nothing anybody could hear. An hour
/// of measurement went down that road.
///
/// 0.002 sits between the two populations measured that day: a real room floor
/// on a headset microphone was 0.00081 RMS, and the quietest frame that
/// actually carried the loudspeakers was 0.13. Anything under this is the
/// measurement failing to happen.
const AUDIBLE_RMS: f32 = 0.002;
/// Safety added to the worst echo-only frame to get the recommended margin.
const SAFETY_DB: f32 = 3.0;
const LEVELS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

struct Args {
    play: Option<PathBuf>,
    mic: Option<PathBuf>,
    reference: Option<PathBuf>,
    save: Option<PathBuf>,
    model: Option<PathBuf>,
    barge: bool,
    transcribe: bool,
    find: bool,
    output: Option<usize>,
    /// Measure with a burst train instead of a recording.
    bursts: Option<usize>,
    /// Playback amplitude. The coupling is a ratio, so this does not change it
    /// — it lifts the echo clear of the microphone noise so the ratio can be
    /// seen at all.
    level: f32,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        play: None,
        mic: None,
        reference: None,
        save: None,
        model: None,
        barge: false,
        transcribe: false,
        find: false,
        output: None,
        bursts: None,
        level: 0.3,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let value = |i: &mut usize| -> Result<PathBuf> {
            *i += 1;
            Ok(PathBuf::from(
                argv.get(*i)
                    .ok_or_else(|| anyhow!("{} wants a path", argv[*i - 1]))?,
            ))
        };
        match argv[i].as_str() {
            "--play" => args.play = Some(value(&mut i)?),
            "--mic" => args.mic = Some(value(&mut i)?),
            "--ref" => args.reference = Some(value(&mut i)?),
            "--save" => args.save = Some(value(&mut i)?),
            "--model" => args.model = Some(value(&mut i)?),
            "--barge" => args.barge = true,
            "--find" => args.find = true,
            "--level" => {
                i += 1;
                args.level = argv
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow!("--level wants an amplitude between 0 and 1"))?;
            }
            "--bursts" => {
                let n = argv.get(i + 1).and_then(|v| v.parse::<usize>().ok());
                if n.is_some() {
                    i += 1;
                }
                args.bursts = Some(n.unwrap_or(10));
            }
            "--out" => {
                i += 1;
                args.output = Some(
                    argv.get(i)
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| anyhow!("--out wants the position of an output device"))?,
                );
            }
            "--transcribe" => args.transcribe = true,
            other => bail!("unknown argument: {other}"),
        }
        i += 1;
    }
    Ok(args)
}

/// Feeds the 16 kHz mono source to the output device, resampling to its rate.
///
/// Linear interpolation, which is below what a TTS deserves and plenty for a
/// measurement: it adds high-frequency hash, and the gate reads energy.
fn output_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    source: Vec<f32>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let step = wav::SAMPLE_RATE as f32 / config.sample_rate as f32;
    let mut position = 0f32;
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    let index = position as usize;
                    let value = match (source.get(index), source.get(index + 1)) {
                        (Some(&a), Some(&b)) => {
                            let t = position - index as f32;
                            a + (b - a) * t
                        }
                        (Some(&a), None) => a,
                        _ => 0.0,
                    };
                    position += step;
                    for sample in frame.iter_mut() {
                        *sample = T::from_sample(value);
                    }
                }
            },
            |error| eprintln!("le flux de sortie a lâché : {error}"),
            None,
        )
        .map_err(|e| anyhow!("cannot open the output stream: {e}"))?;
    Ok(stream)
}

/// Plays `source` through the speakers while recording the microphone.
fn play_and_record(
    source: Vec<f32>,
    announce: Option<&str>,
    output: Option<usize>,
) -> Result<Vec<f32>> {
    let host = cpal::default_host();
    let device = match output {
        // Picking by position in the list, because that is how the listing
        // prints them and a WASAPI identifier is not something anyone types.
        Some(n) => host
            .output_devices()
            .map_err(|e| anyhow!("cannot list output devices: {e}"))?
            .nth(n)
            .ok_or_else(|| anyhow!("no output device at position {n}"))?,
        None => host
            .default_output_device()
            .ok_or_else(|| anyhow!("no output device: nothing to play through"))?,
    };
    let supported = device
        .default_output_config()
        .map_err(|e| anyhow!("cannot read the output config: {e}"))?;
    println!(
        "sortie    {} — {} Hz, {} canaux, {:?}",
        device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "?".into()),
        supported.sample_rate(),
        supported.channels(),
        supported.sample_format()
    );

    let recorder: Recorder<CollectingSink> = Recorder::open(RecorderConfig::speech())
        .map_err(|e| anyhow!("cannot open the microphone: {e}"))?;
    println!("micro     {:?}", recorder.info());

    // The microphone starts first, on purpose: the reference must begin inside
    // the recording, never before it. A negative lag has no representation here
    // and would silently lock onto the wrong peak.
    recorder
        .start(CollectingSink::new())
        .map_err(|e| anyhow!("cannot start recording: {}", e.error))?;
    std::thread::sleep(std::time::Duration::from_millis(200));

    let seconds = wav::seconds(&source);
    if let Some(text) = announce {
        println!("\n{text}\n");
    }
    println!("lecture   {seconds:.1} s…");

    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => output_stream::<f32>(&device, config, source),
        SampleFormat::I16 => output_stream::<i16>(&device, config, source),
        SampleFormat::I32 => output_stream::<i32>(&device, config, source),
        SampleFormat::U16 => output_stream::<u16>(&device, config, source),
        SampleFormat::F64 => output_stream::<f64>(&device, config, source),
        other => bail!("unsupported output format {other} — add it to the match above"),
    }?;
    stream
        .play()
        .map_err(|e| anyhow!("cannot start playback: {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(
        (seconds * 1000.0) as u64 + TAIL_MS,
    ));
    drop(stream);

    let stopped = recorder
        .stop()
        .map_err(|e| anyhow!("cannot stop recording: {e}"))?;
    if !stopped.is_complete() {
        eprintln!(
            "  (incomplet : {:?}, {} trames perdues)",
            stopped.end_reason, stopped.dropped_frames
        );
    }
    let samples = stopped.sink.into_samples();
    recorder
        .close()
        .map_err(|e| anyhow!("closing the microphone failed: {e}"))?;
    Ok(samples)
}

/// The signal the echo path is measured with: bursts, not speech.
///
/// Speech was the obvious choice and it was wrong. Measured on 2026-10-26: the
/// played sentence came back so attenuated that its envelope sat under the
/// handling noise of the microphone, and the two halves of the recording locked
/// 8 s apart — the correlation was fitting noise.
///
/// A burst train fixes both ends of that. It carries **far more energy per
/// second** than speech, so it survives a bad path; and its envelope is square
/// and unmistakable, so a lock is either right or obviously absent. The
/// frequency moves from burst to burst, across the band a voice occupies, so
/// that one room resonance cannot pass for a coupling.
///
/// It is also what the setup wizard should play. Nobody wants their assistant
/// replaying a recording of them while it configures itself.
///
/// # The carrier is broadband noise, and it used to be a frequency sweep
///
/// Each burst used to sit at its own frequency, climbing 180 Hz to 3 kHz
/// across the train, so that *one room resonance could not pass for a
/// coupling*. The intent was right and the instrument was wrong, measured on
/// 2026-11-04 on laptop loudspeakers:
///
/// | Salves | Fréquence | Couplage |
/// |---|---|---|
/// | 1–2 | 180–250 Hz | −36 à −40 dB |
/// | 3–5 | 350–700 Hz | −21 à −18 dB |
/// | 6–10 | 1–3 kHz | −3 à −8 dB |
///
/// A small speaker cannot radiate 180 Hz and is at its best near 3 kHz, so its
/// response climbs 33 dB across the band the sweep walks through. **A coupling
/// that depends on frequency then presents itself as a coupling that varies in
/// time**, which is the one thing [`echo::Gate`] has no way to model: it
/// learned −37 dB from the quiet low bursts and was then surprised by the loud
/// high ones, scoring them as somebody talking. Correlation 0.51, and a
/// recommended margin of **36 dB** — a number that would leave ARIA deaf to
/// every interruption.
///
/// Band-limited noise fixes it by exciting the **whole band at every instant**.
/// No single resonance can dominate, which was the original worry, and the
/// microphone-to-reference ratio keeps one meaning from the first burst to the
/// last. The same room, same speakers, same envelope: correlation **0.98**,
/// and a margin of 5 dB.
fn burst_train(bursts: usize) -> Vec<f32> {
    // **The envelope must not repeat.** The first version used one duration and
    // one gap for every burst, which gives a periodic envelope — and a periodic
    // envelope has a correlation peak at every period, so the lock cannot
    // choose between them. Measured: the same recording locked at 7 frames once
    // and 113 the next time, both with a plausible-looking correlation.
    //
    // These durations are a fixed, uneven pattern. Nothing about the particular
    // numbers matters except that no gap equals its neighbour and no run of
    // them repeats, so the envelope has one signature and only one.
    const ON_MS: [usize; 8] = [120, 300, 180, 90, 260, 140, 340, 200];
    const OFF_MS: [usize; 8] = [210, 90, 380, 160, 110, 300, 130, 240];
    let rate = wav::SAMPLE_RATE as f32;

    // One continuous carrier cut into bursts, rather than one per burst: the
    // filters need a few milliseconds to settle, and restarting them at every
    // burst would put that transient exactly where the envelope rises.
    let lengths: Vec<usize> = (0..bursts)
        .map(|b| ON_MS[b % ON_MS.len()] * wav::SAMPLE_RATE as usize / 1000)
        .collect();
    let carrier = voice_band_noise(lengths.iter().sum());

    let mut out = Vec::new();
    let mut read = 0usize;
    for (b, &on) in lengths.iter().enumerate() {
        let on_ms = ON_MS[b % ON_MS.len()];
        for i in 0..on {
            let t = i as f32 / rate;
            // Raised-cosine edges: a square edge clicks, and a click is the one
            // transient every gate in this crate is built to throw away.
            let fade = (t / 0.02).min(1.0).min((on_ms as f32 / 1000.0 - t) / 0.02);
            out.push(0.3 * fade.clamp(0.0, 1.0) * carrier[read]);
            read += 1;
        }
        out.extend(std::iter::repeat_n(
            0.0,
            OFF_MS[b % OFF_MS.len()] * wav::SAMPLE_RATE as usize / 1000,
        ));
    }
    out
}

/// Which end of a second-order section is being shaped.
enum Slope {
    Low,
    High,
}

/// Deterministic noise, flat across the band a voice occupies.
///
/// **Deterministic on purpose.** A measurement whose stimulus changes between
/// two runs cannot be compared with itself, and the first question asked of
/// any surprising result is whether the run before it was the same run.
fn voice_band_noise(frames: usize) -> Vec<f32> {
    // xorshift64*, eight lines and no dependency. Nothing here needs the
    // statistical quality of a real generator: what the measurement wants is a
    // signal with no periodic structure, and that is the easy half.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut white: Vec<f32> = Vec::with_capacity(frames);
    for _ in 0..frames {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let x = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        white.push(((x >> 40) as f32 / 8_388_608.0) - 1.0);
    }

    // 250 Hz to 3.8 kHz, two sections at each end for 24 dB per octave. Below
    // 250 Hz a laptop speaker radiates nothing, so energy spent there is
    // energy that never reaches the microphone and only drags the average
    // coupling down; above 3.8 kHz there is nothing left of a voice, and the
    // chain is sampled at 16 kHz anyway.
    let mut shaped = white;
    for _ in 0..2 {
        shaped = biquad(&shaped, 250.0, Slope::High);
        shaped = biquad(&shaped, 3_800.0, Slope::Low);
    }

    let peak = shaped.iter().copied().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak > 0.0 {
        for v in &mut shaped {
            *v /= peak;
        }
    }
    shaped
}

/// One second-order section, Robert Bristow-Johnson's cookbook, Q = 1/√2.
///
/// Written out rather than pulled in: it is twelve lines, and a filter crate
/// would be the only dependency this binary has that the product does not.
fn biquad(xs: &[f32], hz: f32, slope: Slope) -> Vec<f32> {
    let w0 = std::f32::consts::TAU * hz / wav::SAMPLE_RATE as f32;
    let (cos, sin) = (w0.cos(), w0.sin());
    let alpha = sin / std::f32::consts::SQRT_2;
    let (b0, b1, b2) = match slope {
        Slope::Low => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0),
        Slope::High => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0),
    };
    let (a0, a1, a2) = (1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    let (b0, b1, b2, a1, a2) = (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    xs.iter()
        .map(|&x| {
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
            y
        })
        .collect()
}

/// Which output device the microphone can actually hear.
///
/// The question the first two runs could not answer: a silent recording means
/// either "no acoustic path" or "nothing was ever played", and those call for
/// opposite fixes. A short tone on each device in turn separates them in a few
/// seconds — and it has to be a *tone*, not speech, because the point here is
/// energy, not the envelope correlation a lock needs.
fn find_audible_output() -> Result<()> {
    let host = cpal::default_host();
    let devices: Vec<_> = host
        .output_devices()
        .map_err(|e| anyhow!("cannot list output devices: {e}"))?
        .collect();
    println!("{} sorties à essayer, 1,5 s chacune\n", devices.len());

    let seconds = 1.5f32;
    let frames = (seconds * wav::SAMPLE_RATE as f32) as usize;
    let tone: Vec<f32> = (0..frames)
        .map(|i| {
            let t = i as f32 / wav::SAMPLE_RATE as f32;
            // Fades in and out: a square-edged tone clicks, and a click is
            // exactly the transient the gate is built to ignore.
            let fade = (t / 0.1).min(1.0).min((seconds - t) / 0.1);
            0.25 * fade * (std::f32::consts::TAU * 440.0 * t).sin()
        })
        .collect();

    for (n, _) in devices.iter().enumerate() {
        let captured = match play_and_record(tone.clone(), None, Some(n)) {
            Ok(samples) => samples,
            Err(e) => {
                println!("  {n}. injouable — {e}");
                continue;
            }
        };
        // The quiet head of the recording is the room's own floor; what the
        // tone adds on top of it is the answer.
        let envelope = echo::envelope(&captured);
        let split = envelope.len().min(10);
        let floor = envelope[..split].iter().copied().fold(0.0f32, f32::max);
        let loudest = envelope.iter().copied().fold(0.0f32, f32::max);
        let gain_db = 20.0 * (loudest / floor.max(1e-6)).log10();
        // **Two conditions, and the absolute one is not redundant.** The ratio
        // says the tone rose above the floor; it cannot say the floor was a
        // floor. On a capture that is digitally silent both terms are zero,
        // the clamp invents a denominator, and the ratio comes back large and
        // meaningless — which is exactly how this printed `← ENTENDUE` on
        // silence. Five decimals rather than four for the same reason: at four,
        // `0.0000 → 0.0000` was shown next to `+12.7 dB` and nobody blinked.
        let heard = loudest >= AUDIBLE_RMS && gain_db > 6.0;
        println!(
            "  {n}. fond {floor:.5} → crête {loudest:.5} RMS, soit {gain_db:+.1} dB  {}",
            if heard {
                "← ENTENDUE"
            } else if loudest < AUDIBLE_RMS {
                "rien — l'enregistrement est vide, pas seulement calme"
            } else {
                "rien"
            }
        );
    }
    println!(
        "\n  Une sortie « ENTENDUE » est celle sur laquelle mesurer l'écho : relancer avec\n\
         `--bursts --out <n>`. Aucune ne l'est ? Alors rien de ce qui est joué n'atteint\n\
         le micro, et `Gate::without_echo` est le bon comportement, pas un défaut. Trois\n\
         causes le produisent, pas deux :\n\
         \n\
         \x20 - un casque, le cas le plus fréquent pour un assistant vocal ;\n\
         \x20 - des enceintes muettes, ou une sortie par défaut qui n'est pas celle qu'on\n\
         \x20   croit — vérifier la sortie système avant d'accuser la pièce ;\n\
         \x20 - une ANNULATION D'ÉCHO EN AMONT, sur le chemin de capture, hors de notre\n\
         \x20   processus : « améliorations audio » de Windows, DSP de portable, pilote de\n\
         \x20   conférence. Elle se reconnaît au silence NUMÉRIQUE EXACT entre les sons —\n\
         \x20   aucun micro réel n'en produit — et c'est la plus fréquente sur un portable."
    );
    Ok(())
}

fn percentile(sorted: &[f32], fraction: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f32 * fraction).round() as usize]
}

fn sparkline(values: &[f32], low: f32, high: f32) -> String {
    values
        .iter()
        .map(|&v| {
            let t = ((v - low) / (high - low)).clamp(0.0, 1.0);
            LEVELS[(t * 8.0).round() as usize]
        })
        .collect()
}

/// Everything the two recordings have to say about the echo path.
fn analyse(mic: &[f32], reference: &[f32], barge: bool) -> Result<()> {
    let mic_env = echo::envelope(mic);
    let ref_env = echo::envelope(reference);
    println!(
        "\nmicro     {:.1} s capturées, référence {:.1} s",
        wav::seconds(mic),
        wav::seconds(reference)
    );

    let (lag, rho) = echo::lock_delay(&mic_env, &ref_env, MAX_LAG_FRAMES);
    println!(
        "verrou    {lag} trames = {} ms, corrélation {rho:.2}",
        lag * echo::FRAME * 1000 / wav::SAMPLE_RATE as usize
    );
    if rho < 0.5 {
        // Before blaming the room: does the delay hold across the recording?
        // Two devices clocked independently drift, and a drift of a few frames
        // over twenty seconds smears an envelope whose features last 100 ms —
        // which looks exactly like "no relationship" while being the opposite.
        let half = ref_env.len() / 2;
        if half > 20 && mic_env.len() > half {
            let (early, _) = echo::lock_delay(&mic_env, &ref_env[..half], MAX_LAG_FRAMES);
            let (late, _) = echo::lock_delay(&mic_env[half..], &ref_env[half..], MAX_LAG_FRAMES);
            let drift = late as isize - early as isize;
            println!(
                "dérive    {early} trames au début, {late} à la fin — {drift:+} trames \
                 ({:+.0} ppm)",
                drift as f32 * echo::FRAME as f32 / (half as f32 * echo::FRAME as f32) * 1e6
            );
        }

        let loudest = mic_env.iter().copied().fold(0.0f32, f32::max);
        // "Below the floor" is not "nothing": measured on 2026-10-26, a capture
        // peaking at 0.003 RMS — a quarter of the floor — transcribed as a full
        // conversation. The floor is a listening radius, not a silence test.
        let heard = if loudest < SPEECH_RMS {
            "et le micro est sous le plancher de parole : silence, ou des sons trop\n\
             lointains pour être adressés à ARIA — ce que le plancher est là pour écarter"
        } else {
            "et le micro a capté autre chose : la pièce, pas les enceintes"
        };
        // **The signature of a canceller upstream of us**, and it is worth
        // computing because it names the cause instead of leaving three open.
        // A microphone in a room always returns *something*: thermal noise in
        // the preamplifier if nothing else. Exact binary zero is a decision
        // somebody else's code made — a noise gate, which rides along with
        // every echo canceller worth the name. Measured on 2026-11-04 on an
        // Intel Smart Sound array: more than half the frames at exact zero,
        // while the same room through a headset microphone sat at 0.0008.
        let muted = mic_env.iter().filter(|&&v| v == 0.0).count();
        let gated = muted * 5 > mic_env.len();
        bail!(
            "corrélation {rho:.2}, crête micro {loudest:.3} RMS — {heard}.\n\
             \n\
             Rien de ce qui est joué ne revient. `Gate::without_echo` est le bon\n\
             comportement dans ce cas, pas un défaut : toute parole y vaut interruption.\n\
             Ce qui ne peut pas se faire ici, c'est la MESURE.\n\
             \n\
             {}",
            if gated {
                format!(
                    "ET LE MICRO EST FILTRÉ EN AMONT : {muted} trames sur {} sont à zéro\n\
                     NUMÉRIQUE EXACT, ce qu'aucun micro réel ne produit. Il y a une porte de\n\
                     bruit, donc presque sûrement une annulation d'écho, sur le chemin de\n\
                     capture et hors de notre processus — « améliorations audio » de Windows,\n\
                     DSP de portable, pilote de conférence. Les désactiver pour ce\n\
                     périphérique, ou mesurer par un autre micro : c'est le seul moyen de\n\
                     voir ce que la pièce fait réellement.",
                    mic_env.len()
                )
            } else {
                "Trois causes, et le plancher du micro n'évoque pas la troisième ici :\n\
                 un casque, le cas le plus fréquent pour un assistant vocal ; des enceintes\n\
                 muettes ou une sortie par défaut qui n'est pas celle qu'on croit — la\n\
                 vérifier avant d'accuser la pièce ; ou une annulation d'écho en amont.\n\
                 La mesure demande des enceintes qui jouent dans la même pièce que le micro,\n\
                 sur la machine cible."
                    .to_string()
            }
        );
    }

    let Some(coupling) = echo::coupling_db(&mic_env, &ref_env, lag) else {
        bail!("not enough loud reference frames to measure the coupling");
    };
    println!("couplage  {coupling:.1} dB — ce que les enceintes renvoient dans le micro");

    // Classified with no margin at all, so every frame reports its excess and
    // nothing is pre-judged. The margin is the output of this, not an input.
    let gate = echo::Gate {
        lag,
        coupling_db: coupling,
        margin_db: f32::INFINITY,
        speech_rms: SPEECH_RMS,
    };
    let verdicts = gate.classify(&mic_env, &ref_env);
    let mut excess: Vec<f32> = verdicts
        .iter()
        .filter(|(v, _)| *v != echo::Verdict::Quiet)
        .map(|&(_, e)| e)
        .collect();
    if excess.is_empty() {
        bail!("every frame is below {SPEECH_RMS} RMS — nothing was recorded");
    }
    let loud = excess.len();
    excess.sort_by(f32::total_cmp);

    println!(
        "\nexcédent  au-dessus de l'écho prédit, sur {loud} trames sonores \
         ({:.0} % de l'enregistrement)",
        loud as f32 * 100.0 / verdicts.len() as f32
    );
    println!(
        "          médiane {:>5.1}   p90 {:>5.1}   p99 {:>5.1}   max {:>5.1}  dB",
        percentile(&excess, 0.5),
        percentile(&excess, 0.9),
        percentile(&excess, 0.99),
        percentile(&excess, 1.0)
    );

    let profile: Vec<f32> = verdicts.iter().map(|&(_, e)| e).collect();
    println!("\n          profil, un caractère par 20 ms, de 0 à 24 dB d'excédent");
    for row in profile.chunks(100) {
        println!("          │{}│", sparkline(row, 0.0, 24.0));
    }

    if barge {
        // With somebody talking over it, the excess splits in two: the echo-only
        // frames stay near zero and the double-talk frames climb. The gap
        // between them *is* the margin, and it is why this mode exists.
        let margin = percentile(&excess, 0.75) + SAFETY_DB;
        let armed = echo::Gate {
            margin_db: margin,
            ..gate
        };
        let verdicts = armed.classify(&mic_env, &ref_env);
        let onset = echo::Gate::barge_onset(&verdicts);
        let counted = |wanted: echo::Verdict| verdicts.iter().filter(|(v, _)| *v == wanted).count();
        println!(
            "\nseuil     {margin:.1} dB (3e quartile + {SAFETY_DB:.0} dB)\n\
             verdicts  {} silence, {} écho, {} interruption",
            counted(echo::Verdict::Quiet),
            counted(echo::Verdict::Echo),
            counted(echo::Verdict::Barge)
        );
        match onset {
            Some(frame) => println!(
                "détection la parole par-dessus est reconnue à {:.2} s, après \
                 {} trames consécutives = {} ms de preuve",
                (frame + echo::BARGE_FRAMES) as f32 * echo::FRAME as f32 / wav::SAMPLE_RATE as f32,
                echo::BARGE_FRAMES,
                echo::BARGE_FRAMES * echo::FRAME * 1000 / wav::SAMPLE_RATE as usize
            ),
            None => println!(
                "détection aucune interruption détectée — soit personne n'a parlé, soit la\n\
                 voix était plus basse que l'écho, ce qui est le cas difficile."
            ),
        }
    } else {
        let worst = percentile(&excess, 1.0);
        println!(
            "\n  Écho seul : tout excédent mesuré ici est du bruit de mesure, puisque rien\n\
             d'autre ne parlait. La marge doit donc passer au-dessus du pire — {:.0} dB,\n\
             sécurité comprise. En dessous, ARIA se coupe la parole à elle-même.",
            worst + SAFETY_DB
        );
    }
    Ok(())
}

fn transcribe(model: Option<PathBuf>, mic: &[f32], reference: &[f32]) -> Result<()> {
    use parakeet_rs::Transcriber;

    let dir = model.unwrap_or_else(aria_voice::asr::default_model_dir);
    let mut asr = aria_voice::asr::load(&dir)?;

    let spoken = asr
        .transcribe_samples(reference.to_vec(), wav::SAMPLE_RATE, 1, None)
        .map_err(|e| anyhow!("transcription failed: {e}"))?
        .text;
    let heard = asr
        .transcribe_samples(mic.to_vec(), wav::SAMPLE_RATE, 1, None)
        .map_err(|e| anyhow!("transcription failed: {e}"))?
        .text;

    // The whole reason the gate exists, in two lines: without it, the second
    // transcript is what the agent would have received as a user request.
    println!("\njoué      « {} »", spoken.trim());
    println!("entendu   « {} »", heard.trim());
    println!(
        "veto      {}",
        if echo::is_self_echo(&heard, &spoken) {
            "oui — le transcript est reconnu comme sa propre voix et serait jeté"
        } else {
            "NON — le dernier filet ne retient pas ce transcript ; à regarder de près"
        }
    );
    Ok(())
}

fn main() -> Result<()> {
    let args = parse_args()?;

    if args.find {
        return find_audible_output();
    }

    if let (Some(mic), Some(reference)) = (&args.mic, &args.reference) {
        return analyse(&wav::read(mic)?, &wav::read(reference)?, args.barge);
    }

    if let Some(n) = args.bursts {
        let reference: Vec<f32> = burst_train(n)
            .iter()
            .map(|s| s * args.level / 0.3)
            .collect();
        println!("référence {n} salves — {:.1} s", wav::seconds(&reference));
        let mic = play_and_record(
            reference.clone(),
            Some("NE DIS RIEN et ne bouge pas : cette passe mesure l'écho seul."),
            args.output,
        )?;
        if let Some(dir) = &args.save {
            std::fs::create_dir_all(dir)?;
            wav::write(&dir.join("echo-bursts-mic.wav"), &mic)?;
            wav::write(&dir.join("echo-bursts-ref.wav"), &reference)?;
            println!("gardé     {}", dir.display());
        }
        return analyse(&mic, &reference, args.barge);
    }

    let Some(path) = args.play.clone() else {
        // Nothing asked for: say what is available and stop. An absent output
        // device is the one failure no amount of code works around.
        print!("{}", aria_voice::describe_host());
        println!("output devices");
        let host = cpal::default_host();
        match host.output_devices() {
            Ok(devices) => {
                for device in devices {
                    let id = device
                        .id()
                        .map(|id| id.to_string())
                        .unwrap_or_else(|_| "?".into());
                    let config = device
                        .default_output_config()
                        .map(|c| {
                            format!(
                                "{} Hz, {} ch, {:?}",
                                c.sample_rate(),
                                c.channels(),
                                c.sample_format()
                            )
                        })
                        .unwrap_or_else(|e| format!("unreadable: {e}"));
                    println!("  {id}  {config}");
                }
            }
            Err(e) => println!("  cannot be listed: {e}"),
        }
        return Ok(());
    };

    let reference = wav::read(&path)?;
    println!(
        "référence {} — {:.1} s",
        path.display(),
        wav::seconds(&reference)
    );
    let announce = if args.barge {
        Some(
            "PARLE PAR-DESSUS, vers le milieu — n'importe quoi, assez fort pour être\n\
             entendu. C'est la seule façon de mesurer l'écart entre un écho et une voix.",
        )
    } else {
        Some("NE DIS RIEN et ne bouge pas : cette passe mesure l'écho seul.")
    };
    let mic = play_and_record(reference.clone(), announce, args.output)?;

    if let Some(dir) = &args.save {
        std::fs::create_dir_all(dir)?;
        let out = dir.join(if args.barge {
            "echo-barge.wav"
        } else {
            "echo-only.wav"
        });
        wav::write(&out, &mic)?;
        println!("gardé     {}", out.display());
    }

    analyse(&mic, &reference, args.barge)?;
    if args.transcribe {
        transcribe(args.model, &mic, &reference)?;
    }
    Ok(())
}
