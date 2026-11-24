//! Phonon, the voice that runs **in this process**.
//!
//! The only [`Voice`] that speaks since 2026-11-19 ([`super::kyutai`] is
//! suspended). It loads Gradium's model through [`ptts`] — published on
//! crates.io, MIT OR Apache-2.0, built on `xn` — in ARIA's own memory.
//!
//! # Why it exists when Kyutai already works
//!
//! **The honest answer has shrunk, and it is worth saying so rather than
//! leaving the old one standing.** What this header used to claim was that the
//! synthesis is a stream where Kyutai's was a finished WAV, and that it loads
//! in 0.23 s where a server took seconds. The first half was the argument that
//! justified the engine.
//!
//! Decision 56 took it away. Kyutai is linked now, streams through the same
//! [`Voice::stream`] this engine introduced, and has no port, no child and no
//! WAV header announcing two gigabytes either. The comparison that is left:
//!
//! | | Phonon | Kyutai, lié |
//! |---|---|---|
//! | Chargement | **0,23 s** | quelques secondes, ~400 Mo |
//! | Premier morceau | 67–141 ms | ~200 ms annoncées |
//! | Synthèse en flux | oui | oui |
//! | Licence du français | CC-BY-NC, à accepter | `estelle` est CC0 |
//!
//! So it stays as the **light** engine and as the second implementation that
//! keeps the trait honest — a `Voice` with one implementation is an interface
//! nobody has tested — but it is no longer the only one that streams, and
//! nothing should be written here as though it were.
//!
//! # The catch, and it is a licence
//!
//! Relevé le 2026-10-29 sur Hugging Face. Of Gradium's published models, only
//! **`phonon-31m-en`** is `apache-2.0` and open. Every 40 M model — including
//! the French one and the multilingual one — is **`cc-by-nc-4.0` and gated**.
//!
//! ARIA is free of charge and not for commercial use, so it may use them — as
//! long as both stay true. The **French one is ours to hand out**, unmodified
//! and with its notice, so that nobody has to create a token (see
//! [`FRENCH_WEIGHTS`]); the others are downloaded by a user who accepted their
//! licence. Every such model carries
//! [`Model::commercial`] `false` so the settings screen says it out loud. The
//! **voices**, by contrast, are `apache-2.0` throughout.
//!
//! # Why ARIA fetches the files itself
//!
//! `ptts` has its own resolver — `Checkpoint::locate("hf://owner/name")` — and
//! it is not used, for a reason worth writing down because it is the second
//! time the same trap appears. Its `Repo::open` calls `hf_hub::HFClientSync::new()`
//! under a comment saying *"the client reads `HF_TOKEN`, `HF_ENDPOINT` and the
//! cache location from the environment"*. That was true of `hf-hub` 0.x. In
//! 1.0 — the version it depends on — the client reads **no** environment
//! variable, talks to the Hub **unauthenticated**, and caches relative to the
//! current working directory. Kyutai's own `download.rs` carries the identical
//! wrong comment, and decision 52 is what it cost us there.
//!
//! Two consequences, both of which the resolver would inflict:
//!
//! - **a gated model is unreachable**, since no token is ever sent — and the
//!   error it then prints tells the reader to set `HF_TOKEN`, which would not
//!   help. Every French Phonon model is gated, so that is not a corner case;
//! - the weights would land wherever ARIA happened to be launched from.
//!
//! So the client is built here with the cache and the token stated outright.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use ptts::preprocess::Lang;
use ptts::synth::Synth;

use super::{Utterance, Voice};
use crate::runtime::Archive;

/// The model loaded when the setting is empty and the language is not French.
///
/// The English 31 M: the only one of Gradium's models that is `apache-2.0` and
/// ungated.
pub const DEFAULT_MODEL: &str = "gradium/phonon-31m-en";

/// The model loaded when the setting is empty and the language is French.
///
/// **Changed on 2026-11-19, by the user's choice** (decision 121). The French
/// voices of the catalogue are only voice embeddings: run through the English
/// model they speak French with an English accent, which is what made them
/// unusable. This one is CC-BY-NC, and gated on Hugging Face: ARIA takes it
/// from its own release, see [`FRENCH_WEIGHTS`] and
/// `docs/todos/phonon-licences.md`.
pub const FRENCH_MODEL: &str = "gradium/phonon-40m-fr";

/// The voice an empty setting means with the French model — Noémie, the first
/// of Gradium's French list. Not the model's own `default-voice.safetensors`,
/// which is not shipped; this one is, and is `apache-2.0`.
const FRENCH_VOICE: &str = "FXxJ9mANRq6BCTX5";

/// The weights of the French model, from a release of ours that holds data and
/// no program, so that an update of ARIA does not carry 122 MB again. The small
/// files and the notice are in `app/resources/phonon/`. Revision
/// `d9108697` of Gradium's repository; another revision goes in `models-2`.
const FRENCH_WEIGHTS: Archive = Archive {
    url: "https://github.com/Coopaguard/ARIA/releases/download/models-1/phonon-40m-fr.safetensors",
    size: 121_713_400,
    sha256: "6ca89d3f7ee732290e23d26680c523ad655b2be228849cc93a62b823857ea577",
    prefix: "",
};

/// The model an empty setting means, for this language.
pub fn default_for(language: &str) -> &'static str {
    match language.to_lowercase().split(['-', '_']).next().unwrap_or("") {
        "fr" => FRENCH_MODEL,
        _ => DEFAULT_MODEL,
    }
}

/// The model in force: the one named, or the language's default.
pub fn resolve(repo: &str, language: &str) -> Option<&'static Model> {
    let repo = repo.trim();
    model(if repo.is_empty() { default_for(language) } else { repo })
}

/// Where the voices live. `apache-2.0`, ungated, and the same for every model.
const VOICES_REPO: &str = "gradium/phonon-voices";
const VOICES_MANIFEST: &str = "voices.jsonl";

/// The voice set the installer carries, decision 133: all 102 voices of
/// `gradium/phonon-voices` (Apache-2.0, 26 MB), pinned at one revision in
/// `app/resources/voices/`. With it the catalogue and every voice are read
/// from disk and the Hub is only asked for the model. Without it — a resource
/// folder that is missing — the Hub, as before.
fn shipped_voices() -> Option<PathBuf> {
    shipped("voices", &[VOICES_MANIFEST])
}

/// What the installer carries of the French model: everything but the weights.
fn shipped_french() -> Option<PathBuf> {
    shipped("phonon/phonon-40m-fr", &["config.json", "tokenizer.json"])
}

/// Where its weights land. Whole, or absent: see [`crate::runtime::download`].
fn french_weights() -> PathBuf {
    cache_dir().join("phonon-40m-fr.safetensors")
}

/// The French model, without the Hub: the shipped files, and the weights
/// downloaded the first time.
fn french_files() -> Result<[PathBuf; 3]> {
    // The voice that speaks and the one being previewed may both ask.
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().expect("never panicked through");
    let dir = shipped_french().context("app/resources/phonon est incomplet")?;
    let weights = french_weights();
    if !weights.is_file() {
        std::fs::create_dir_all(cache_dir())?;
        let _over = crate::models::Over;
        crate::runtime::download(&FRENCH_WEIGHTS, &weights, 1, 1)
            .with_context(|| format!("{} ne descend pas", FRENCH_WEIGHTS.url))?;
    }
    Ok([dir.join("config.json"), weights, dir.join("tokenizer.json")])
}

/// Any model, from the Hub.
fn hub_files(client: &hf_hub::HFClientSync, repo: &str) -> Result<[PathBuf; 3]> {
    Ok([
        fetch(client, repo, "config.json")?,
        fetch(client, repo, "model.safetensors")?,
        fetch(client, repo, "tokenizer.json")?,
    ])
}

/// The three files a model is read from. The French one falls back on the Hub —
/// where a build made before the release is published, or a fork, finds it with
/// a token — and the error said is then that of our own download.
fn model_files(client: &hf_hub::HFClientSync, repo: &str) -> Result<[PathBuf; 3]> {
    if repo != FRENCH_MODEL {
        return hub_files(client, repo);
    }
    french_files().or_else(|ours| {
        tracing::warn!("{ours:#} : essai depuis Hugging Face");
        hub_files(client, repo).map_err(|hub| {
            tracing::warn!("{hub:#}");
            ours
        })
    })
}

static RESOURCES: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Where the application's resources are: `voices/` and `phonon/` below.
pub fn set_resources(resources: PathBuf) {
    let _ = RESOURCES.set(resources);
}

/// A resource folder, if every one of these files is in it.
fn shipped(folder: &str, files: &[&str]) -> Option<PathBuf> {
    let set = RESOURCES.get().cloned();
    // `cargo run` of a bench has no resource folder: the repository's copy.
    let repo = cfg!(debug_assertions)
        .then(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../app/resources"));
    [set, repo]
        .into_iter()
        .flatten()
        .map(|root| root.join(folder))
        .find(|dir| files.iter().all(|file| dir.join(file).is_file()))
}

/// One model on offer.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    /// The Hub repository.
    pub repo: &'static str,
    /// The language it was trained for, as a tag — `fr`, `en`.
    pub language: &'static str,
    /// Whether its licence allows commercial use. See the module header.
    pub commercial: bool,
    /// Whether the user needs an accepted licence and a token. Not the French
    /// model's case, though its repository is gated: see [`FRENCH_WEIGHTS`].
    pub gated: bool,
    /// The languages of the voices offered with it. The French model offers
    /// the French voices, the multilingual one offers all the others: no voice
    /// is listed under two models of the same screen.
    pub voices: &'static [&'static str],
}

/// Every Phonon model ARIA knows about.
///
/// Relevé le 2026-10-29. A list and not a Hub query, because a settings screen
/// that cannot open without the network is a settings screen nobody can use to
/// fix their network — and because the licence of each one is a fact we
/// checked, not one the API reports usefully.
pub const MODELS: &[Model] = &[
    Model {
        repo: DEFAULT_MODEL,
        language: "en",
        commercial: true,
        gated: false,
        voices: &["en"],
    },
    Model {
        repo: FRENCH_MODEL,
        language: "fr",
        commercial: false,
        gated: false,
        voices: &["fr"],
    },
    Model {
        repo: "gradium/phonon-40m-multilingual",
        language: "fr",
        commercial: false,
        gated: true,
        voices: &["de", "en", "es", "pt"],
    },
    Model {
        repo: "gradium/phonon-40m-en",
        language: "en",
        commercial: false,
        gated: true,
        voices: &["en"],
    },
];

pub fn model(repo: &str) -> Option<&'static Model> {
    let repo = repo.trim();
    let wanted = if repo.is_empty() { DEFAULT_MODEL } else { repo };
    MODELS.iter().find(|m| m.repo == wanted)
}

/// One voice from `voices.jsonl`.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Catalogued {
    /// Path inside the repository.
    pub filename: String,
    /// What a person calls it — `Noémie`, `Augustin`.
    pub name: String,
    pub voice_id: String,
    pub language: String,
    /// `null` for the twelve character voices, which a plain `String` turned
    /// into a parse error — and the line was skipped without a word.
    #[serde(default, deserialize_with = "null_is_empty")]
    pub gender: String,
    #[serde(default, deserialize_with = "null_is_empty")]
    pub description: String,
}

fn null_is_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(<Option<String> as serde::Deserialize>::deserialize(d)?.unwrap_or_default())
}

/// Where the models and voices land.
///
/// ARIA's own folder, beside the ASR's 2.4 GB, for the reason the module header
/// gives: nothing in the library picks this for us, and the default it would
/// fall into is "wherever the process started".
pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ARIA_PHONON_CACHE") {
        return PathBuf::from(dir);
    }
    crate::models_root().join("phonon")
}

/// The Hub client, with **our** cache — the one this engine's weights live in.
///
/// The client itself is built by [`crate::models::client_with_cache`], which is
/// where the token policy lives since 2026-11-13: the ear needs exactly the
/// same one, and `hf-hub` 1.0 reads no environment variable, so one place to
/// get it wrong is enough. What stays here is the cache, because it is this
/// engine's folder and nobody else's.
fn client() -> Result<hf_hub::HFClientSync> {
    crate::models::client_with_cache(&cache_dir())
}

/// One file from a repository, downloaded or found in the cache.
fn fetch(client: &hf_hub::HFClientSync, repo: &str, file: &str) -> Result<PathBuf> {
    let (owner, name) = repo.split_once('/').context("owner/name attendu")?;
    let (relay, _over) = crate::models::watched(file);
    client
        .model(owner, name)
        .download_file()
        .filename(file.to_string())
        .progress(relay)
        .send()
        .map_err(|e| gated_hint(repo, file, e))
}

/// The failure a newcomer hits, with what to do about it.
fn gated_hint(repo: &str, file: &str, e: hf_hub::HFError) -> anyhow::Error {
    let gated = model(repo).is_some_and(|m| m.gated);
    if gated {
        anyhow!(
            "{repo}/{file} ne descend pas : {e}\n\
             Ce dépôt demande d'accepter sa licence à la main. Accepte-la sur \
             huggingface.co/{repo} avec ton compte, puis mets ton jeton dans HF_TOKEN \
             (ou lance `hf auth login` une fois).",
        )
    } else {
        anyhow!("{repo}/{file} ne descend pas : {e}")
    }
}

/// The voices this engine can speak with, for the settings screen.
///
/// Reads the shipped manifest, or the Hub without it. `language` filters it; an
/// empty filter returns everything. Which ones a model offers is
/// [`Model::voices`].
pub fn catalogue(language: &str) -> Result<Vec<Catalogued>> {
    let manifest = match shipped_voices() {
        Some(dir) => dir.join(VOICES_MANIFEST),
        None => fetch(&client()?, VOICES_REPO, VOICES_MANIFEST)?,
    };
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("{} ne se lit pas", manifest.display()))?;
    let wanted = language.trim().to_lowercase();
    let mut voices: Vec<Catalogued> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        // A malformed line is skipped rather than fatal: it is someone else's
        // file, and one unreadable entry must not empty the list.
        .filter_map(|line| serde_json::from_str::<Catalogued>(line).ok())
        .filter(|voice| wanted.is_empty() || voice.language.eq_ignore_ascii_case(&wanted))
        .collect();
    voices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(voices)
}

/// Whether the model's files are already on this machine.
///
/// Our own cache, in `hf-hub`'s layout — a probe of someone else's shape, so it
/// decides what a button says and never a result. The same caveat as
/// [`super::kyutai::weights_cached`], and the same reason.
///
/// `repo` is a resolved repository — see [`resolve`]. The folder is directly
/// under [`cache_dir`]: this probed `.cache/huggingface/hub` beneath it until
/// 2026-11-19, a layout `hf-hub` 1.0 does not write, so it always said no.
pub fn weights_cached(repo: &str) -> bool {
    let Some(repo) = model(repo).map(|m| m.repo) else {
        return false;
    };
    if repo == FRENCH_MODEL && french_weights().is_file() {
        return true;
    }
    let folder = format!("models--{}", repo.replace('/', "--"));
    let snapshots = cache_dir().join(folder).join("snapshots");
    let Ok(entries) = std::fs::read_dir(&snapshots) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|snapshot| {
        std::fs::read_dir(snapshot.path()).is_ok_and(|files| {
            files.filter_map(Result::ok).any(|file| {
                file.path()
                    .extension()
                    .is_some_and(|ext| ext == "safetensors")
            })
        })
    })
}

/// What a voice is opened with.
pub struct Setup {
    /// A repository from [`MODELS`], or empty for the language's default —
    /// see [`default_for`].
    pub model: String,
    /// A `voice_id` from [`catalogue`], or empty for the model's own
    /// `default-voice.safetensors`.
    pub voice: String,
    /// The language tag the text is normalised for — `fr`, `en`.
    pub language: String,
}

/// The language `ptts` normalises text for.
///
/// Only English and French exist in `preprocess::Lang`, and the fallback is
/// **off** rather than English: normalisation rewrites numbers and symbols into
/// the words of a language, so applying English rules to Spanish would say the
/// numbers in English. Nothing is worse than nothing here.
fn normalize(tag: &str) -> ptts::preprocess::Normalize {
    match tag.to_lowercase().split(['-', '_']).next().unwrap_or("") {
        "fr" => Lang::Fr.into(),
        "en" => Lang::En.into(),
        _ => ptts::preprocess::Normalize::OFF,
    }
}

/// A voice that synthesises in this process.
pub struct PhononVoice {
    /// `&mut` is needed to register a voice, so the whole thing sits behind a
    /// lock. Uncontended in practice — one speech thread — and the alternative
    /// would be deciding the voice before the model is loaded, which is what
    /// `SynthBuilder::voice` refuses for good reason.
    synth: Mutex<Synth>,
    /// The model, so [`Voice::revoice`] can find its default voice.
    repo: &'static str,
    /// Behind a lock because [`Voice::revoice`] changes it through `&self`.
    label: Mutex<String>,
}

impl PhononVoice {
    /// Load the model, and the voice with it.
    ///
    /// Seconds on a first run while the weights come down, then about a quarter
    /// of a second. The caller registers that wait with `Load::Voice`, which is
    /// what puts it on the orb.
    pub fn open(setup: &Setup) -> Result<Self> {
        let chosen = resolve(&setup.model, &setup.language)
            .ok_or_else(|| anyhow!("modèle Phonon inconnu : {}", setup.model))?;
        let client = client()?;

        let started = Instant::now();
        let [config, weights, tokenizer] = model_files(&client, chosen.repo)?;
        let fetched = started.elapsed();

        let loading = Instant::now();
        let mut synth = Synth::builder(
            ptts::checkpoint::read_config(&config)
                .map_err(|e| anyhow!("config illisible : {e}"))?,
            &weights,
            normalize(&setup.language),
        )
        .tokenizer_file(&tokenizer)
        .build()
        .map_err(|e| anyhow!("le modèle Phonon ne charge pas : {e}"))?;

        let (path, voice) = voice_file(&client, chosen, &setup.voice)?;
        register(&mut synth, &path, &voice)?;
        tracing::info!(
            repo = chosen.repo,
            %voice,
            fetch_s = fetched.as_secs_f32(),
            load_s = loading.elapsed().as_secs_f32(),
            device = %synth.device_name(),
            rate = synth.sample_rate(),
            "modèle Phonon en mémoire"
        );
        if !chosen.commercial {
            // Said at every start, not once in a settings tooltip: a licence
            // that forbids commercial use must not be missing from the log.
            tracing::warn!(
                repo = chosen.repo,
                "ce modèle est en CC-BY-NC : pas d'usage commercial"
            );
        }

        Ok(Self {
            synth: Mutex::new(synth),
            repo: chosen.repo,
            label: Mutex::new(format!("Phonon {} · {voice}", chosen.repo)),
        })
    }
}

/// Where the chosen voice's file is, and the name a person reads.
///
/// Empty means [`FRENCH_VOICE`] with the French model, the model's own voice
/// otherwise. Fetched before any lock is taken: the
/// first time it is a download, and the synth must stay free to speak.
fn voice_file(
    client: &hf_hub::HFClientSync,
    chosen: &Model,
    wanted: &str,
) -> Result<(PathBuf, String)> {
    let wanted = match wanted.trim() {
        "" if chosen.repo == FRENCH_MODEL => FRENCH_VOICE,
        wanted => wanted,
    };
    if wanted.is_empty() {
        let path = fetch(client, chosen.repo, "default-voice.safetensors")?;
        return Ok((path, "la voix du modèle".to_string()));
    }
    // By `voice_id` and not by name: a name is what a person reads, an
    // identifier is what survives the catalogue being reordered or translated.
    let entry = catalogue("")?
        .into_iter()
        .find(|voice| voice.voice_id == wanted)
        .ok_or_else(|| anyhow!("voix Phonon inconnue : {wanted}"))?;
    let shipped = shipped_voices()
        .map(|dir| dir.join(&entry.filename))
        .filter(|path| path.is_file());
    let path = match shipped {
        Some(path) => path,
        None => fetch(client, VOICES_REPO, &entry.filename)?,
    };
    Ok((path, entry.name))
}

/// Register a voice as **the** voice of this synth.
///
/// Always under `default`, the name `ptts` speaks in when a request names
/// none. Under the voice's own id, a second voice added by [`Voice::revoice`]
/// would have lost to whichever id sorted first. Same name means replaced, and
/// `ptts` forgets the old conditioning on its own.
fn register(synth: &mut Synth, path: &std::path::Path, name: &str) -> Result<()> {
    synth
        .add_voice_file("default", path)
        .map_err(|e| anyhow!("la voix {name} ne charge pas : {e}"))
}

impl Voice for PhononVoice {
    fn name(&self) -> String {
        self.label.lock().expect("label poisoned").clone()
    }

    fn revoice(&self, voice: &str) -> Result<bool> {
        let chosen = model(self.repo).ok_or_else(|| anyhow!("modèle Phonon inconnu"))?;
        let (path, name) = voice_file(&client()?, chosen, voice)?;
        // Taken only now, for one small file: a sentence being spoken holds
        // this lock, so the change lands between two sentences.
        register(&mut self.synth.lock().expect("synth poisoned"), &path, &name)?;
        *self.label.lock().expect("label poisoned") = format!("Phonon {} · {name}", self.repo);
        Ok(true)
    }

    fn speak(&self, text: &str) -> Result<Utterance> {
        let synth = self.synth.lock().expect("synth poisoned");
        let samples = synth
            .say(text)
            .map_err(|e| anyhow!("la synthèse a échoué : {e}"))?;
        Ok(Utterance {
            samples,
            sample_rate: synth.sample_rate(),
        })
    }

    /// The reason this engine is here: each piece goes to the loudspeakers as
    /// it exists, rather than the sentence going as a block once it is whole.
    fn stream(
        &self,
        text: &str,
        on_chunk: &mut (dyn FnMut(Utterance) -> Result<()> + Send),
    ) -> Result<()> {
        let synth = self.synth.lock().expect("synth poisoned");
        let rate = synth.sample_rate();
        let stream = synth
            .stream(text)
            .map_err(|e| anyhow!("le flux de synthèse n'a pas démarré : {e}"))?;
        for chunk in stream {
            let samples = chunk.map_err(|e| anyhow!("un morceau a échoué : {e}"))?;
            // Dropping the iterator here stops the worker threads, so an
            // interrupted sentence is abandoned rather than finished for
            // nobody — which is what `Player::say` relies on.
            on_chunk(Utterance {
                samples,
                sample_rate: rate,
            })?;
        }
        Ok(())
    }
}

/// A sentence this engine can be checked against without a model.
pub fn describe(repo: &str) -> String {
    match model(repo) {
        Some(m) if m.commercial => format!("{} — licence ouverte", m.repo),
        Some(m) => format!("{} — CC-BY-NC, pas d'usage commercial", m.repo),
        None => format!("{repo} — modèle inconnu"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The installer carries the whole set, and every line names a file that
    /// is there: a missing one would send that voice back to the Hub in silence.
    #[test]
    fn the_shipped_voices_are_all_there() {
        let dir = shipped_voices().expect("app/resources/voices");
        let voices = catalogue("").unwrap();
        assert_eq!(voices.len(), 102);
        assert_eq!(voices.iter().filter(|v| v.language == "fr").count(), 15);
        for voice in &voices {
            assert!(dir.join(&voice.filename).is_file(), "{}", voice.filename);
        }
    }

    /// The French model and the multilingual one share the whole set out
    /// between them: every voice is offered by one of the two, none by both.
    #[test]
    fn french_and_multilingual_share_out_every_voice() {
        let offered = |repo: &str| {
            let languages = model(repo).expect("catalogued").voices;
            catalogue("")
                .unwrap()
                .into_iter()
                .filter(|v| languages.contains(&v.language.as_str()))
                .map(|v| v.voice_id)
                .collect::<std::collections::HashSet<_>>()
        };
        let french = offered(FRENCH_MODEL);
        let others = offered("gradium/phonon-40m-multilingual");
        assert_eq!(french.len(), 15);
        assert_eq!(others.len(), 87);
        assert!(french.is_disjoint(&others));
    }

    /// The default of the French model is one of the shipped French voices, and
    /// all of the model but its weights is in the repository.
    #[test]
    fn the_french_model_is_shipped_with_its_default_voice() {
        let french = catalogue("fr").unwrap();
        assert!(french.iter().any(|v| v.voice_id == FRENCH_VOICE));
        for file in ["config.json", "tokenizer.json", "NOTICE.txt"] {
            assert!(shipped("phonon/phonon-40m-fr", &[file]).is_some(), "{file}");
        }
    }

    /// The French voice speaks with the shipped files and our own weights,
    /// without the Hub.
    #[test]
    #[ignore = "loads the French model, and downloads it the first time"]
    fn the_french_model_speaks_without_the_hub() {
        french_files().expect("our own files");
        let setup = Setup {
            model: String::new(),
            voice: String::new(),
            language: "fr".into(),
        };
        let voice = PhononVoice::open(&setup).expect("opens");
        assert!(voice.name().contains("Noémie"), "{}", voice.name());
        let said = voice.speak("Bonjour, je suis Aria.").expect("speaks");
        assert!(said.samples.len() as u32 > said.sample_rate / 2);
    }

    /// Outside French, the default is the open model nobody has to accept.
    #[test]
    fn the_default_model_is_commercially_licensed_and_ungated() {
        let default = resolve("", "en").expect("the default is catalogued");
        assert!(default.commercial, "the default must have an open licence");
        assert!(!default.gated, "the default must not need an accepted licence");
        assert_eq!(default.repo, DEFAULT_MODEL);
    }

    /// Decision 121: in French, an empty setting means the French model —
    /// the catalogue's French voices on the English model have an accent.
    #[test]
    fn in_french_the_default_is_the_french_model() {
        for tag in ["fr", "fr-FR", "FR_ca"] {
            assert_eq!(resolve("", tag).map(|m| m.repo), Some(FRENCH_MODEL));
        }
        assert_eq!(
            resolve("gradium/phonon-31m-en", "fr").map(|m| m.repo),
            Some(DEFAULT_MODEL),
            "a model chosen by hand is never replaced"
        );
    }

    /// And the honest counterpart: the French models are non-commercial, and
    /// the catalogue says so. A list where everything is allowed would mean the
    /// licences were never looked up.
    #[test]
    fn every_french_model_is_flagged_non_commercial() {
        let french: Vec<&Model> = MODELS.iter().filter(|m| m.language == "fr").collect();
        assert!(
            !french.is_empty(),
            "a French-speaking assistant needs French models listed"
        );
        for m in french {
            assert!(
                !m.commercial,
                "{} is French and claims to be commercial",
                m.repo
            );
            assert_eq!(m.gated, m.repo != FRENCH_MODEL, "{}: ours needs no token", m.repo);
        }
    }

    #[test]
    fn an_empty_setting_resolves_to_the_default() {
        assert_eq!(model("   ").map(|m| m.repo), Some(DEFAULT_MODEL));
        assert_eq!(
            model("gradium/phonon-40m-fr").map(|m| m.language),
            Some("fr")
        );
        assert!(model("someone/else").is_none());
    }

    /// An unknown language normalises to nothing rather than to English:
    /// English rules on Spanish would say the numbers in English.
    #[test]
    fn an_unknown_language_turns_normalisation_off() {
        assert_eq!(normalize("es"), ptts::preprocess::Normalize::OFF);
        assert_eq!(normalize(""), ptts::preprocess::Normalize::OFF);
        assert_ne!(normalize("fr"), ptts::preprocess::Normalize::OFF);
        assert_ne!(normalize("fr-FR"), ptts::preprocess::Normalize::OFF);
    }

    #[test]
    fn the_cache_is_arias_own_folder() {
        if std::env::var_os("ARIA_PHONON_CACHE").is_some() {
            return;
        }
        assert!(cache_dir().ends_with(Path::new("aria").join("models").join("phonon")));
    }

    #[test]
    fn a_missing_cache_holds_no_weights() {
        assert!(!weights_cached("gradium/phonon-does-not-exist"));
    }
}
