//! Kyutai's Pocket TTS, **linked** rather than launched.
//!
//! **Suspended since 2026-11-19**, by the user's choice: Phonon alone speaks,
//! and the application resolves a `kyutai` setting to it. The code stays
//! compiled so that bringing it back is a setting, not a rewrite.
//!
//! # Why Pocket TTS and not the 1.6B model epic 06 was comparing
//!
//! Epic 06 left the TTS between Magpie and `kyutai/tts-1.6b-en_fr`, both of
//! which need a GPU and a server you build yourself. Pocket TTS, published
//! since, removes every one of those costs — and the figures are its own
//! README's, relevé le 2026-10-28 :
//!
//! | | `tts-1.6b-en_fr` | Pocket TTS |
//! |---|---|---|
//! | Parameters | 1.6 B | ~100 M |
//! | Runs on | GPU, via `moshi-server` | **CPU**, 4 threads |
//! | First chunk | ~0.5 s | **~200 ms** |
//! | French | declared | declared, with `estelle` as its own voice |
//! | Weights | CC-BY-4.0 | CC-BY-4.0 |
//!
//! The VRAM budget of epic 08 wanted ~8 Go left for a local LLM. This
//! costs **none of it**, which is the argument that actually decided it: the
//! TTS stops competing with the brain.
//!
//!
//! # A library, where decision 50 said a subprocess — decision 56
//!
//! This module used to be `tts/pocket.rs` and drove `pocket-tts.exe serve`
//! over port 7422. Both of the reasons it did are gone, and one of them was
//! never a reason at all:
//!
//! | Motif de la décision 50 | État au 2026-10-29 |
//! |---|---|
//! | « son manifeste épingle un fork git de `xn-ptts` par commit » | le fork est passé sur `ptts = "0.5"` de crates.io ; plus aucune source `git+` dans son `Cargo.lock` |
//! | « sa compilation exige CMake », pour SentencePiece | feature optionnelle, **coupée** par `default-features = false` — avec axum, clap et tokio au passage |
//!
//! What that buys, and the third line is the one that mattered:
//!
//! | | Sous-processus | Lié |
//! |---|---|---|
//! | Un enfant à surveiller | oui, 505 Mo | non |
//! | Un port sur la boucle locale | 7422 | aucun |
//! | **Un orphelin après fermeture** | **oui, mesuré** | impossible |
//! | Un binaire de 22 Mo à télécharger à la main | oui | non |
//! | Un aller-retour HTTP par phrase | oui | non |
//! | Synthèse | une phrase d'un bloc | **en flux** |
//!
//! L'orphelin n'était pas un cas limite : relevé le 2026-10-29, fermer la
//! fenêtre laissait `pocket-tts.exe` en vie avec 505 Mo et le port 7422, parce
//! que `Drop` ne tourne pas pour la pile d'un fil quand le processus se
//! termine. La parade nommée était un Job Object Windows ; elle n'est plus à
//! écrire, puisqu'il n'y a plus d'enfant.
//!
//! # Ce que le fork ajoute, et rien d'autre
//!
//! Le crate de Kyutai ne publiait qu'un `[[bin]]` : tout était déjà écrit et
//! tout était privé. `Coopaguard/pocket-tts` ajoute une cible bibliothèque,
//! deux features (`cli`, `sentencepiece`) pour ce qui empêchait de lier, et un
//! correctif de cache — leur `download.rs` affirmait lire `HF_TOKEN` et
//! `HF_HOME`, ce qui était vrai de `hf-hub` 0.x et faux de la 1.0 qu'il
//! utilise. D'où [`set_cache`] ici. Le détail est dans
//! `docs/todos/pocket-tts-official.md`.
//!
//! # Ce qui n'est **pas** passé par `ptts` en direct
//!
//! La décision 54 l'avait mesuré et sa conclusion tient : `ptts` seul ne fait
//! pas parler ce checkpoint. Ce crate-ci est précisément la couche qui manquait
//! — il remappe `flow_lm.bos_before_voice` vers `fuser.padding_value` et sait
//! relire les états primés que Kyutai publie comme « embeddings ». C'est lui
//! qu'on lie, pas celui d'en dessous. [`super::phonon`] reste l'autre moteur
//! en processus, avec ses propres modèles.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use pocket_tts::config::Config;
use pocket_tts::model::{LoadOptions, TtsModel, VoiceState};

use super::{Utterance, Voice};
/// One of the voices the released weights carry an embedding for.
///
/// Relevé le 2026-10-28 from `rust/src/defaults.rs` of the Pocket TTS
/// repository, cross-checked against the files actually published under
/// `kyutai/pocket-tts-without-voice-cloning/languages/french/embeddings/` —
/// all twenty-seven exist for French, which is not something the catalogue
/// itself promises.
#[derive(Clone, Copy, Debug)]
pub struct Catalogued {
    /// What the model is asked for. A bare name resolves to the precomputed
    /// state for the loaded language, which is a small file rather than a
    /// voice to clone.
    pub id: &'static str,
    /// Whether the recording behind it allows commercial use.
    ///
    /// The voices come from several corpora under different licences, and
    /// nothing in the file name says which: a voice picked by ear would
    /// otherwise bring its licence in silence.
    /// Relevé le 2026-10-28 on `kyutai/tts-voices`: Expresso and EARS are
    /// CC-BY-**NC**, VCTK and CML-TTS are CC-BY-4.0, the voice donations and
    /// Kyutai's own recordings are CC0.
    pub commercial: bool,
}

/// Every voice the released weights can speak with.
pub const VOICES: &[Catalogued] = &[
    // Kyutai's own recordings, CC0 — including the French default.
    Catalogued {
        id: "estelle",
        commercial: true,
    },
    // Voice donations, CC0.
    Catalogued {
        id: "marius",
        commercial: true,
    },
    Catalogued {
        id: "javert",
        commercial: true,
    },
    // alba-mackenna, CC-BY-4.0.
    Catalogued {
        id: "alba",
        commercial: true,
    },
    // VCTK, CC-BY-4.0.
    Catalogued {
        id: "anna",
        commercial: true,
    },
    Catalogued {
        id: "vera",
        commercial: true,
    },
    Catalogued {
        id: "fantine",
        commercial: true,
    },
    Catalogued {
        id: "charles",
        commercial: true,
    },
    Catalogued {
        id: "paul",
        commercial: true,
    },
    Catalogued {
        id: "eponine",
        commercial: true,
    },
    Catalogued {
        id: "azelma",
        commercial: true,
    },
    Catalogued {
        id: "george",
        commercial: true,
    },
    Catalogued {
        id: "mary",
        commercial: true,
    },
    Catalogued {
        id: "jane",
        commercial: true,
    },
    Catalogued {
        id: "michael",
        commercial: true,
    },
    Catalogued {
        id: "eve",
        commercial: true,
    },
    // voice-zero, CC0.
    Catalogued {
        id: "bill_boerst",
        commercial: true,
    },
    Catalogued {
        id: "peter_yearsley",
        commercial: true,
    },
    Catalogued {
        id: "stuart_bell",
        commercial: true,
    },
    Catalogued {
        id: "caro_davy",
        commercial: true,
    },
    // The other languages' defaults, which French can still speak with.
    Catalogued {
        id: "giovanni",
        commercial: true,
    },
    Catalogued {
        id: "lola",
        commercial: true,
    },
    Catalogued {
        id: "juergen",
        commercial: true,
    },
    Catalogued {
        id: "rafael",
        commercial: true,
    },
    Catalogued {
        id: "daan",
        commercial: true,
    },
    // EARS: non-commercial. Offered, flagged, never the default. `cosette`
    // (Expresso, NC too) was removed on 2026-11-22 at the user's request.
    Catalogued {
        id: "jean",
        commercial: false,
    },
];

/// The voice the language is spoken with when none is chosen.
///
/// Kyutai's own mapping, and `estelle` is the one that matters here: a French
/// voice from Kyutai's own recordings, which `kyutai/tts-voices` releases as
/// CC0. The default therefore satisfies the commercial-use condition without
/// anyone having to check.
pub fn default_voice(language: &str) -> &'static str {
    match language {
        l if l.starts_with("french") => "estelle",
        l if l.starts_with("italian") => "giovanni",
        l if l.starts_with("spanish") => "lola",
        l if l.starts_with("german") => "juergen",
        l if l.starts_with("portuguese") => "rafael",
        l if l.starts_with("dutch") => "daan",
        _ => "alba",
    }
}

/// The model name for a language tag.
///
/// The settings file stores `fr`, because that is what a voice is picked by
/// everywhere else; Pocket TTS names its models `french`, `english` and so on.
/// One translation, in one place, rather than a second language field.
pub fn model_for(tag: &str) -> &'static str {
    match tag.to_lowercase().split(['-', '_']).next().unwrap_or("") {
        "fr" => "french",
        "de" => "german",
        "it" => "italian",
        "es" => "spanish",
        "pt" => "portuguese",
        "nl" => "dutch",
        // English is the model's own default, and the one every unknown tag
        // falls back to — a wrong accent beats a refusal to start.
        _ => "english",
    }
}

/// Where the weights land — and ARIA has to decide it, which was not obvious.
///
/// `hf-hub` 1.0 **reads no environment variables.** Its own README is explicit:
/// `HFClientSync::new()` talks to the public endpoint unauthenticated and
/// caches in `.cache/huggingface/hub` **relative to the current working
/// directory**. The comment in Pocket TTS's own `download.rs` said the client
/// reads `HF_TOKEN`, `HF_ENDPOINT` and `HF_HOME`; that was true of `hf-hub` 0.x
/// and is not true of the 1.0 it depends on. Verified on 2026-10-28 against
/// both, and corrected in the fork.
///
/// While there was a subprocess, this was survivable by starting it from the
/// right directory. **Linking removes that escape**: the current directory is
/// now ARIA's own, which in development is the repository — so four hundred
/// megabytes of weights would land in a git working copy that does not ignore
/// them. [`set_cache`] is the replacement, and the fork gained
/// `download::set_cache_dir` so that it exists to call.
///
/// The directory is unchanged from the subprocess era on purpose: switching to
/// the library must not re-download what is already there.
///
/// `ARIA_POCKET_CACHE` overrides it, for a machine where the weights belong on
/// another disk.
pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ARIA_POCKET_CACHE") {
        return PathBuf::from(dir);
    }
    crate::models_root().join("pocket-tts")
}

/// The Hub cache inside [`cache_dir`], in `hf-hub`'s own layout.
///
/// `.cache/huggingface/hub` under it, which is where the subprocess put its
/// files when it was started from that directory — so the same files are found
/// rather than fetched a second time.
fn hub_dir() -> PathBuf {
    cache_dir().join(".cache").join("huggingface").join("hub")
}

/// Tell the library where its cache is, once for the process.
///
/// Called before every load, and the first call is the one that counts: the
/// fork's `set_cache_dir` is an `OnceLock`, so two concurrent loaders cannot
/// leave it half-way between two directories.
fn set_cache() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| pocket_tts::download::set_cache_dir(hub_dir()));
}

/// Does the cache already hold weights?
///
/// A **probe of someone else's cache layout**, and it is worth saying so: the
/// shape below — `models--<owner>--<name>/snapshots/<sha>/…` — is `hf-hub`'s,
/// not ours, and it may change. It is only ever used to decide what a button
/// says and whether a download is announced, so being wrong costs a
/// misleading label and never a wrong result: the real answer comes from
/// loading the model, which downloads whatever is missing.
///
/// Deliberately not a list of expected files. Which ones they are is Pocket
/// TTS's own resolution — one of two repositories depending on whether the
/// gated one answers, a revision pinned per language, int8 variants under
/// quantisation — and a copy of that list here would be a second source of
/// truth that breaks silently the day they bump a revision.
pub fn weights_cached() -> bool {
    cached_under(&cache_dir())
}

/// The same question against an explicit root, so it is testable without
/// touching the environment.
fn cached_under(root: &Path) -> bool {
    let hub = root.join(".cache").join("huggingface").join("hub");
    let Ok(entries) = std::fs::read_dir(&hub) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .starts_with("models--kyutai--pocket-tts")
            && holds_a_model(&entry.path().join("snapshots"))
    })
}

/// Any `.safetensors` under a snapshot, at any depth the languages use.
fn holds_a_model(snapshots: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(snapshots) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|snapshot| {
        walkdir(&snapshot.path(), 4).iter().any(|file| {
            file.extension()
                .is_some_and(|extension| extension == "safetensors")
        })
    })
}

/// A bounded walk. Bounded because this runs when a settings screen opens, and
/// an unbounded descent into a cache someone pointed at their home directory
/// would be a frozen window.
fn walkdir(at: &Path, depth: usize) -> Vec<PathBuf> {
    if depth == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(at) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walkdir(&path, depth - 1));
        } else {
            found.push(path);
        }
    }
    found
}

/// What a voice is opened with.
pub struct Setup {
    /// The model's own name: `french`, not `fr`. See [`model_for`].
    pub language: String,
    /// A catalogued voice name, a path, or empty for the language's default.
    pub voice: String,
    /// int8 weights: less memory, and the only knob that changes the sound.
    pub quantize: bool,
}

impl Setup {
    /// The voice that will actually be used, defaults resolved.
    pub fn resolved_voice(&self) -> String {
        if self.voice.trim().is_empty() {
            default_voice(&self.language).to_string()
        } else {
            self.voice.trim().to_string()
        }
    }
}

/// The weights, on one of the two backends.
///
/// An enum rather than a type parameter, because the choice comes from a
/// setting read at run time: making this module generic so a configuration
/// boolean could travel through it would be promoting a run-time decision into
/// the type system.
enum Model {
    F32(TtsModel<pocket_tts::F32>),
    Int8(TtsModel<pocket_tts::Int8>),
}

/// A voice prompt, already run through the model it belongs to.
///
/// The expensive part of a voice is the model; this is the cheap part — a small
/// `.safetensors` of primed attention caches, imported in milliseconds once
/// the weights are warm. Which is why the two are separate here, and why the
/// sub-menu of twenty-seven voices became usable the moment they were.
enum Prompt {
    F32(VoiceState<pocket_tts::F32>),
    Int8(VoiceState<pocket_tts::Int8>),
}

impl Model {
    fn sample_rate(&self) -> u32 {
        match self {
            Self::F32(model) => model.sample_rate() as u32,
            Self::Int8(model) => model.sample_rate() as u32,
        }
    }

    fn prompt(&self, voice: &str) -> Result<Prompt> {
        // `truncate` only ever trims an audio file to its first thirty
        // seconds; a catalogued name resolves to a precomputed state, so it
        // decides nothing here.
        match self {
            Self::F32(model) => Ok(Prompt::F32(
                model
                    .get_state_for_audio_prompt(voice, false)
                    .map_err(|e| anyhow!("la voix {voice} ne charge pas : {e:#}"))?,
            )),
            Self::Int8(model) => Ok(Prompt::Int8(
                model
                    .get_state_for_audio_prompt(voice, false)
                    .map_err(|e| anyhow!("la voix {voice} ne charge pas : {e:#}"))?,
            )),
        }
    }

    fn speak(
        &self,
        prompt: &Prompt,
        text: &str,
        on_chunk: &mut (dyn FnMut(&[f32]) -> Result<()> + Send),
    ) -> Result<()> {
        // Never set: stopping mid-sentence is done by returning an error from
        // `on_chunk`, which is what `Player::say` already does when the queue
        // has been cut. A second mechanism for the same thing would be a
        // second place to keep in agreement with decision 17.
        let stop = AtomicBool::new(false);
        let max = pocket_tts::defaults::MAX_TOKEN_PER_CHUNK;
        match (self, prompt) {
            (Self::F32(model), Prompt::F32(voice)) => {
                model.generate_audio_stream(voice, text, max, None, &stop, on_chunk)
            }
            (Self::Int8(model), Prompt::Int8(voice)) => {
                model.generate_audio_stream(voice, text, max, None, &stop, on_chunk)
            }
            // Unreachable by construction — a prompt is only ever made by the
            // model it is used with — and an error rather than a panic because
            // the alternative would be a wrong voice played confidently.
            _ => Err(anyhow!("voix et modèle de backends différents")),
        }
    }
}

/// The models read so far, one per language and precision.
///
/// **The thing that keeps the one-copy property the subprocess had for free.**
/// A server was a single five-hundred-megabyte process that the settings
/// screen and the speech thread both talked to. Loading the library twice —
/// once to speak, once to audition — would put two copies in ARIA's own
/// address space, which is strictly worse than what it replaced. So the
/// weights are held here and handed out as an [`Arc`], and a voice is only ever
/// the small prompt on top.
///
/// The lock is held **across the load**, deliberately: a second caller waiting
/// a few seconds is the point, where letting it through would read four
/// hundred megabytes to produce a duplicate. Nothing is ever evicted, for the
/// same reason the server was never stopped — and the orb's bar, decision 51,
/// is what makes the first wait visible.
static MODELS: LazyLock<Mutex<Resident>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keyed by language and precision, which are exactly the two things that make
/// one set of weights a different set of weights.
type Resident = HashMap<(String, bool), Arc<Model>>;

/// Say so, once, when this build cannot keep up with its own speech.
///
/// # Why a build choice has to announce itself
///
/// `xn` picks its SIMD kernels **at compile time** — neither it nor `xn-gemm`
/// calls `is_x86_feature_detected`, checked on 2026-10-30 — so a build made
/// without `-C target-cpu=x86-64-v3` runs the inference on SSE2 and
/// synthesises at 0.49× real time. Below 1.0 the voice cannot keep up with its
/// own playback and the speech comes out in pieces, whatever the sentence
/// splitting does.
///
/// That is invisible from the outside: same binary, same model, same settings
/// screen, and a symptom — "the voice stutters" — that sounds like a bug in
/// the audio queue. It cost an evening once, looking at the queue. So the
/// build says which one it is, at the only moment anyone is reading the log.
///
/// Decision 63 carries the measurement and the hardware floor it costs.
fn warn_if_slow_kernels() {
    static SAID: OnceLock<()> = OnceLock::new();
    SAID.get_or_init(|| {
        if !cfg!(target_feature = "avx2") {
            tracing::warn!(
                "cette construction n'a pas AVX2 : la synthèse tournera à environ 0,49× le \
                 temps réel, donc la voix sera hachée. C'est un choix de compilation — voir \
                 `.cargo/config.toml` et la décision 63 — pas un défaut de la file audio."
            );
        }
    });
}

/// The model for a language and precision, loaded if this is the first ask.
fn model_in_memory(language: &str, quantize: bool) -> Result<Arc<Model>> {
    set_cache();
    warn_if_slow_kernels();
    let key = (language.to_string(), quantize);
    let mut models = MODELS.lock().expect("models poisoned");
    if let Some(model) = models.get(&key) {
        return Ok(Arc::clone(model));
    }

    // The language YAMLs are embedded in the crate, so there is nothing to
    // fetch for the configuration — unlike the `config.json` that had to be
    // hand-written for the spike of decision 54.
    let (config, origin) = Config::load(Some(language), None)
        .with_context(|| format!("config de langue {language} illisible"))?;
    let options = LoadOptions::default();
    let started = std::time::Instant::now();
    let model = if quantize {
        Model::Int8(
            TtsModel::<pocket_tts::Int8>::load(config, origin, &options)
                .map_err(|e| anyhow!("le modèle Kyutai ne charge pas : {e:#}"))?,
        )
    } else {
        Model::F32(
            TtsModel::<pocket_tts::F32>::load(config, origin, &options)
                .map_err(|e| anyhow!("le modèle Kyutai ne charge pas : {e:#}"))?,
        )
    };
    tracing::info!(
        language,
        quantize,
        rate = model.sample_rate(),
        ms = started.elapsed().as_millis(),
        "modèle Kyutai en mémoire, dans le processus"
    );

    let model = Arc::new(model);
    models.insert(key, Arc::clone(&model));
    Ok(model)
}

/// How many models are resident. For the log and for the test below.
pub fn models_in_memory() -> usize {
    MODELS.lock().expect("models poisoned").len()
}

/// A voice that synthesises in this process.
pub struct KyutaiVoice {
    model: Arc<Model>,
    /// `generate_audio_stream` takes the prompt by reference, so this could be
    /// plain — the lock is here to serialise **one model against itself**. The
    /// crate's own note is that batch size is one and it is not meant to be
    /// shared between threads; now that the weights are shared, the preview
    /// and the speech thread can reach the same instance, and this is what
    /// keeps them from doing it at once.
    prompt: Mutex<Prompt>,
    rate: u32,
    label: String,
}

impl KyutaiVoice {
    /// Load what is needed to speak, reusing whatever is already resident.
    ///
    /// Seconds the first time for a language, the download included; a few
    /// milliseconds afterwards, because only the voice prompt is new. The
    /// caller registers the wait as `Load::Voice`, which is what puts it on
    /// the orb.
    pub fn open(setup: &Setup) -> Result<Self> {
        let voice = setup.resolved_voice();
        let model = model_in_memory(&setup.language, setup.quantize)?;
        let prompt = model.prompt(&voice)?;
        Ok(Self {
            rate: model.sample_rate(),
            label: format!("Kyutai {} · {voice}", setup.language),
            model,
            prompt: Mutex::new(prompt),
        })
    }
}

impl Voice for KyutaiVoice {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn speak(&self, text: &str) -> Result<Utterance> {
        let mut samples = Vec::new();
        self.stream(text, &mut |utterance: Utterance| {
            samples.extend_from_slice(&utterance.samples);
            Ok(())
        })?;
        Ok(Utterance {
            samples,
            sample_rate: self.rate,
        })
    }

    /// The reason this module exists at all: each piece goes to the speakers
    /// as soon as it exists, instead of the sentence going there in one block
    /// once it is finished.
    ///
    /// The chunks arrive on the library's decoder thread rather than this one,
    /// which is why [`Voice::stream`] asks for a `Send` callback.
    fn stream(
        &self,
        text: &str,
        on_chunk: &mut (dyn FnMut(Utterance) -> Result<()> + Send),
    ) -> Result<()> {
        let prompt = self.prompt.lock().expect("voice poisoned");
        let rate = self.rate;
        // The one adaptation: the library hands over bare samples, the trait
        // hands over an utterance that carries its own rate. Still `Send`,
        // because `&mut (dyn … + Send)` is.
        let mut pass = |pcm: &[f32]| -> Result<()> {
            on_chunk(Utterance {
                samples: pcm.to_vec(),
                sample_rate: rate,
            })
        };
        self.model.speak(&prompt, text, &mut pass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe is of someone else's cache layout, so what it recognises is
    /// worth pinning: the day `hf-hub` changes shape, this test fails rather
    /// than a button quietly lying about a 400 MB download.
    mod cache {
        use super::*;

        fn hub(root: &Path) -> PathBuf {
            root.join(".cache").join("huggingface").join("hub")
        }

        #[test]
        fn an_empty_cache_holds_nothing() {
            let root = std::env::temp_dir().join("aria-kyutai-empty");
            let _ = std::fs::remove_dir_all(&root);
            assert!(!cached_under(&root), "a directory that is not there");
            std::fs::create_dir_all(hub(&root)).expect("the hub can be made");
            assert!(!cached_under(&root), "a hub with no repository in it");
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn a_snapshot_with_weights_counts() {
            let root = std::env::temp_dir().join("aria-kyutai-full");
            let _ = std::fs::remove_dir_all(&root);
            let snapshot = hub(&root)
                .join("models--kyutai--pocket-tts-without-voice-cloning")
                .join("snapshots")
                .join("1e08e6a2")
                .join("languages")
                .join("french");
            std::fs::create_dir_all(&snapshot).expect("the snapshot can be made");
            // A repository directory with no weights in it is the half-finished
            // case: `hf-hub` creates the tree before the bytes arrive, so
            // recognising the directory alone would call an interrupted
            // download a model.
            assert!(!cached_under(&root), "the tree exists, the weights do not");
            std::fs::write(snapshot.join("model.safetensors"), b"not really").expect("written");
            assert!(cached_under(&root));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn another_models_cache_is_not_ours() {
            let root = std::env::temp_dir().join("aria-kyutai-other");
            let _ = std::fs::remove_dir_all(&root);
            let other = hub(&root)
                .join("models--openai-community--gpt2")
                .join("snapshots")
                .join("abc");
            std::fs::create_dir_all(&other).expect("made");
            std::fs::write(other.join("model.safetensors"), b"x").expect("written");
            assert!(
                !cached_under(&root),
                "a cache shared with another model must not count as ours"
            );
            let _ = std::fs::remove_dir_all(&root);
        }

        /// The override exists for a machine where 400 MB belong on another
        /// disk. Checked by its absence being the documented default, since a
        /// test that sets the variable would race every other test in the
        /// process.
        #[test]
        fn the_default_cache_is_under_arias_own_models() {
            if std::env::var_os("ARIA_POCKET_CACHE").is_some() {
                return;
            }
            let dir = cache_dir();
            assert!(dir.ends_with(Path::new("aria").join("models").join("pocket-tts")));
        }

        /// The one thing the switch to a library could have broken silently:
        /// weights already downloaded by the subprocess must be found, not
        /// fetched again. The subprocess was started **in** `cache_dir`, and
        /// `hf-hub` cached relative to it — so the directory the library is
        /// now told about has to be exactly that one.
        #[test]
        fn the_library_is_pointed_at_the_directory_the_subprocess_filled() {
            assert!(hub_dir().starts_with(cache_dir()));
            assert_eq!(
                hub_dir(),
                cache_dir().join(".cache").join("huggingface").join("hub"),
                "the layout `hf-hub` creates under the working directory"
            );
        }
    }

    #[test]
    fn a_language_tag_becomes_a_model_name() {
        assert_eq!(model_for("fr"), "french");
        assert_eq!(model_for("fr-FR"), "french");
        assert_eq!(model_for("FR_fr"), "french");
        // The fallback is the model's own default, not an error: a wrong
        // accent beats refusing to speak.
        assert_eq!(model_for("sv"), "english");
        assert_eq!(model_for(""), "english");
    }

    /// The default of every language is the one voice nobody chose
    /// deliberately, so it carries no restriction.
    #[test]
    fn every_default_voice_allows_commercial_use() {
        for language in [
            "french",
            "english",
            "italian",
            "spanish",
            "german",
            "portuguese",
            "dutch",
        ] {
            let id = default_voice(language);
            let found = VOICES
                .iter()
                .find(|v| v.id == id)
                .unwrap_or_else(|| panic!("{language} defaults to {id}, which is not catalogued"));
            assert!(
                found.commercial,
                "{language} defaults to {id}, which is non-commercial"
            );
        }
    }

    /// One voice is not usable in a product, and the point of the flag is
    /// that the screen can say so. A catalogue where everything is allowed
    /// would mean the licences were never actually looked up.
    #[test]
    fn the_non_commercial_voices_are_marked() {
        let flagged: Vec<&str> = VOICES
            .iter()
            .filter(|v| !v.commercial)
            .map(|v| v.id)
            .collect();
        assert_eq!(flagged, vec!["jean"]);
    }

    #[test]
    fn an_empty_voice_setting_resolves_to_the_language_default() {
        let setup = Setup {
            language: "french".into(),
            voice: "  ".into(),
            quantize: false,
        };
        assert_eq!(setup.resolved_voice(), "estelle");
        let chosen = Setup {
            language: "french".into(),
            voice: " marius ".into(),
            quantize: false,
        };
        assert_eq!(chosen.resolved_voice(), "marius");
    }

    /// Nothing is resident until something asks, which is what keeps opening
    /// the settings screen from reading four hundred megabytes.
    #[test]
    fn no_model_is_held_before_one_is_asked_for() {
        assert_eq!(models_in_memory(), 0);
    }
}

#[cfg(test)]
mod speed {
    use super::*;
    use crate::tts::Voice;
    use std::time::Instant;

    /// How fast the engine produces speech, against the clock it has to beat.
    ///
    /// The number that matters is **audio seconds per wall second**. Below 1.0
    /// the voice cannot keep up with its own playback, and no amount of
    /// chunking fixes that: the queue empties mid-sentence whatever the pieces
    /// are. Measured in the app on 2026-10-29 at 0.40, which is what sent us
    /// looking.
    ///
    /// `#[ignore]` because it loads ~400 MB of weights and speaks for several
    /// seconds.
    ///
    /// ```sh
    /// cargo test -p aria-voice --lib speed -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "loads the Kyutai weights and synthesises for several seconds"]
    fn the_engine_is_timed_against_real_time() {
        for quantize in [false, true] {
            let setup = Setup {
                language: model_for("fr").to_string(),
                voice: String::new(),
                quantize,
            };
            let at = Instant::now();
            let voice = match KyutaiVoice::open(&setup) {
                Ok(voice) => voice,
                Err(e) => {
                    println!("int8={quantize} : non chargeable ({e:#})");
                    continue;
                }
            };
            let loaded = at.elapsed();

            let text = "Demain, ciel couvert sur Lyon, douze degrés au plus chaud, \
                        averses en fin d'après-midi.";
            let mut samples = 0usize;
            let mut rate = 0u32;
            let mut first: Option<std::time::Duration> = None;
            let at = Instant::now();
            voice
                .stream(text, &mut |utterance| {
                    if first.is_none() {
                        first = Some(at.elapsed());
                    }
                    samples += utterance.samples.len();
                    rate = utterance.sample_rate;
                    Ok(())
                })
                .expect("la synthèse aboutit");
            let took = at.elapsed();

            let audio = samples as f32 / rate.max(1) as f32;
            println!(
                "int8={quantize} : chargement {:.1} s · premier échantillon {} ms · \
                 {audio:.2} s d'audio en {:.2} s = {:.2}× le temps réel",
                loaded.as_secs_f32(),
                first.unwrap_or_default().as_millis(),
                took.as_secs_f32(),
                audio / took.as_secs_f32()
            );
        }
    }
}
