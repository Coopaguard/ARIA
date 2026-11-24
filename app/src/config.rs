//! The single configuration file, and the defaults behind it.
//!
//! Holds the default permission profile (overridable per session), the
//! global shortcut, and the toggle that keeps the core resident in the
//! background. Sensitive parts stay out of the repository.
//! See the original specification.
//!
//! # Two things this settles, that were open until the settings screen needed them
//!
//! **TOML, not the YAML sketched in the original specification.** In Rust, YAML costs a
//! dependency whose maintained successor is not obvious, where TOML is native,
//! is what Cargo itself reads, and round-trips without surprises. The sketch was
//! a sketch; this is the cost of honouring it.
//!
//! **English keys.** Decision 12 puts code in English and what the user reads in
//! French, and a hand-edited file sits across that line. The argument for
//! English: the French surface is the **settings screen**, which now exists —
//! nobody has to open this file to change a setting, so it is closer to code
//! than to interface. Both are recorded in the decision log (decision 24)
//! and both are one `#[serde(rename)]` away from being reversed.
//!
//! # Where it lives
//!
//! `%APPDATA%/dev.coopaguard.aria/aria.toml` on Windows — Tauri's own app
//! config directory, so it follows the identifier rather than the install path.
//! Out of the repository, which is what the original specification asks for.

use std::collections::BTreeMap;
use std::path::PathBuf;

use aria_core::permissions::Profile;
use aria_voice::calibration::Calibration;
use aria_voice::name::Name;
use serde::{Deserialize, Serialize};
use tauri::Manager;

/// Everything the application reads at start-up.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// The default permission profile. A default: a session may be launched
    /// under another one, and each session carries the one it runs under.
    pub profile: Profile,

    /// Keep the core running with every window closed.
    ///
    /// Unticked, closing the window **stops ARIA**: no more global shortcut, no
    /// more cadences, running sessions interrupted. Epic 04 asks for that to be
    /// said explicitly on screen, because it is the one setting that silently
    /// turns a background service into an ordinary application.
    pub stay_in_background: bool,

    /// Unload the audio models when going to sleep.
    ///
    /// Ticked frees the memory but waking costs the reload — about 3.8 s,
    /// measured in T1. Unticked wakes instantly but the resting footprint goes
    /// past the 300 MB target of the original specification.
    pub unload_audio_on_sleep: bool,

    /// Compute on the graphics card. On by default; off leaves the card to
    /// whatever else wants it, a game for one, at the price of speed.
    ///
    /// It moves two things, added on 2026-11-18:
    ///
    /// - **the local model** (epic 10): off passes `--device none` to
    ///   `llama-server`, which otherwise offloads every layer it can — it
    ///   already did before this field existed, Vulkan on the winget build;
    /// - **the microphone's transcription**: Parakeet on DirectML, through
    ///   `aria_voice::asr::set_gpu`. Smart Turn stays on the processor, its
    ///   model is the `-cpu` build.
    ///
    /// Pocket-tts is **not** covered: `xn` picks its backend at compile time
    /// and the fork binds the model to the CPU. See epic 10.
    pub gpu: bool,

    /// What ARIA answers to, and the spellings the ASR really produces for it.
    ///
    /// The aliases are learned by having the name said three times during
    /// setup: it turns an unsolvable problem — make an ASR spell an arbitrary
    /// name — into a trivial one, knowing how *this* ASR spells it.
    pub name: Name,

    /// The wake-word detector: its model, threshold, `k`, and whether the
    /// transcript must confirm it. See `aria_voice::wake`. Read at start-up.
    pub wake: aria_voice::wake::Settings,

    /// The voice chain's thresholds, **per input device**.
    ///
    /// Keyed by the device identifier and never global, decision 38: a
    /// threshold measured on a headset is wrong on laptop speakers, and a chain
    /// that silently degrades when a headset is plugged in is a fault nobody
    /// ever diagnoses. A `BTreeMap` so the file keeps a stable order and a diff
    /// stays readable.
    pub calibration: BTreeMap<String, Calibration>,

    /// The configurable global shortcut.
    ///
    /// **Not `Win+C`.** Windows 11 reassigns it to Copilot — withdrawn in June
    /// 2024, reinstated in 2025 with a system setting of its own. The default
    /// below avoids the Windows key entirely.
    pub global_shortcut: String,

    /// The main agent: which engine, which model, and what it is told it is.
    pub agent: Agent,

    /// The engine delegated sessions run on, and its own settings.
    pub delegation: Delegation,

    /// What ARIA sounds like.
    pub voice: Voice,

    /// The first-run screen has been through (T12).
    ///
    /// `true` when the field is absent from an existing file: someone who has
    /// used ARIA before is not walked through it again. A fresh install has no
    /// file, gets [`Config::default`], and therefore `false`.
    #[serde(default = "already")]
    pub welcomed: bool,
}

fn already() -> bool {
    true
}

/// The post that holds the conversation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Agent {
    /// Which engine holds the conversation.
    ///
    /// An id from `aria_core::providers::engine::choices(Post::Voice)`: Gemma 4
    /// E4B by default, Voxtral, or Claude Code as the backup (epic 10).
    ///
    /// An unknown id falls back to the default with a line in the log: this
    /// file is hand-editable, and a typo must cost a setting rather than the
    /// assistant.
    pub engine: String,

    /// The binary, or a path to it.
    ///
    /// A setting and not only an environment variable, because "the CLI is not
    /// on PATH" is the commonest way this product fails to start and
    /// `ARIA_CLAUDE_BIN` is not something a user is going to discover.
    pub command: String,

    /// The model, **in full**.
    ///
    /// Never an alias: `docs/cli/claude-code-cli.md` §8 measured `--model haiku` resolving to a
    /// generation behind what was asked for, silently. And a setting rather
    /// than a constant because the original specification is explicit — model identifiers
    /// live in configuration.
    pub model: String,

    /// What the user adds to what ARIA is told it is.
    ///
    /// **Added, never replacing.** The shipped prompt carries the rules that
    /// make the mechanism work — repeat a payload verbatim, no Markdown
    /// because it is spoken, say nothing after delegating — and a field that
    /// let someone overwrite them would be a field that breaks the product
    /// invisibly. What belongs here is a persona: how ARIA addresses the user,
    /// what it knows about them, what it should never do.
    pub persona: String,

    /// How a local main agent is run, when [`Self::engine`] names one —
    /// epic 10. Ignored for the CLI.
    pub local: LocalAgent,
}

/// The `llama-server` behind a local main agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LocalAgent {
    /// The binary. Empty: `ARIA_LLAMA_SERVER`, the `PATH`, then winget's
    /// package folder — see `aria_core::agent::local::sidecar::find`.
    pub server: String,
    /// Passed to `llama-server` after ARIA's own flags — `-ngl 99`, `-t 8`.
    /// The right values are the machine's, which is why they are a setting.
    pub args: Vec<String>,
    /// Skip the transcript written after each heard answer. Off by default:
    /// without it the model's history only holds « (message vocal) » for
    /// what the user said, and « et après-demain ? » has nothing to lean on.
    pub no_transcript: bool,
}

impl Default for LocalAgent {
    fn default() -> Self {
        Self {
            server: String::new(),
            args: Vec::new(),
            no_transcript: false,
        }
    }
}

impl LocalAgent {
    /// What `llama-server` receives after ARIA's own flags: the device, then
    /// the user's arguments — last, so a hand-written `--device` still wins.
    /// `gpu` is [`Config::gpu`].
    pub fn server_args(&self, gpu: bool) -> Vec<String> {
        let device = (!gpu).then(|| ["--device".to_string(), "none".to_string()]);
        device.into_iter().flatten().chain(self.args.iter().cloned()).collect()
    }
}

/// Which synthesis engine speaks.
///
/// A setting rather than a build choice, and the original specification asked for
/// exactly this before either engine existed: *les identifiants de modèles
/// vivent en configuration, jamais en dur*.
///
/// # Phonon only, since 2026-11-19
///
/// Decided by the user. The Windows voice is **gone** — it was the default and
/// the fallback, and a file that still says `system` reads as Phonon through
/// the alias below. Kyutai is **suspended, not removed**: its code, its
/// settings and its screen rows stay, but [`Engine::in_force`] turns it into
/// Phonon at load time and the screen does not offer it. Bringing it back is
/// that method and `ENGINES` in `Settings.vue`.
///
/// There is no fallback any more: an engine that does not start leaves ARIA
/// answering in writing, and the log says why.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Kyutai Pocket TTS, **linked** and running on the CPU. Suspended — see
    /// above.
    ///
    /// ~100 M parameters, ~200 ms to the first chunk, CC-BY-4.0 weights, and
    /// no VRAM at all — which is what settled epic 06's open question: the TTS
    /// stops competing with a local LLM for the card. Driven as a subprocess
    /// until decision 56; now a library call, so there is no binary to install
    /// and no port.
    Kyutai,
    /// Phonon, loaded into **this** process through `ptts`.
    ///
    /// The only engine whose synthesis is a **stream**: one sentence arrives
    /// in about six pieces, the first in 67 to 141 ms, so audio starts before
    /// the sentence is finished. No port, no child, no orphan.
    ///
    /// Its catch is a licence, not a measurement: only its English model is
    /// open, every French one is CC-BY-NC and gated. See decision 55.
    #[default]
    #[serde(alias = "system")]
    Phonon,
}

impl Engine {
    /// The engine that actually speaks for this setting.
    ///
    /// Kyutai is suspended, so it resolves to Phonon. Applied at load time,
    /// which makes the screen, the speech thread and the next save agree.
    pub fn in_force(self) -> Self {
        match self {
            Engine::Kyutai => Engine::Phonon,
            other => other,
        }
    }
}

/// What ARIA sounds like.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Voice {
    /// Whether ARIA speaks at all. Unticked, it answers in writing.
    pub enabled: bool,

    /// Which engine does the speaking.
    pub engine: Engine,

    /// The language a voice is picked for when none is chosen.
    ///
    /// Shared by both engines on purpose: it is a fact about the user, not
    /// about an engine. Each translates it its own way — Phonon picks its
    /// voice catalogue by it, Pocket TTS names a model `french`.
    pub language: String,

    /// The Phonon side, kept separate for the same reason as the Kyutai one.
    pub phonon: Phonon,

    /// The Kyutai side, kept separate from [`Self::id`].
    ///
    /// Separate rather than one "voice" field for both, and that is not
    /// tidiness: the two name spaces have nothing in common, so a shared field
    /// would carry a Phonon identifier into Pocket TTS the moment someone
    /// switched engines to compare them. Each engine keeps what it was last
    /// set to, so switching back and forth costs nothing.
    pub kyutai: Kyutai,
}

/// How Pocket TTS should speak.
///
/// # Two fields fewer than it had, and that is the point of decision 56
///
/// It carried a `binary` path and a `port`, because the engine was a
/// subprocess: somebody had to download `pocket-tts.exe` by hand, and a port
/// can be taken. Linking the model deleted both questions rather than
/// answering them — there is no file to find and no port to argue over. A
/// settings file written before the switch still loads, `#[serde(default)]`
/// ignores the two stale keys, and nobody is asked to migrate anything.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Kyutai {
    /// A catalogued voice name, or empty for the language's own default —
    /// `estelle` in French, which is CC0.
    pub voice: String,

    /// int8 weights: less memory, slightly different sound.
    pub quantize: bool,
}

/// Which Phonon model, and which of its voices.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Phonon {
    /// A repository from `aria_voice::tts::phonon::MODELS`, or empty for the
    /// default — the English one, which is the only one with an open licence.
    pub model: String,

    /// A `voice_id` from the catalogue, or empty for the model's own voice.
    ///
    /// Both fields default to empty rather than to a name, so the engine
    /// resolves them itself — which means a settings file written before this
    /// engine existed picks up the open model rather than nothing.
    ///
    /// An identifier and not the name a person reads: `Noémie` is what the
    /// screen shows, `FXxJ9mANRq6BCTX5` is what survives the catalogue being
    /// reordered or relabelled upstream.
    pub voice: String,
}

/// The engine delegated sessions run on.
///
/// A section of its own since 2026-11-12, and that is the point of it: the two
/// posts are not the same job. The voice post is a dispatcher whose every word
/// is spoken, under a byte budget, never billed per token; a delegated session
/// is the one doing the work, and may be given reach the voice post never has
/// (decision 105). An engine good at one can be wrong for the other, so the
/// screen asks twice.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Delegation {
    /// An id from `aria_core::providers::engine::choices(Post::Delegation)`.
    pub engine: String,

    /// The binary for it, when that engine is a CLI.
    ///
    /// Separate from `agent.command` on purpose: the two posts may be two
    /// different programs, and sharing one field would make choosing a second
    /// engine for one post silently repoint the other.
    pub command: String,
    // No minimum version since 2026-11-19: the CLI is called, and a call that
    // fails says why. A `minimum_version` left in an old file is ignored.
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            // Claude Code again since 2026-11-19 (decision 127). Gemma was the
            // default for a day: faster and free, but too weak — it promised
            // tasks it never started. The local models stay a choice.
            engine: aria_core::providers::claude_code::ID.into(),
            command: std::env::var("ARIA_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()),
            model: aria_core::agent::session::MODEL.into(),
            persona: String::new(),
            local: LocalAgent::default(),
        }
    }
}

impl Default for Delegation {
    fn default() -> Self {
        Self {
            engine: aria_core::providers::claude_code::ID.into(),
            command: std::env::var("ARIA_CLAUDE_BIN")
                .unwrap_or_else(|_| aria_core::providers::claude_code::DEFAULT_COMMAND.into()),
        }
    }
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            enabled: true,
            language: "fr".into(),
            engine: Engine::default(),
            kyutai: Kyutai::default(),
            phonon: Phonon::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            profile: Profile::Normal,
            stay_in_background: true,
            unload_audio_on_sleep: true,
            gpu: true,
            global_shortcut: "Ctrl+Alt+C".into(),
            name: Name::default(),
            wake: aria_voice::wake::Settings::default(),
            calibration: BTreeMap::new(),
            agent: Agent::default(),
            delegation: Delegation::default(),
            voice: Voice::default(),
            welcomed: false,
        }
    }
}

impl Config {
    /// The calibration for one input device, or the shipped defaults.
    ///
    /// Falls back on two grounds, and both matter. An **unknown** device has
    /// never been calibrated, which is the normal first run. An
    /// **implausible** stored value is refused outright — decision 38 says a
    /// calibration must never make the chain worse than the default, and that
    /// has to hold for a file someone hand-edited as much as for a wizard that
    /// went wrong.
    pub fn calibration_for(&self, device: &str) -> Calibration {
        match self.calibration.get(device) {
            Some(stored) if stored.is_plausible() => *stored,
            Some(_) => {
                tracing::warn!(device, "calibration hors des bornes, défauts appliqués");
                Calibration::default()
            }
            None => Calibration::default(),
        }
    }

    /// Where the file lives, creating the directory if it is not there.
    pub fn path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> anyhow::Result<PathBuf> {
        let dir = app.path().app_config_dir()?;
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join("aria.toml"))
    }

    /// Read the file, or hand back the defaults.
    ///
    /// A missing file is the normal first run, not an error. A **malformed**
    /// file is different and is logged loudly: silently resetting someone's
    /// settings because one line failed to parse is how a shortcut they
    /// configured stops working with no explanation.
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(mut config) => {
                    let engine = config.voice.engine.in_force();
                    if engine != config.voice.engine {
                        tracing::warn!(
                            "moteur de voix {:?} suspendu, {engine:?} à la place",
                            config.voice.engine
                        );
                        config.voice.engine = engine;
                    }
                    // Silencing ARIA is the orb's job, not a setting: with the
                    // switch gone from the screen, `false` here would leave
                    // her mute with nothing to undo it.
                    if !config.voice.enabled {
                        tracing::warn!("voix désactivée dans aria.toml : réactivée, l'orbe s'en charge");
                        config.voice.enabled = true;
                    }
                    // The name is no longer a setting: ARIA is called Aria.
                    // Spellings learned for another name are worthless, so a
                    // renamed config starts over rather than keeping them.
                    if config.name.canonical != Name::default().canonical {
                        tracing::warn!(
                            "nom {:?} ignoré : l'assistante s'appelle Aria",
                            config.name.canonical
                        );
                        config.name = Name::default();
                    }
                    // A voice taken out of the catalogue (`cosette`, on
                    // 2026-11-22) falls back on the language's default.
                    let kyutai = config.voice.kyutai.voice.trim();
                    if !kyutai.is_empty()
                        && !aria_voice::tts::kyutai::VOICES.iter().any(|v| v.id == kyutai)
                    {
                        tracing::warn!(voix = kyutai, "voix retirée du catalogue : voix par défaut");
                        config.voice.kyutai.voice.clear();
                    }
                    if !config.wake.is_plausible() {
                        tracing::warn!(wake = ?config.wake, "réglages du mot de réveil hors des bornes, défauts appliqués");
                        config.wake = aria_voice::wake::Settings::default();
                    }
                    config
                }
                Err(e) => {
                    tracing::error!(
                        "{} will not parse, falling back to defaults \
                         (the file is left untouched): {e}",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::error!("{} cannot be read: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Write the file.
    ///
    /// Through a temporary file and a rename, so an interrupted write leaves
    /// the old settings rather than half of the new ones. Cheap here, and the
    /// failure it avoids — an empty config after a crash — is the kind nobody
    /// connects back to its cause.
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(self)?;
        let temporary = path.with_extension("toml.new");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows voice is gone and Kyutai is suspended: a file written for
    /// either still loads, and speaks with Phonon.
    #[test]
    fn a_retired_or_suspended_engine_reads_as_phonon() {
        let system: Config = toml::from_str("[voice]\nengine = \"system\"\nid = \"x\"").unwrap();
        assert_eq!(system.voice.engine, Engine::Phonon);
        let kyutai: Config = toml::from_str("[voice]\nengine = \"kyutai\"").unwrap();
        assert_eq!(kyutai.voice.engine.in_force(), Engine::Phonon);
        assert_eq!(Config::default().voice.engine, Engine::Phonon);
    }

    #[test]
    fn the_defaults_round_trip_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_what_it_omits() {
        // `#[serde(default)]` on the struct: adding a setting must not make
        // every existing config file unreadable.
        let config: Config = toml::from_str("profile = \"strict\"").unwrap();
        assert_eq!(config.profile, Profile::Strict);
        assert_eq!(config.global_shortcut, Config::default().global_shortcut);
    }

    /// A file written before the GPU switch existed keeps the GPU: off would
    /// silently move the model to the CPU on the next start.
    #[test]
    fn an_older_file_keeps_the_gpu() {
        let config: Config = toml::from_str("[agent.local]\nargs = [\"-t\", \"8\"]").unwrap();
        assert!(config.gpu);
        assert_eq!(config.agent.local.server_args(config.gpu), ["-t", "8"]);
    }

    #[test]
    fn the_gpu_switch_off_keeps_the_model_off_every_device() {
        let local = LocalAgent { args: vec!["-t".into(), "8".into()], ..Default::default() };
        assert_eq!(local.server_args(false), ["--device", "none", "-t", "8"]);
    }

    #[test]
    fn the_default_shortcut_avoids_the_windows_key() {
        // Decision 9, and the reason is not taste: Windows 11 reassigns Win+C
        // to Copilot.
        assert!(
            !Config::default()
                .global_shortcut
                .to_lowercase()
                .contains("super")
        );
        assert!(
            !Config::default()
                .global_shortcut
                .to_lowercase()
                .contains("win")
        );
    }
}
