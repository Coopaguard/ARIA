//! Make a Phonon voice from someone's recordings, then check it.
//!
//! No published Phonon checkpoint carries its speaker encoder (decision 133).
//! Its config, though, describes one exactly: `speaker_mimi` is Kyutai's Mimi
//! codec — ratios 8·6·5·4, eight transformer layers, 512 dimensions at 12.5 Hz.
//! Mimi itself is published, CC-BY 4.0, in `kyutai/moshiko-pytorch-bf16`. This
//! tries it, and does not take the match for granted: `--probe` makes catalogue
//! voices speak, encodes what came out, and asks whether the encoding lands next
//! to the voice that produced it. If it does, the latent spaces agree.
//!
//! ```text
//! cargo run -p aria-voice --example clone_voice -- --probe
//! cargo run -p aria-voice --example clone_voice -- \
//!     --voice louis=D:/rec/L3-louis/p1-30cm,D:/rec/L1-louis/p2-1m \
//!     --voice alex=D:/rec/L4-alex/p1-30cm --out D:/voices
//! ```

use anyhow::{Context, Result, anyhow, bail};
use ptts::mimi::{MimiConfig, MimiEncoder};
use ptts::synth::Synth;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use xn::{CpuDevice, Tensor};

type Q = xn::Unquantized<f32, CpuDevice>;

const RATE: usize = 24_000;
/// What every catalogue voice holds: ten seconds at 12.5 Hz.
const FRAMES: usize = 125;
const CHANNELS: usize = 512;
const FR_REPO: &str = "models--gradium--phonon-40m-fr";
const MIMI_FILE: &str = "tokenizer-e351c8d8-checkpoint125.safetensors";

const SENTENCES: [&str; 3] = [
    "Bonjour, je suis ARIA. Que puis-je faire pour toi aujourd'hui ?",
    "La réunion de demain est déplacée à quinze heures trente, dans la petite salle.",
    "Il fait douze degrés dehors, avec un peu de vent. Pense à prendre une veste.",
];

fn main() -> Result<()> {
    let mut probe = false;
    let mut voices: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut out = std::env::temp_dir().join("aria-voices");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--probe" => probe = true,
            "--out" => out = args.next().context("--out wants a folder")?.into(),
            "--voice" => {
                let spec = args.next().context("--voice wants name=dir[,dir]")?;
                let (name, dirs) = spec.split_once('=').context("--voice wants name=dir[,dir]")?;
                voices.push((name.to_string(), dirs.split(',').map(PathBuf::from).collect()));
            }
            other => bail!("unknown argument: {other}"),
        }
    }
    std::fs::create_dir_all(&out)?;

    let snapshot = fr_snapshot()?;
    let cfg = ptts::checkpoint::read_config(&snapshot.join("config.json"))
        .map_err(|e| anyhow!("config: {e}"))?;
    let encoder = load_mimi(cfg.speaker_mimi_cfg())?;
    let mut synth = Synth::builder(cfg, snapshot.join("model.safetensors"), ptts::preprocess::Lang::Fr)
        .tokenizer_file(snapshot.join("tokenizer.json"))
        .build()
        .map_err(|e| anyhow!("phonon: {e}"))?;
    let catalogue = catalogue()?;
    println!("catalogue: {} voices, encoder loaded", catalogue.len());

    // Always: the voices need the statistics it measures. `--probe` alone
    // stops there.
    let rvq = Rvq::load()?;
    let adapt = probe_encoder(&encoder, &rvq, &mut synth, &catalogue, &out)?;
    if probe && voices.is_empty() {
        return Ok(());
    }
    aria_voice::asr::set_gpu(false);
    let asr = aria_voice::asr::load(&aria_voice::asr::default_model_dir())
        .inspect_err(|e| println!("ASR indisponible : {e}"))
        .ok();
    let mut tools = Tools { encoder: &encoder, rvq: &rvq, adapt: &adapt, asr };
    for (name, dirs) in &voices {
        make_voice(name, dirs, &mut tools, &mut synth, &catalogue, &out)?;
    }
    Ok(())
}

fn fr_snapshot() -> Result<PathBuf> {
    let snapshots = aria_voice::models_root().join("phonon").join(FR_REPO).join("snapshots");
    std::fs::read_dir(&snapshots)
        .with_context(|| format!("{}", snapshots.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.join("model.safetensors").is_file())
        .context("phonon-40m-fr is not downloaded")
}

/// Mimi under the names `ptts` expects: moshi wraps every conv once more, and
/// keeps the attention's input projection as a bare tensor.
fn load_mimi(cfg: &MimiConfig) -> Result<MimiEncoder<Q>> {
    let path = aria_voice::models_root().join("mimi").join(MIMI_FILE);
    let vb = xn::nn::VB::load_with_key_map(&[&path], CpuDevice, |k| {
        (k.starts_with("encoder") || k.starts_with("downsample"))
            .then(|| k.replacen("conv.conv.", "conv.", 1).replace("in_proj_weight", "in_proj.weight"))
    })
    .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    MimiEncoder::load(&vb.root(), cfg).map_err(|e| anyhow!("mimi encoder: {e}"))
}

/// `[channels][frames]`, row-major, as the catalogue stores it.
struct Latent(Vec<f32>, usize);

impl Latent {
    fn frames(&self) -> usize {
        self.1
    }
    /// The time-average, which is what a voice is once its words are gone.
    fn mean(&self) -> Vec<f32> {
        (0..CHANNELS)
            .map(|c| self.0[c * self.1..(c + 1) * self.1].iter().sum::<f32>() / self.1 as f32)
            .collect()
    }
    fn first(&self, frames: usize) -> Latent {
        let mut v = Vec::with_capacity(CHANNELS * frames);
        for c in 0..CHANNELS {
            v.extend_from_slice(&self.0[c * self.1..c * self.1 + frames]);
        }
        Latent(v, frames)
    }
    fn stats(&self) -> (f32, f32) {
        let n = self.0.len() as f32;
        let mean = self.0.iter().sum::<f32>() / n;
        let var = self.0.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
        (mean, var.sqrt())
    }
}

/// Mimi's split quantizer, read from the same file: one semantic codebook, then
/// the acoustic ones. Phonon may have been trained on what comes *out* of it,
/// not on the raw encoder — the probe tries each.
struct Rvq {
    first: Branch,
    rest: Branch,
}

struct Branch {
    /// `[256][512]` and `[512][256]`.
    input: Vec<f32>,
    output: Vec<f32>,
    /// One `[2048][256]` table per level.
    codebooks: Vec<Vec<f32>>,
}

const Q_DIM: usize = 256;
const BINS: usize = 2048;

impl Rvq {
    fn load() -> Result<Self> {
        let path = aria_voice::models_root().join("mimi").join(MIMI_FILE);
        let t = xn::safetensors::load_from_file(&path, &CpuDevice)?;
        let get = |k: &str| -> Result<Vec<f32>> {
            match t.get(k) {
                Some(xn::TypedTensor::F32(t)) => Ok(t.to_vec()?),
                _ => bail!("{k} missing"),
            }
        };
        let branch = |name: &str| -> Result<Branch> {
            let mut codebooks = Vec::new();
            for level in 0.. {
                let base = format!("quantizer.{name}.vq.layers.{level}._codebook");
                let Ok(sum) = get(&format!("{base}.embedding_sum")) else { break };
                let usage = get(&format!("{base}.cluster_usage"))?;
                let mut cb = sum;
                for (row, u) in cb.chunks_mut(Q_DIM).zip(usage) {
                    row.iter_mut().for_each(|x| *x /= u.max(1e-5));
                }
                codebooks.push(cb);
            }
            Ok(Branch {
                input: get(&format!("quantizer.{name}.input_proj.weight"))?,
                output: get(&format!("quantizer.{name}.output_proj.weight"))?,
                codebooks,
            })
        };
        Ok(Self { first: branch("rvq_first")?, rest: branch("rvq_rest")? })
    }

    /// `levels`: `None` is the continuous path — both projections, no codebook.
    fn apply(&self, x: &Latent, levels: Option<usize>) -> Latent {
        let t = x.frames();
        let mut out = vec![0f32; CHANNELS * t];
        for (branch, n) in [(&self.first, levels.map(|_| 1)), (&self.rest, levels.map(|l| l.saturating_sub(1)))] {
            for f in 0..t {
                let frame: Vec<f32> = (0..CHANNELS).map(|c| x.0[c * t + f]).collect();
                let mut r: Vec<f32> = (0..Q_DIM)
                    .map(|o| (0..CHANNELS).map(|c| branch.input[o * CHANNELS + c] * frame[c]).sum())
                    .collect();
                let q = match n {
                    None => r,
                    Some(n) => {
                        let mut q = vec![0f32; Q_DIM];
                        for cb in branch.codebooks.iter().take(n) {
                            let best = (0..BINS)
                                .map(|i| {
                                    let d: f32 = cb[i * Q_DIM..(i + 1) * Q_DIM]
                                        .iter()
                                        .zip(&r)
                                        .map(|(c, v)| (c - v) * (c - v))
                                        .sum();
                                    (i, d)
                                })
                                .min_by(|a, b| a.1.total_cmp(&b.1))
                                .unwrap()
                                .0;
                            for k in 0..Q_DIM {
                                q[k] += cb[best * Q_DIM + k];
                                r[k] -= cb[best * Q_DIM + k];
                            }
                        }
                        q
                    }
                };
                for c in 0..CHANNELS {
                    out[c * t + f] += (0..Q_DIM).map(|k| branch.output[c * Q_DIM + k] * q[k]).sum::<f32>();
                }
            }
        }
        Latent(out, t)
    }
}

fn encode(encoder: &MimiEncoder<Q>, pcm: &[f32]) -> Result<Latent> {
    let x: Tensor<f32, CpuDevice> = Tensor::from_vec(pcm.to_vec(), (1, 1, ()), &CpuDevice)?;
    let y = encoder.encode_to_latent(&x)?;
    let dims = y.dims().to_vec();
    if dims[1] != CHANNELS {
        bail!("encoder gave {dims:?}");
    }
    Ok(Latent(y.to_vec()?, dims[2]))
}

struct Entry {
    id: String,
    name: String,
    language: String,
    latent: Latent,
    path: PathBuf,
}

fn catalogue() -> Result<Vec<Entry>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/resources/voices");
    let mut entries = Vec::new();
    for line in std::fs::read_to_string(dir.join("voices.jsonl"))?.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        let path = dir.join(v["filename"].as_str().unwrap_or_default());
        let tensors = xn::safetensors::load_from_file(&path, &CpuDevice)?;
        let Some(xn::TypedTensor::F32(t)) = tensors.get("speaker_wavs") else { continue };
        let frames = t.dims()[2];
        entries.push(Entry {
            id: v["voice_id"].as_str().unwrap_or_default().into(),
            name: v["name"].as_str().unwrap_or_default().into(),
            language: v["language"].as_str().unwrap_or_default().into(),
            latent: Latent(t.to_vec()?, frames),
            path,
        });
    }
    Ok(entries)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

/// Where `latent` falls among the catalogue: the rank of `own` (1 is closest),
/// and the three nearest by name. Means are centred on the catalogue's own, or
/// every voice looks like every other.
fn nearest(latent: &Latent, catalogue: &[Entry], own: Option<&str>) -> (Option<usize>, Vec<(String, f32)>) {
    let centre = centroid(catalogue);
    let centred = |m: Vec<f32>| m.iter().zip(&centre).map(|(a, b)| a - b).collect::<Vec<_>>();
    let q = centred(latent.mean());
    let mut scored: Vec<(String, String, f32)> = catalogue
        .iter()
        .map(|e| (e.id.clone(), e.name.clone(), cosine(&q, &centred(e.latent.mean()))))
        .collect();
    scored.sort_by(|a, b| b.2.total_cmp(&a.2));
    let rank = own.and_then(|id| scored.iter().position(|s| s.0 == id).map(|p| p + 1));
    (rank, scored.into_iter().take(3).map(|s| (s.1, s.2)).collect())
}

fn centroid(catalogue: &[Entry]) -> Vec<f32> {
    let mut c = vec![0f32; CHANNELS];
    for e in catalogue {
        for (a, b) in c.iter_mut().zip(e.latent.mean()) {
            *a += b / catalogue.len() as f32;
        }
    }
    c
}

fn say_all(synth: &Synth) -> Result<Vec<f32>> {
    let mut all = Vec::new();
    for s in SENTENCES {
        all.extend(synth.say(s).map_err(|e| anyhow!("say: {e}"))?);
        all.extend(std::iter::repeat_n(0.0, RATE / 4));
    }
    Ok(all)
}

/// Per channel, mean and spread over every frame of `latents`.
fn channel_stats(latents: &[&Latent]) -> Vec<(f32, f32)> {
    (0..CHANNELS)
        .map(|c| {
            let values: Vec<f32> =
                latents.iter().flat_map(|l| l.0[c * l.1..(c + 1) * l.1].iter().copied()).collect();
            let n = values.len() as f32;
            let mean = values.iter().sum::<f32>() / n;
            let var = values.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
            (mean, var.sqrt().max(1e-6))
        })
        .collect()
}

/// Moves Mimi's quantized latents onto the catalogue's per-channel statistics.
/// `from` is measured on the probe voices, not on the one being converted:
/// standardising a voice on its own statistics would erase it.
struct Adapt {
    from: Vec<(f32, f32)>,
    to: Vec<(f32, f32)>,
}

impl Adapt {
    fn apply(&self, x: &Latent) -> Latent {
        let mut v = x.0.clone();
        for c in 0..CHANNELS {
            let ((fm, fs), (tm, ts)) = (self.from[c], self.to[c]);
            for y in &mut v[c * x.1..(c + 1) * x.1] {
                *y = (*y - fm) / fs * ts + tm;
            }
        }
        Latent(v, x.1)
    }

    /// One factor for every channel: the shape kept, only the scale moved.
    fn scale(&self, x: &Latent) -> Latent {
        let f = self.to.iter().map(|s| s.1).sum::<f32>() / self.from.iter().map(|s| s.1).sum::<f32>();
        Latent(x.0.iter().map(|y| y * f).collect(), x.1)
    }
}

/// Make the French catalogue voices speak, encode what they said, and see
/// whether each lands on itself. Chance is 1 in 102.
fn probe_encoder(
    encoder: &MimiEncoder<Q>,
    rvq: &Rvq,
    synth: &mut Synth,
    catalogue: &[Entry],
    out: &Path,
) -> Result<Adapt> {
    let (m, s) = (catalogue.iter().map(|e| e.latent.stats()))
        .fold((0.0, 0.0), |(a, b), (x, y)| (a + x / catalogue.len() as f32, b + y / catalogue.len() as f32));
    println!("catalogue latents: mean {m:.3}, std {s:.3}");
    let variants: [(&str, Option<Option<usize>>); 3] =
        [("raw", None), ("projected", Some(None)), ("rvq-8", Some(Some(8)))];
    let mut ranks: Vec<Vec<usize>> = vec![Vec::new(); variants.len()];
    let mut quantized: Vec<(String, Latent)> = Vec::new();
    for e in catalogue.iter().filter(|e| e.language == "fr").take(6) {
        let wav = out.join(format!("probe-{}.wav", e.name));
        let pcm = if wav.is_file() {
            read_wav(&wav)?
        } else {
            synth.add_voice_file("default", &e.path).map_err(|e| anyhow!("{e}"))?;
            let pcm = say_all(synth)?;
            write_wav(&wav, &pcm)?;
            pcm
        };
        let raw = encode(encoder, &pcm)?;
        for (i, (label, variant)) in variants.iter().enumerate() {
            let latent = match variant {
                None => Latent(raw.0.clone(), raw.1),
                Some(levels) => rvq.apply(&raw, *levels),
            };
            let (mean, std) = latent.stats();
            let (rank, top) = nearest(&latent, catalogue, Some(&e.id));
            println!(
                "probe {:<10} {label:<9} mean {mean:.3} std {std:.3} | rank of itself {:?}/102 | nearest {:?}",
                e.name, rank, top
            );
            ranks[i].extend(rank);
            if *variant == Some(Some(8)) {
                quantized.push((e.id.clone(), latent));
            }
        }
    }
    let cat: Vec<&Latent> = catalogue.iter().map(|e| &e.latent).collect();
    let ours: Vec<&Latent> = quantized.iter().map(|q| &q.1).collect();
    let adapt = Adapt { from: channel_stats(&ours), to: channel_stats(&cat) };
    let adapted: Vec<usize> = quantized
        .iter()
        .filter_map(|(id, l)| nearest(&adapt.apply(l), catalogue, Some(id)).0)
        .collect();
    for ((label, _), r) in variants.iter().zip(&ranks) {
        println!("probe ranks {label:<9} {r:?} (1 = the encoder recognises the voice it heard)");
    }
    println!("probe ranks adapted   {adapted:?}");
    Ok(adapt)
}

struct Tools<'a> {
    encoder: &'a MimiEncoder<Q>,
    rvq: &'a Rvq,
    adapt: &'a Adapt,
    asr: Option<parakeet_rs::ParakeetTDT>,
}

impl Tools<'_> {
    /// Audio to the catalogue's space, the whole way.
    fn to_catalogue(&self, pcm: &[f32]) -> Result<Latent> {
        Ok(self.adapt.apply(&self.rvq.apply(&encode(self.encoder, pcm)?, Some(8))))
    }

    fn hear(&mut self, pcm: &[f32]) -> String {
        use parakeet_rs::Transcriber;
        let Some(asr) = self.asr.as_mut() else { return "(pas d'ASR)".into() };
        let Ok(at16) = ptts::audio::resample(pcm.to_vec(), RATE, 16_000) else { return "(rééchantillonnage)".into() };
        match asr.transcribe_samples(at16, 16_000, 1, None) {
            Ok(r) => r.text.trim().to_string(),
            Err(e) => format!("(échec : {e})"),
        }
    }
}

fn make_voice(name: &str, dirs: &[PathBuf], tools: &mut Tools, synth: &mut Synth, catalogue: &[Entry], out: &Path) -> Result<()> {
    let pcm = gather(dirs)?;
    write_wav(&out.join(format!("{name}-source.wav")), &pcm)?;
    let source_f0 = median_f0(&pcm);
    let raw = encode(tools.encoder, &pcm)?;
    if raw.frames() < FRAMES {
        bail!("{name}: only {} frames of speech, {FRAMES} wanted", raw.frames());
    }
    let quantized = tools.rvq.apply(&raw.first(FRAMES), Some(8));
    let source = tools.adapt.apply(&quantized);
    println!(
        "\n== {name} — source F0 {source_f0:.0} Hz, nearest catalogue voices {:?}",
        nearest(&source, catalogue, None).1
    );

    for (variant, latent) in [("scaled", tools.adapt.scale(&quantized)), ("adapted", tools.adapt.apply(&quantized))] {
        let file = out.join(format!("{name}-{variant}.safetensors"));
        let tensor = Tensor::from_vec(latent.0.clone(), (1, CHANNELS, FRAMES), &CpuDevice)?;
        let tensors = HashMap::from([("speaker_wavs".to_string(), xn::TypedTensor::F32(tensor))]);
        let info = HashMap::from([
            ("encoder".to_string(), format!("kyutai/mimi e351c8d8@125, rvq-8, {variant}")),
            ("source".to_string(), format!("aria clone_voice, {name}")),
        ]);
        xn::safetensors::save_with_data_info(&tensors, Some(info), &file)?;

        synth.add_voice_file("default", &file).map_err(|e| anyhow!("{e}"))?;
        let spoken = say_all(synth)?;
        write_wav(&out.join(format!("{name}-{variant}.wav")), &spoken)?;
        // The probe's question asked of the clone: does what Phonon said in
        // this voice sound, to the encoder, like the person it came from?
        let back = tools.to_catalogue(&spoken)?;
        let centre = centroid(catalogue);
        let c = |m: Vec<f32>| m.iter().zip(&centre).map(|(a, b)| a - b).collect::<Vec<_>>();
        println!(
            "{name}-{variant}: {:.1}s, F0 {:.0} Hz (source {source_f0:.0}), similarity to source {:.3}, nearest {:?}",
            spoken.len() as f32 / RATE as f32,
            median_f0(&spoken),
            cosine(&c(back.mean()), &c(source.mean())),
            nearest(&back, catalogue, None).1
        );
        println!("  entendu : « {} »", tools.hear(&spoken));
    }
    Ok(())
}

/// Median pitch over voiced frames, by autocorrelation at 8 kHz — enough to
/// tell a low voice from a high one, which is the first thing a clone gets wrong.
fn median_f0(pcm: &[f32]) -> f32 {
    let x: Vec<f32> = pcm.chunks(3).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
    let (rate, frame, hop) = (8_000usize, 320usize, 80usize);
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    let mut f0s = Vec::new();
    let mut start = 0;
    while start + frame + rate / 60 < x.len() {
        let w = &x[start..start + frame];
        let energy: f32 = w.iter().map(|v| v * v).sum();
        if (energy / frame as f32).sqrt() > peak * 0.05 {
            let (mut best, mut lag_best) = (0f32, 0usize);
            for lag in rate / 400..rate / 60 {
                let r: f32 = (0..frame).map(|i| w[i] * x[start + i + lag]).sum::<f32>() / energy;
                if r > best {
                    (best, lag_best) = (r, lag);
                }
            }
            if best > 0.5 {
                f0s.push(rate as f32 / lag_best as f32);
            }
        }
        start += hop;
    }
    if f0s.is_empty() {
        return 0.0;
    }
    f0s.sort_by(f32::total_cmp);
    f0s[f0s.len() / 2]
}

/// Every WAV of the folders, trimmed of its silences, joined with short gaps,
/// loudness-normalised and at the encoder's rate.
fn gather(dirs: &[PathBuf]) -> Result<Vec<f32>> {
    let mut all = Vec::new();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .collect();
        files.sort();
        for f in files {
            let (pcm, rate) = ptts::audio::decode_file(&f).map_err(|e| anyhow!("{}: {e}", f.display()))?;
            let pcm = ptts::audio::resample(pcm, rate as usize, RATE).map_err(|e| anyhow!("{e}"))?;
            all.extend(trim(&pcm));
            all.extend(std::iter::repeat_n(0.0, RATE * 15 / 100));
        }
        if all.len() > RATE * 12 {
            break;
        }
    }
    ptts::utils::normalize_loudness(&mut all, RATE as u32).map_err(|e| anyhow!("{e}"))?;
    println!("gathered {:.1}s of speech", all.len() as f32 / RATE as f32);
    Ok(all)
}

/// From the first 20 ms frame above a tenth of the loudest to the last, with
/// 100 ms either side.
fn trim(pcm: &[f32]) -> &[f32] {
    let frame = RATE / 50;
    let rms: Vec<f32> = pcm
        .chunks(frame)
        .map(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().cloned().fold(0.0, f32::max);
    let loud = |r: &&f32| **r > peak * 0.1;
    let (Some(a), Some(b)) = (rms.iter().position(|r| loud(&r)), rms.iter().rposition(|r| loud(&r))) else {
        return &[];
    };
    let start = (a * frame).saturating_sub(RATE / 10);
    let end = ((b + 1) * frame + RATE / 10).min(pcm.len());
    &pcm[start..end]
}

fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path)?;
    Ok(r.samples::<i16>().map(|s| s.map(|s| s as f32 / 32767.0)).collect::<Result<_, _>>()?)
}

fn write_wav(path: &Path, pcm: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in pcm {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}
