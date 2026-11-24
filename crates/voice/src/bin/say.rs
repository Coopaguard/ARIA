//! T4 — the bench for the voice.
//!
//! ```bash
//! cargo run --release -p aria-voice --bin say -- "Bonjour, je suis Aria."   # Phonon
//! cargo run --release -p aria-voice --bin say -- --voices
//! cargo run --release -p aria-voice --bin say -- --cut "Une phrase assez longue pour être coupée."
//! cargo run --release -p aria-voice --bin say -- --kyutai "Bonjour, je suis Aria."
//! cargo run --release -p aria-voice --bin say -- --kyutai --voice marius --quantize "…"
//! ```
//!
//! It prints the two numbers T0's budget is about — how long synthesis took,
//! and how long the result lasts — because a voice that sounds good and starts
//! after a second and a half is not the voice this product can use.
//!
//! `--kyutai` is here for one reason: the figures that choose an engine have
//! to be taken on the target machine, not read off a README. It also prints
//! the **model load**, which is the number the loading bar was rebuilt around
//! and the one nothing else reports.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use aria_voice::tts::{Player, Playing, kyutai};

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut cut = false;
    let mut kyutai = false;
    let mut phonon = false;
    let mut quantize = false;
    let mut chosen = String::new();
    let mut words: Vec<String> = Vec::new();
    let mut argv = argv.iter();
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--voices" => return list(kyutai, phonon),
            "--cut" => cut = true,
            "--kyutai" => kyutai = true,
            "--phonon" => phonon = true,
            "--quantize" => quantize = true,
            "--voice" => {
                chosen = argv
                    .next()
                    .ok_or_else(|| anyhow!("--voice attend un nom"))?
                    .clone();
            }
            "-h" | "--help" => {
                println!(
                    "say [--voices] [--cut] [--kyutai [--quantize] | --phonon] [--voice <nom>] <texte…>"
                );
                return Ok(());
            }
            other => words.push(other.to_string()),
        }
    }

    let text = if words.is_empty() {
        "Bonjour. Je suis Aria. Il fera vingt-trois degrés ce matin.".to_string()
    } else {
        words.join(" ")
    };

    let started = Instant::now();
    // Phonon unless `--kyutai`: the Windows voice that used to be the default
    // was removed on 2026-11-19.
    let voice = if kyutai && !phonon {
        kyutai_voice(&chosen, quantize)?
    } else {
        phonon_voice(&chosen)?
    };
    let loaded = started.elapsed();
    let player = Player::open(voice)?;
    println!("voix    {}", player.voice_name());
    // Separately from the synthesis, because they are paid at different
    // moments: this one once per run, the other once per sentence.
    println!("chargement {:.1} s", loaded.as_secs_f32());
    println!("texte   « {text} »");

    let took = player.say(&text)?;
    println!("synthèse {} ms", took.as_millis());

    if cut {
        // Decision 17, by hand: speech is cut mid-word and the queue goes with
        // it. What you should hear is an abrupt stop, not a fade.
        std::thread::sleep(Duration::from_millis(700));
        println!("— coupure —");
        player.silence();
        std::thread::sleep(Duration::from_millis(300));
        if player.state() == Playing::Speaking {
            bail!("la coupure n'a pas pris : le lecteur parle encore");
        }
        println!("silence immédiat : la file a été jetée");
        return Ok(());
    }

    // The stream is real time, so the wait is the audio's own length plus a
    // margin for the device's buffer.
    while player.state() == Playing::Speaking {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    println!(
        "référence d'écho : {} échantillons",
        player.reference().len()
    );
    Ok(())
}

/// Phonon, in this process.
///
/// `--voice` takes a `voice_id` from the catalogue; `ARIA_PHONON_MODEL` picks a
/// model other than the open English default, and `ARIA_PHONON_LANG` the
/// language its text normalisation uses.
fn phonon_voice(chosen: &str) -> Result<Box<dyn aria_voice::tts::Voice>> {
    let setup = aria_voice::tts::phonon::Setup {
        model: std::env::var("ARIA_PHONON_MODEL").unwrap_or_default(),
        voice: chosen.to_string(),
        language: std::env::var("ARIA_PHONON_LANG").unwrap_or_else(|_| "fr".into()),
    };
    Ok(Box::new(aria_voice::tts::phonon::PhononVoice::open(
        &setup,
    )?))
}

/// Kyutai, loaded into this process. Nothing to start and nothing to kill.
fn kyutai_voice(chosen: &str, quantize: bool) -> Result<Box<dyn aria_voice::tts::Voice>> {
    let setup = kyutai::Setup {
        language: kyutai::model_for("fr").to_string(),
        voice: chosen.to_string(),
        quantize,
    };
    Ok(Box::new(kyutai::KyutaiVoice::open(&setup)?))
}

fn list(kyutai: bool, phonon: bool) -> Result<()> {
    if phonon || !kyutai {
        // The only catalogue of the three with names a person would recognise,
        // so it prints the name, the gender and the identifier the setting
        // actually stores.
        let language = std::env::var("ARIA_PHONON_LANG").unwrap_or_else(|_| "fr".into());
        let voices = aria_voice::tts::phonon::catalogue(&language)?;
        println!("{} voix Phonon en {language}", voices.len());
        for voice in voices {
            println!(
                "  {:<22} {:<8} {}",
                voice.name, voice.gender, voice.voice_id
            );
        }
        return Ok(());
    }
    println!("{} voix livrées avec le modèle", kyutai::VOICES.len());
    for voice in kyutai::VOICES {
        println!(
            "  {:<18} {}",
            voice.id,
            if voice.commercial {
                "usage commercial"
            } else {
                "NON commercial"
            }
        );
    }
    Ok(())
}
