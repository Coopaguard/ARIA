//! Where the memory lives, and who loads its vectoriser.
//!
//! This module exists because of one line in decision 94: **the core receives
//! its root, it does not discover it.** The path API that knows where `~` is
//! belongs to Tauri, and Tauri lives here. The core gets a
//! [`aria_core::store::Root`] and never a home directory, which is also what
//! makes its confinement tests run against a throwaway folder.

use std::path::PathBuf;
use std::sync::Arc;

use aria_core::store::{Memory, Root};
use aria_core::Core;
use tauri::{AppHandle, Manager};

/// The folder the pages live in, under the user's own home.
///
/// `~/.aria/memories`, and decision 94 chose it against `%APPDATA%` with an
/// argument that is the mirror image of decision 24's. The settings file sits
/// in `%APPDATA%` because **nobody needs to open it** — the settings screen is
/// its surface. The memory is the opposite: someone *will* open it, and the
/// whole weight of decisions 82 and 88 rests on them being able to. A hidden
/// `%APPDATA%` path a non-developer cannot find would empty that of meaning.
pub fn pages_dir(app: &AppHandle) -> Result<PathBuf, tauri::Error> {
    Ok(app.path().home_dir()?.join(".aria").join("memories"))
}

/// Where the derived index goes — local to this machine, never roamed.
///
/// And this is decision 94's objection turned into an argument: a home
/// directory is frequently redirected to OneDrive on Windows 11, so an index
/// brushed on every write (decision 92) would be copied back and forth for
/// nothing, and two machines would share an index describing two different sets
/// of pages. `%LOCALAPPDATA%` is explicitly not itinerant.
///
/// `%LOCALAPPDATA%/aria/index`, and **not** `app_local_data_dir()`, which Tauri
/// resolves to `%LOCALAPPDATA%/dev.coopaguard.aria`: decision 104 keeps one
/// local root for everything ARIA writes, because the alternative was two, and
/// the models were about to be split across both.
pub fn index_dir(_app: &AppHandle) -> Result<PathBuf, tauri::Error> {
    Ok(aria_voice::local_root().join("index"))
}

/// Where the memory's own weights go: `%LOCALAPPDATA%/aria/models/memory`.
///
/// Beside the four engines that already hold weights, and resolved by the same
/// function they are (decision 104). The vectoriser is a model like the others;
/// what it is for does not change where it belongs.
pub fn weights_dir() -> PathBuf {
    aria_voice::models_root().join("memory")
}

/// Open the memory and hand it to the core.
///
/// Failure here is not fatal and must not be: a memory that will not open costs
/// the memory, not the application. The tools then answer
/// `MemoryError::Detached`, which a model reads and reports, and the rest of
/// ARIA works exactly as it did before there was a memory at all.
pub fn attach(app: &AppHandle, core: &Core) {
    let path = match pages_dir(app) {
        Ok(path) => path,
        Err(e) => {
            tracing::error!("dossier de mémoire introuvable : {e}");
            return;
        }
    };
    let opened = Root::open(&path)
        .map_err(|e| e.to_string())
        .and_then(|root| Memory::open(root).map_err(|e| e.to_string()));
    match opened {
        Ok(memory) => {
            tracing::info!(pages = memory.len(), "mémoire ouverte dans {}", path.display());
            if !memory.unreadable().is_empty() {
                tracing::warn!(pages = ?memory.unreadable(), "pages illisibles, ignorées");
            }
            core.attach_memory(memory);
        }
        Err(e) => tracing::error!("mémoire non ouverte ({}) : {e}", path.display()),
    }
}

/// Load the vectoriser on its own thread, then attach it.
///
/// **On its own, and never through the voice's `LoadRegistry`.** Decision 97
/// corrected decision 83 on exactly this: that registry drives
/// `VoiceState::Loading`, which is the orb *and* whether ARIA can speak. Making
/// an optional component gate the voice is what decision 92 forbids — a memory
/// that finds nothing beats an ARIA that will not answer.
///
/// So the thread is detached and nobody waits for it. Until it finishes, the
/// lexical half answers alone, which is a degradation and not a failure.
pub fn spawn_vectoriser(app: &AppHandle, core: Arc<Core>) {
    let cache = index_dir(app).ok().map(|dir| dir.join("vectors.db"));
    let weights = Some(weights_dir());

    std::thread::Builder::new()
        .name("vectoriser".into())
        .spawn(move || {
            // The ear's environment, shared: `ort` allows one per process, and
            // a second built here refused whichever started later — the ear,
            // on 2026-11-18, which left ARIA deaf. See `E5Base::load`.
            let environment = match aria_voice::asr::environment() {
                Ok(environment) => Some(environment),
                Err(e) => {
                    tracing::warn!("environnement ONNX indisponible pour la mémoire : {e:#}");
                    None
                }
            };
            let model = match aria_memory::E5Base::load(weights, environment) {
                Ok(model) => Arc::new(model),
                // The first run downloads about a gigabyte, so this is also
                // what an offline first start looks like. Logged and dropped.
                Err(e) => {
                    tracing::warn!("vectoriseur non chargé, recherche lexicale seule : {e}");
                    return;
                }
            };
            if let Err(e) = core.with_memory(|memory| {
                memory.attach_embedder(model, cache.as_deref())?;
                Ok(memory.pending_vectors())
            }) {
                tracing::warn!("vectoriseur non attaché : {e}");
                return;
            }
            // Deliberately not catching up here. Decision 92: vectorising
            // takes CPU, decision 63 measured what that costs — the synthesis
            // is the bottleneck and the voice comes out chopped when something
            // else eats cores. The catch-up belongs to a silence, and
            // `Core::memory_vectorise` is what a silence calls.
            //
            // Decision 103 shrank what is left for it: every write vectorises
            // its own page now, so this batch covers only what did not pass
            // through a write — a folder that already existed before the
            // model did, pages edited in an editor, and a write whose forward
            // pass failed. On a first start with an empty folder it is zero.
            match core.with_memory(|memory| Ok(memory.pending_vectors())) {
                Ok(0) => tracing::info!("vectoriseur attaché, rien à rattraper"),
                Ok(pending) => tracing::info!(
                    pending,
                    "vectoriseur attaché ; le rattrapage attend un silence"
                ),
                Err(e) => tracing::warn!("état du rattrapage inconnu : {e}"),
            }
        })
        .map(|_| ())
        .unwrap_or_else(|e| tracing::error!("fil du vectoriseur non lancé : {e}"));
}
