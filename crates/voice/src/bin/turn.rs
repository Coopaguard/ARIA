//! T2 — end of turn: when does the model say the speaker has stopped.
//!
//! Smart Turn v3 judges from the **waveform**, not the transcript, so it runs
//! beside the ASR instead of after it. Its own benchmark sheet measures French at
//! 94.09 % accuracy and 4.07 % false positives — a false positive being the
//! painful error, since it cuts the speaker off.
//!
//! **It is an arbiter, not a sensor**: a cheap energy gate finds the pauses, and
//! the model decides whether a pause ends the turn. That is what `segments` is
//! for, and it is why Handy ships a VAD.
//!
//! Two things flattened the first results, and only one of them was the usage.
//! Polling mid-sentence asks a question with no answer. But the curve was flat
//! mostly because the output named `logits` is **already a probability**, and a
//! second sigmoid squashed everything into `[0.5, 0.73]`. Read correctly, the
//! same recording separates a cut-off phrase at 0.12 from finished ones at 0.89
//! to 0.99. The reasoning now lives with the model, in
//! [`aria_voice::turn::Detector`].
//!
//! **The engine is not here.** The gate, the detector and the rule are in the
//! library, and this binary only measures them — so the bench and the live loop
//! cannot drift apart. Checked by re-running the labelled set after the move:
//! identical numbers, family by family.
//!
//! ```bash
//! cargo run --release -p aria-voice --bin turn                        # the graph's signature
//! cargo run --release -p aria-voice --bin turn -- --wav <file>        # arbitrate at each pause
//! cargo run --release -p aria-voice --bin turn -- --wav <file> --sweep # poll continuously
//! cargo run --release -p aria-voice --bin turn -- --set <dir>          # score the labelled set
//! ```
//!
//! `--set` is how the threshold stops being a guess: it replays the twenty
//! labelled utterances of `calibration/turn-set.tsv` and sweeps the threshold
//! across them, counting separately the error that cuts the speaker off and the
//! error that leaves ARIA looking deaf.

use anyhow::{Result, anyhow, bail};
use aria_voice::calibration::Calibration;
use aria_voice::{gate, mel, turn, wav};
use std::path::PathBuf;

/// How often `--sweep` asks. 160 ms is the granularity a voice loop can afford.
const STEP_SAMPLES: usize = 16 * mel::HOP;
/// Eight levels for the sparkline.
const LEVELS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

struct Args {
    model: PathBuf,
    wav: Option<PathBuf>,
    /// Directory holding `utterance-NN.wav` for the labelled calibration set.
    set: Option<PathBuf>,
    sweep: bool,
    /// Run the ASR too, and score the transcript rule beside the prosody one.
    transcribe: bool,
}

fn parse_args() -> Result<Args> {
    let mut model = None;
    let mut wav = None;
    let mut set = None;
    let mut sweep = false;
    let mut transcribe = false;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--wav" => {
                i += 1;
                wav = Some(PathBuf::from(
                    argv.get(i).ok_or_else(|| anyhow!("--wav wants a path"))?,
                ));
            }
            "--set" => {
                // Same convention as `dictate --save`: no directory means the
                // project's `samples/`, so the two commands always agree.
                match argv.get(i + 1) {
                    Some(value) if !value.starts_with("--") => {
                        i += 1;
                        set = Some(PathBuf::from(value));
                    }
                    _ => set = Some(aria_voice::samples_dir()),
                }
            }
            "--sweep" => sweep = true,
            "--transcribe" => transcribe = true,
            other if other.starts_with("--") => bail!("unknown argument: {other}"),
            positional => model = Some(PathBuf::from(positional)),
        }
        i += 1;
    }
    Ok(Args {
        model: model.unwrap_or_else(turn::Detector::default_model_path),
        wav,
        set,
        sweep,
        transcribe,
    })
}

fn bar(p: f32) -> String {
    let filled = (p * 24.0).round() as usize;
    format!("{}{}", "█".repeat(filled), "·".repeat(24 - filled.min(24)))
}

/// Every pause the gate found, with the probability that the turn ended there.
///
/// The window handed over is always the last 8 s of the **stream** ending at the
/// pause, silence included — see the comment in [`main`] for what trimming it
/// cost.
fn analyse(
    detector: &mut turn::Detector,
    samples: &[f32],
    cal: Calibration,
) -> Result<Vec<(usize, usize, f32)>> {
    let mut out = Vec::new();
    for segment in gate::segments(samples, cal) {
        let window = turn::Detector::window(samples, segment.to);
        out.push((segment.from, segment.to, detector.prosody(window)?));
    }
    Ok(out)
}

/// One line of `calibration/turn-set.tsv`: the file index, the expected verdict,
/// and what the speaker was asked to say.
///
/// The two ways of not being finished are kept apart because they are not the
/// same problem. A **hesitation** announces itself — filler, lengthened vowel,
/// pitch held up — and a waveform detector hears all of it. A **clean cut**
/// differs from a finished sentence by syntax alone, which no amount of
/// waveform tells you. Scoring them together would average a solved problem
/// with an unsolvable one and report a number that describes neither.
struct Case {
    index: usize,
    finished: bool,
    family: String,
    text: String,
}

fn calibration_cases() -> Result<Vec<Case>> {
    let mut out = Vec::new();
    for line in include_str!("../../calibration/turn-set.tsv").lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(index), Some(label), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("malformed line in turn-set.tsv: {line}");
        };
        let (finished, family) = match label.trim() {
            "fini" => (true, "fini"),
            "hesitation" => (false, "hésitation"),
            "coupure" => (false, "coupure"),
            other => bail!("unknown label {other:?} in turn-set.tsv"),
        };
        out.push(Case {
            index: index.trim().parse()?,
            finished,
            family: family.to_string(),
            text: text.trim().to_string(),
        });
    }
    Ok(out)
}

/// Scores the labelled set, then sweeps the threshold across it.
///
/// Four points gave a plausible 0.85 and no way to defend it. Twenty give the
/// two numbers that matter separately: a **false positive** cuts the speaker
/// off mid-sentence, a false negative leaves ARIA looking deaf. They are not
/// equally bad, so the recommendation below weighs them unequally.
fn calibrate(
    detector: &mut turn::Detector,
    dir: &std::path::Path,
    mut asr: Option<parakeet_rs::ParakeetTDT>,
    cal: Calibration,
) -> Result<()> {
    use parakeet_rs::Transcriber;

    let cases = calibration_cases()?;
    println!(
        "jeu       {} énoncés attendus dans {}\n",
        cases.len(),
        dir.display()
    );

    let mut scored: Vec<(&Case, f32, String)> = Vec::new();
    for case in &cases {
        let path = dir.join(format!("utterance-{:02}.wav", case.index));
        if !path.is_file() {
            println!("  {:>2}. manquant — {}", case.index, path.display());
            continue;
        }
        let samples = wav::read(&path)?;
        let found = analyse(detector, &samples, cal)?;
        let Some(&(_, _, last)) = found.last() else {
            println!("  {:>2}. aucun segment — fichier muet ?", case.index);
            continue;
        };
        // The whole file, which is what the loop would have accumulated by the
        // time it reaches this pause.
        let heard = match asr.as_mut() {
            Some(asr) => asr
                .transcribe_samples(samples.clone(), wav::SAMPLE_RATE, 1, None)
                .map_err(|e| anyhow!("transcription failed: {e}"))?
                .text
                .trim()
                .to_string(),
            // Without the ASR, an empty transcript reads as unfinished, so the
            // combined columns are left out rather than quietly wrong.
            None => String::new(),
        };
        // Only the last pause carries the label: an utterance with an internal
        // pause is still one turn, and the earlier scores are printed as
        // information, not counted.
        let earlier: Vec<String> = found[..found.len() - 1]
            .iter()
            .map(|&(_, _, p)| format!("{p:.2}"))
            .collect();
        println!(
            "  {:>2}. {:<11} {:.2}  {}  {}{}",
            case.index,
            case.family,
            last,
            bar(last),
            case.text,
            if earlier.is_empty() {
                String::new()
            } else {
                format!("   (pauses internes : {})", earlier.join(", "))
            }
        );
        if asr.is_some() {
            println!(
                "      {}  « {heard} »",
                if aria_voice::turn::looks_unfinished(&heard) {
                    "inachevé "
                } else {
                    "ponctué  "
                }
            );
        }
        scored.push((case, last, heard));
    }

    if scored.len() < cases.len() {
        println!(
            "\n  {} énoncés sur {} — le seuil se lit sur le jeu complet.",
            scored.len(),
            cases.len()
        );
    }
    if scored.is_empty() {
        bail!("nothing to calibrate");
    }

    // A false positive is "not finished, judged finished": ARIA speaks over the
    // user. A false negative is "finished, judged not": ARIA waits for a
    // sentence that will never come. They are not equally bad.
    let with_transcript = asr.is_some();
    let right_at = |threshold: f32, combined: bool| -> (usize, usize) {
        let mut cut = 0;
        let mut mute = 0;
        for (case, prosody, heard) in &scored {
            let over = if combined {
                aria_voice::turn::decide(*prosody, threshold, heard) == aria_voice::turn::Turn::Over
            } else {
                *prosody >= threshold
            };
            match (case.finished, over) {
                (false, true) => cut += 1,
                (true, false) => mute += 1,
                _ => {}
            }
        }
        (cut, mute)
    };

    if with_transcript {
        println!("\nseuil   prosodie seule        prosodie + ponctuation");
        println!("        coupe  muet  justes    coupe  muet  justes");
    } else {
        println!("\nseuil   coupe la parole   reste muet   justes");
    }
    let mut best = (0.0f32, -1.0f32);
    let mut safest: Option<(f32, usize)> = None;
    for step in 1..20 {
        let threshold = step as f32 / 20.0;
        let (cut, mute) = right_at(threshold, false);
        let right = scored.len() - cut - mute;
        if with_transcript {
            let (cut2, mute2) = right_at(threshold, true);
            let right2 = scored.len() - cut2 - mute2;
            println!(
                " {threshold:.2}     {cut:>3}   {mute:>3}   {right:>2}/{:<2}     {cut2:>3}   {mute2:>3}   {right2:>2}/{:<2}",
                scored.len(),
                scored.len()
            );
        } else {
            println!(
                " {threshold:.2}         {cut:>2}            {mute:>2}        {right:>2}/{}  {:.0} %",
                scored.len(),
                right as f32 * 100.0 / scored.len() as f32
            );
        }
        // The best threshold is read on the rule that will actually ship.
        let (cut, mute) = right_at(threshold, with_transcript);
        let accuracy = (scored.len() - cut - mute) as f32 / scored.len() as f32;
        if accuracy > best.1 {
            best = (threshold, accuracy);
        }
        if cut == 0 && safest.is_none() {
            safest = Some((threshold, mute));
        }
    }

    // Per family, and this is the line to read first: the average over the whole
    // set hides which of the two "wait" cases the model can actually hear.
    println!("\npar famille, au seuil {:.2}", best.0);
    for family in ["fini", "hésitation", "coupure"] {
        let mut scores: Vec<f32> = scored
            .iter()
            .filter(|(case, _, _)| case.family == family)
            .map(|(_, p, _)| *p)
            .collect();
        if scores.is_empty() {
            continue;
        }
        let wanted_over = family == "fini";
        let right = scored
            .iter()
            .filter(|(case, _, _)| case.family == family)
            .filter(|(_, prosody, heard)| {
                let over = if with_transcript {
                    aria_voice::turn::decide(*prosody, best.0, heard)
                        == aria_voice::turn::Turn::Over
                } else {
                    *prosody >= best.0
                };
                over == wanted_over
            })
            .count();
        scores.sort_by(f32::total_cmp);
        println!(
            " {family:<11} {right:>2}/{:<2}   médiane {:.2}   de {:.2} à {:.2}",
            scores.len(),
            scores[scores.len() / 2],
            scores[0],
            scores[scores.len() - 1]
        );
    }

    let suspended_high = scored
        .iter()
        .filter(|(c, _, _)| !c.finished)
        .map(|(_, p, _)| *p)
        .fold(0.0f32, f32::max);
    let finished_low = scored
        .iter()
        .filter(|(c, _, _)| c.finished)
        .map(|(_, p, _)| *p)
        .fold(1.0f32, f32::min);

    println!(
        "\n  plus haut suspendu {suspended_high:.2}, plus bas fini {finished_low:.2} — \
         marge {:.2}",
        finished_low - suspended_high
    );
    if finished_low > suspended_high {
        println!(
            "  Les deux classes ne se recouvrent pas : n'importe quel seuil entre les deux\n\
             sépare parfaitement. Prendre le milieu, {:.2}, pour la marge la plus large.",
            (finished_low + suspended_high) / 2.0
        );
    } else {
        println!(
            "  Les classes se recouvrent : aucun seuil ne les sépare, et c'est l'arbitrage\n\
             qui décide. Meilleure exactitude à {:.2} ({:.0} %).",
            best.0,
            best.1 * 100.0
        );
        match safest {
            Some((threshold, mute)) => println!(
                "  Pour ne jamais couper la parole : {threshold:.2}, au prix de {mute} énoncé(s)\n\
                 fini(s) où ARIA attend — c'est la faute qu'on préfère, et le temporisateur\n\
                 de 350 ms la rattrape.",
            ),
            None => println!(
                "  Aucun seuil n'évite de couper la parole : le modèle note au moins une\n\
                 suspension au-dessus de 0,95. Réécouter ce fichier avant de régler quoi que\n\
                 ce soit — c'est probablement l'énoncé, pas le modèle."
            ),
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = parse_args()?;

    // The shipped defaults. The bench measures *against* them, which is the
    // point: a calibration that does not beat them is not applied.
    let cal = Calibration::default();
    println!("modèle  {}", args.model.display());
    println!(
        "réglage seuil {:.2}, parole {} RMS, silence {} ms, voisé {} ms",
        cal.turn_threshold, cal.speech_rms, cal.silence_ms, cal.min_voiced_ms
    );
    let mut detector = turn::Detector::load(&args.model)?;

    if let Some(dir) = &args.set {
        println!();
        let asr = if args.transcribe {
            let dir = aria_voice::asr::default_model_dir();
            println!("ASR     {}", dir.display());
            Some(aria_voice::asr::load(&dir)?)
        } else {
            None
        };
        return calibrate(&mut detector, dir, asr, cal);
    }

    let Some(path) = args.wav else {
        return Ok(());
    };

    let samples = wav::read(&path)?;
    println!(
        "\nfichier {} — {:.1} s",
        path.display(),
        wav::seconds(&samples)
    );

    if args.sweep {
        let started = std::time::Instant::now();
        let mut curve: Vec<(f32, f32)> = Vec::new();
        for end in (STEP_SAMPLES..=samples.len()).step_by(STEP_SAMPLES) {
            let from = end.saturating_sub(mel::WINDOW_SAMPLES);
            let p = detector.prosody(&samples[from..end])?;
            curve.push((end as f32 / wav::SAMPLE_RATE as f32, p));
        }
        let elapsed = started.elapsed().as_millis().max(1);
        println!(
            "balayage {} fenêtres, pas de 160 ms — {:.1} ms par fenêtre\n",
            curve.len(),
            elapsed as f32 / curve.len() as f32
        );
        for row in curve.chunks(60) {
            let line: String = row
                .iter()
                .map(|&(_, p)| LEVELS[((p * 8.0).round() as usize).min(8)])
                .collect();
            println!("  {:>5.1}s │{line}│", row[0].0);
        }
        return Ok(());
    }

    // The gate before the gate: where the energy actually is. Printed because a
    // threshold chosen by guesswork is how three real cut-offs got clipped into
    // looking finished.
    let rms: Vec<f32> = samples
        .chunks(wav::SAMPLE_RATE as usize / 10)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let loudest = rms.iter().copied().fold(0.0f32, f32::max);
    println!(
        "énergie   crête {loudest:.3} RMS, seuil {} — un caractère par 100 ms\n",
        cal.speech_rms
    );
    for row in rms.chunks(80) {
        let line: String = row
            .iter()
            .map(|&r| LEVELS[((r / loudest.max(1e-6) * 8.0).round() as usize).min(8)])
            .collect();
        println!("          │{line}│");
    }
    println!();

    let started = std::time::Instant::now();
    let found = analyse(&mut detector, &samples, cal)?;
    println!(
        "segments  {} — porte d'énergie à {} RMS, {} ms de silence\n",
        found.len(),
        cal.speech_rms,
        cal.silence_ms
    );
    if found.is_empty() {
        bail!("aucun segment de parole trouvé — seuil trop haut, ou fichier muet");
    }

    let mut scores = Vec::new();
    // `analyse` hands over the last 8 s of the **stream** ending at each pause,
    // not the segment on its own. Handing over an isolated 2 s phrase leaves six
    // seconds of zeros after it, so the model sees speech that stopped long ago
    // and answers "finished" — which is how three genuine cut-offs scored 0.97.
    for (i, &(from, to, p)) in found.iter().enumerate() {
        println!(
            "  {:>2}. {:>5.1} → {:>5.1} s  ({:.1} s)   {:.2}  {}",
            i + 1,
            from as f32 / wav::SAMPLE_RATE as f32,
            to as f32 / wav::SAMPLE_RATE as f32,
            (to - from) as f32 / wav::SAMPLE_RATE as f32,
            p,
            bar(p)
        );
        scores.push(p);
    }
    println!(
        "\n  {:.1} ms par décision",
        started.elapsed().as_millis() as f32 / scores.len() as f32
    );

    let high = scores.iter().copied().fold(f32::MIN, f32::max);
    let low = scores.iter().copied().fold(f32::MAX, f32::min);
    println!(
        "  minimum {low:.2}, maximum {high:.2}, amplitude {:.2}",
        high - low
    );
    if high - low < 0.3 {
        println!(
            "\n  ATTENTION : moins de 0,3 d'amplitude entre les segments. Un modèle qui ne\n\
             discrimine pas, c'est d'abord une convention de traits fausse — pas un mauvais\n\
             modèle, et pas un mauvais seuil."
        );
    }

    Ok(())
}
