//! Microphone capture and speech-to-text, out of the window and out of `aria-core`.
//!
//! The chain this crate owns, in order, and the order is load-bearing:
//!
//! ```text
//! capture → energy gate → ASR → transcript veto → name → end of turn
//!        ↘ echo gate ↗ (in parallel, and only while ARIA is talking)
//! ```
//!
//! **The veto is before the name, and it was not always.** Until 2026-11-13
//! `ModeTrigger::NameHeard` was applied at the top of `aria::voice::act`, before
//! the transcript was weighed against what ARIA had said — so an echo carrying
//! ARIA's own name threw the turn away and **kept the wake**: hybrid flipped to
//! active on ARIA's own sentence. The sleep request had the same shape. Both now
//! come off `aria::voice::Allowed`, which cannot be obtained without passing the
//! veto. Decisions 114 and 116.
//!
//! What that order still cannot catch, by design: **an echo clipped to the name
//! alone**. [`echo::is_self_echo`] carries a floor of two words, and lowering it
//! would break what it protects — "Aria" alone over ARIA's own sentence is also
//! how a user interrupts. The acoustic gate answers that one, in 60 ms, without
//! needing the words.
//!
//! **The echo gate is wired, and it is wired beside the chain rather than
//! inside it.** That was not the original plan — this header used to draw it
//! between the energy gate and the ASR, so that frames explained by ARIA's own
//! voice never reached the transcriber. Both halves are done now, and the
//! difference between them is still worth knowing:
//!
//! | | |
//! |---|---|
//! | **cutting playback** when a second voice arrives | **done**, since 2026-11-03, by `aria::voice::Barge` on the shell's capture thread |
//! | **hiding ARIA's own voice from the ASR** | **done**, since 2026-11-04. Echo still opens a segment and still gets transcribed — but [`echo::SpokenLog::vetoes`] throws the transcript away before anything of the turn has an effect |
//!
//! Both halves live in the shell, and for the same reason: each *is* a
//! decision, and this crate does not make decisions. The cut needs the
//! `tts::Silencer`, which belongs to whoever owns the loudspeakers; the veto
//! needs to know what was spoken, and the thread that knows is not the thread
//! that hears. What this crate owns is the evidence — the gate, the word
//! comparison, and the bounded record the two threads share.
//!
//! What this crate deliberately does not own: the state machine over the three
//! audio modes, which belongs to `aria-core`. It answers "what was heard"; it
//! does not decide what to do about it. The TTS *is* here, since decision 56 —
//! this header said it would "land in T4", and T4 landed.
//!
//! # The seam with `aria-core`
//!
//! **The chain pushes in; the core never reaches out.** `aria-core` does not
//! depend on this crate and must not start — it drags an ONNX runtime, and the
//! boundary that pays there is "the core builds without a window *or* a model".
//! The shell is what wires the two together.
//!
//! So each piece here produces exactly one thing the core already knows how to
//! consume:
//!
//! | This crate | Feeds |
//! |---|---|
//! | [`gate::Segmenter`] | nothing directly — it says *where the pauses are*, so the detector has somewhere to be asked |
//! | [`turn::Detector`] + [`turn::decide`] | `turn_processed()`, and the moment a transcript becomes final |
//! | ASR transcripts | `push_transcript(text, final)` |
//! | [`wake::Detector`], confirmed by [`name::Name::addressed`] | `ModeTrigger::NameHeard` |
//! | [`name::asks_for_sleep`] | `ModeTrigger::SpokenSleep` |
//! | [`echo::Live`] | `VoiceState::Interrupted`, by stopping playback — through the shell, which is where the authority to stop lives |
//! | [`echo::SpokenLog`] | nothing — it *withholds*. A transcript it recognises as ARIA's own voice never reaches `heard_turn` |
//!
//! The judgement of *whether the name should change anything* is the core's,
//! not ours. What belongs here is knowing how an ASR mangles a proper noun —
//! which is a fact about Parakeet, not a policy about ARIA.
//!
//! [`calibration::Calibration`] carries the five numbers that are not
//! constants. It is settings data, serialised into the configuration file per
//! input device, and the defaults are measured rather than invented.

/// Re-exported so the shell can drive capture without choosing its own copy of
/// the recorder crate — two of them in the tree means two copies of cpal, and
/// two devices opened on the same microphone.
pub use handy_recorder;

pub mod asr;
pub mod calibration;
pub mod echo;
pub mod engine;
pub mod gate;
pub mod gpu;
pub mod mel;
pub mod models;
pub mod name;
pub mod runtime;
pub mod tts;
pub mod turn;
pub mod wake;
pub mod wav;

/// Where recordings live: `samples/` at the root of the project, git-ignored.
///
/// One place, and every bench in this crate looks here by default — a
/// calibration set split across two directories is a calibration set that
/// silently scores half of itself and reports a threshold for the other half.
///
/// Resolved from `CARGO_MANIFEST_DIR` rather than the working directory, so it
/// is the same folder whether the command is run from the workspace root or
/// from the crate. `ARIA_SAMPLES` overrides it.
pub fn samples_dir() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("ARIA_SAMPLES") {
        return std::path::PathBuf::from(path);
    }
    // Two levels up rather than `../..`, so what gets printed in an error is a
    // path someone can paste.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("this crate lives two levels below the workspace root")
        .join("samples")
}

/// Everything ARIA keeps on this machine and never roams: `%LOCALAPPDATA%/aria`.
///
/// `ARIA_LOCAL` overrides it, for a machine where this belongs on another disk.
///
/// # Lowercase, and the reason is Linux
///
/// It was `ARIA` until 2026-11-13. Windows does not care — its filesystem is
/// case-insensitive, so the rename costs nothing there and the 4.3 GB already
/// on disk are found either way. **Linux does care**, and that is the whole
/// point: `$XDG_DATA_HOME` and `~/.config` hold lowercase names, a stray
/// capitalised folder among them is noise, and the day this runs on Linux a
/// path spelled two ways is two folders. Changed while it was free rather than
/// after there was data under both spellings.
///
/// It also agrees with `~/.aria`, which was lowercase from the start.
///
/// Not `app_local_data_dir()`, which Tauri resolves to
/// `%LOCALAPPDATA%/<identifier>` — `dev.coopaguard.aria`. Both are correct
/// places; having **two of them** is what is not, and that is what this
/// function exists to prevent. It was briefly true: the voice's weights were
/// here while the memory's were going to land under the identifier, so one
/// application would have had two model folders and the open item of T1
/// — "nothing says where they are" — would have had two answers.
///
/// It lives in this crate rather than in `aria-core` for a reason the header
/// gives: the core must not depend on this crate, and this crate must not
/// depend on the core, so the definition goes where the models are. `app/` is
/// the only other caller, and it already depends on this crate.
pub fn local_root() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ARIA_LOCAL") {
        return std::path::PathBuf::from(dir);
    }
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    std::path::PathBuf::from(base).join("aria")
}

/// Where every model's weights live: `%LOCALAPPDATA%/aria/models`.
///
/// One sub-folder per engine — `parakeet-tdt-0.6b-v3`, `smart-turn-v3`,
/// `pocket-tts`, `phonon`, `memory`. The names of the four that already hold
/// weights are **not** ours to change: 3.3 GB sit under them, and renaming the
/// folder would silently re-download all of it.
///
/// `ARIA_MODELS` overrides the root; each engine keeps its own variable for the
/// case where one model, and not the set, belongs elsewhere.
pub fn models_root() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ARIA_MODELS") {
        return std::path::PathBuf::from(dir);
    }
    local_root().join("models")
}

/// What the machine can hear from, and what it can run the models on.
///
/// Printed before anything else is attempted: a missing microphone and a missing
/// GPU backend are the two failures worth seeing immediately, and they are the
/// two that no amount of code can work around.
pub fn describe_host() -> String {
    let mut out = String::new();

    out.push_str("input devices\n");
    match handy_recorder::list_input_devices() {
        Ok(devices) if devices.is_empty() => out.push_str("  none — nothing to capture\n"),
        Ok(devices) => {
            for d in devices {
                out.push_str(&format!("  {d:?}\n"));
            }
        }
        Err(e) => out.push_str(&format!("  cannot be listed: {e}\n")),
    }

    out.push_str("microphone permission\n");
    out.push_str(&format!("  {:?}\n", handy_recorder::permission_status()));

    out
}

#[cfg(test)]
mod path_tests {
    /// One root, and every engine under it.
    ///
    /// Written as the failure it prevents: for a few hours the memory's weights
    /// resolved under `%LOCALAPPDATA%/dev.coopaguard.aria` while these four
    /// resolved under `%LOCALAPPDATA%/aria`, so one application had two model
    /// folders and nothing said so (decision 104). The check is cheap and the
    /// drift is invisible, which is exactly when a test earns its place.
    #[test]
    fn every_model_resolves_under_the_one_root() {
        // The per-engine overrides must not be set, or this proves nothing.
        for key in ["ARIA_MODELS", "ARIA_LOCAL", "ARIA_ASR_MODEL", "ARIA_TURN_MODEL",
                    "ARIA_POCKET_CACHE", "ARIA_PHONON_CACHE"] {
            if std::env::var(key).is_ok() {
                println!("{key} est posé : contrôle non joué");
                return;
            }
        }

        let root = super::models_root();
        assert!(root.ends_with("models"), "{}", root.display());
        assert_eq!(root.parent().and_then(|p| p.file_name()), Some("aria".as_ref()));

        for path in [
            super::asr::default_model_dir(),
            super::turn::Detector::default_model_path(),
            super::tts::kyutai::cache_dir(),
            super::tts::phonon::cache_dir(),
        ] {
            assert!(path.starts_with(&root), "{} hors de {}", path.display(), root.display());
        }
    }
}
