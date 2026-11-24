//! The ear's weights, fetched when they are not there.
//!
//! Decided on 2026-11-13: **Parakeet and Smart Turn download themselves at
//! launch.** Until then the two ONNX models of the ear were the only ones a
//! human had to go and get — 2.41 GB of the 4.3 GB on disk — and `asr::load`
//! met a newcomer with a path and a link. The three other engines already
//! fetched their own (`tts::kyutai` through Pocket TTS, [`crate::tts::phonon`]
//! through this client, the memory's vectoriser through `fastembed`), so what
//! was missing was not a policy but these five files.
//!
//! # Why they are not left to `hf-hub`'s cache
//!
//! The Hub client can keep its own cache, and that is what [`crate::tts::phonon`]
//! lets it do. The ear cannot: 2.4 GB already sit in
//! `%LOCALAPPDATA%/aria/models/parakeet-tdt-0.6b-v3`, in a flat layout, and
//! decision 104 is explicit that **those folders are not to be renamed** —
//! a move re-downloads everything without saying so. So each file is asked for
//! with `local_dir`, which puts it exactly where the loader already looks, and
//! an existing install is recognised rather than duplicated.
//!
//! That also means the paths here must be **derived from the loaders** and not
//! spelled again: [`crate::asr::default_model_dir`] and
//! [`crate::turn::Detector::default_model_path`] honour `ARIA_ASR_MODEL` and
//! `ARIA_TURN_MODEL`, and a download that ignored them would land somewhere
//! nothing reads.
//!
//! # The trap that shapes the whole module
//!
//! **`hf-hub` writes straight to the destination.** Read in its source
//! (1.0, `repository/download.rs`): `File::create(dest)` then stream, with no
//! temporary file and no rename. So a download interrupted at 60 % of a 2.4 GB
//! file leaves a **truncated file at the final path** — and
//! [`crate::asr::load`] only checks that `encoder-model.onnx` *exists*. The
//! next start would load a half file and fail inside ONNX Runtime, which is
//! the worst place to find out.
//!
//! Hence the staging folder: every file is fetched into `.partial` beside its
//! destination, and only a **completed** download is renamed into place. Same
//! parent, so the rename is atomic and never a copy of 2.4 GB. A failure
//! leaves the fragment in `.partial`, where the next attempt overwrites it and
//! nothing mistakes it for a model.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Where a staging download waits for its rename.
///
/// Inside the destination's own parent, which is what makes the rename atomic:
/// a staging area elsewhere could be on another volume, and `rename` would
/// either fail or silently become a 2.4 GB copy.
const STAGING: &str = ".partial";

/// One file the ear needs, and where it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Weight {
    /// `owner/name` on the Hub.
    pub repo: &'static str,
    /// Its path **inside** the repository, which is not always its name here:
    /// Parakeet's four files live in a folder of the repository and flat in
    /// ours.
    pub remote: &'static str,
    /// The absolute file the loader will open.
    pub local: PathBuf,
}

impl Weight {
    /// Already on disk?
    ///
    /// Existence only, and the module header says why that is not enough on
    /// its own: a truncated file exists too. What makes this honest is that
    /// nothing truncated ever reaches this path — see [`STAGING`].
    pub fn present(&self) -> bool {
        self.local.is_file()
    }

    fn staging(&self) -> Result<PathBuf> {
        let parent = self
            .local
            .parent()
            .with_context(|| format!("{} n'a pas de dossier parent", self.local.display()))?;
        Ok(parent.join(STAGING))
    }
}

/// The five files of the ear: four for Parakeet, one for Smart Turn.
///
/// The repositories are the ones the loaders' own error messages have named
/// since T1, verified to resolve on 2026-11-13:
/// `huggingface.co/altunenes/parakeet-rs` and
/// `huggingface.co/pipecat-ai/smart-turn-v3`. Neither is gated.
pub fn ear() -> Vec<Weight> {
    let asr = crate::asr::default_model_dir();
    let turn = crate::turn::Detector::default_model_path();
    let mut all: Vec<Weight> = [
        "encoder-model.onnx",
        "encoder-model.onnx.data",
        "decoder_joint-model.onnx",
        "vocab.txt",
    ]
    .into_iter()
    .map(|name| Weight {
        repo: "altunenes/parakeet-rs",
        // The repository keeps them in a folder named after the model; we keep
        // them flat in a folder of the same name. Hence a remote path and a
        // local one that do not match.
        remote: match name {
            "encoder-model.onnx" => "parakeet-tdt-0.6b-v3/encoder-model.onnx",
            "encoder-model.onnx.data" => "parakeet-tdt-0.6b-v3/encoder-model.onnx.data",
            "decoder_joint-model.onnx" => "parakeet-tdt-0.6b-v3/decoder_joint-model.onnx",
            _ => "parakeet-tdt-0.6b-v3/vocab.txt",
        },
        local: asr.join(name),
    })
    .collect();
    all.push(Weight {
        repo: "pipecat-ai/smart-turn-v3",
        remote: "smart-turn-v3.2-cpu.onnx",
        local: turn,
    });
    all
}

/// The ear without Parakeet: Smart Turn alone, for a main agent that hears
/// the audio itself (epic 10).
pub fn listener() -> Vec<Weight> {
    ear()
        .into_iter()
        .filter(|weight| weight.local == crate::turn::Detector::default_model_path())
        .collect()
}

/// A local main agent's two files — the model and its audio encoder — in
/// `<models root>/<dir>`, beside the ear's folders. Epic 10.
///
/// The names come from `aria_core::agent::local`, which this crate cannot
/// see: `app/` passes them through, as plain strings.
pub fn brain(repo: &'static str, dir: &str, files: [&'static str; 2]) -> Vec<Weight> {
    let folder = crate::models_root().join(dir);
    files
        .into_iter()
        .map(|file| Weight {
            repo,
            remote: file,
            local: folder.join(file),
        })
        .collect()
}

/// What is missing, in the order it will be fetched.
pub fn missing(all: &[Weight]) -> Vec<&Weight> {
    all.iter().filter(|weight| !weight.present()).collect()
}

/// Fetch whatever the ear is missing, and say what is happening while it does.
///
/// Returns how many files were downloaded — zero on every start but the first.
/// `announce` is called once per file, before it starts, because that is the
/// only cadence worth showing: one of these files is 2.4 GB and the other four
/// together are 76 MB, so a per-file bar would sit at four fifths for minutes.
/// The byte-level progress goes to the log.
///
/// **Errors are returned, not swallowed.** A missing ear is not a degraded
/// mode — ARIA hears nothing at all — so the caller decides, and today
/// [`crate::engine::Engine::load`] turns it into the message that parks the
/// voice chain rather than retrying.
pub fn ensure(announce: impl FnMut(&Weight, usize, usize)) -> Result<usize> {
    ensure_weights(ear(), announce)
}

/// The same, for any list of files — the ear, the listener, a local agent.
pub fn ensure_weights(
    all: Vec<Weight>,
    mut announce: impl FnMut(&Weight, usize, usize),
) -> Result<usize> {
    // A file already fetched by another tool — the POC fills the Hugging Face
    // cache with exactly these GGUF — is linked rather than downloaded again.
    for weight in missing(&all) {
        adopt_from_hub_cache(weight);
    }
    let wanted = missing(&all);
    if wanted.is_empty() {
        return Ok(0);
    }

    tracing::info!(
        "{} fichier(s) de modèle manquant(s) : téléchargement depuis le Hub",
        wanted.len()
    );
    let client = client()?;
    let total = wanted.len();
    let _over = Over;
    for (index, weight) in wanted.iter().enumerate() {
        announce(weight, index + 1, total);
        fetch(&client, weight, index + 1, total)
            .with_context(|| format!("{}/{} n'a pas pu être téléchargé", weight.repo, weight.remote))?;
    }
    tracing::info!("{total} fichier(s) de modèle téléchargé(s)");
    Ok(total)
}

/// One file, into staging, then into place.
fn fetch(client: &hf_hub::HFClientSync, weight: &Weight, index: usize, count: usize) -> Result<()> {
    let (owner, name) = weight
        .repo
        .split_once('/')
        .with_context(|| format!("{} : owner/name attendu", weight.repo))?;
    let staging = weight.staging()?;
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("impossible de créer {}", staging.display()))?;

    tracing::info!(
        "{} → {} (dans {})",
        weight.remote,
        weight.local.display(),
        STAGING
    );
    let downloaded = client
        .model(owner, name)
        .download_file()
        .filename(weight.remote.to_string())
        .local_dir(staging.clone())
        .progress(Relay {
            file: weight.remote.rsplit('/').next().unwrap_or(weight.remote).to_string(),
            index,
            count,
            last: std::sync::Mutex::new(None),
        })
        .send()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if let Some(parent) = weight.local.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("impossible de créer {}", parent.display()))?;
    }
    std::fs::rename(&downloaded, &weight.local).with_context(|| {
        format!(
            "{} n'a pas pu être déplacé vers {}",
            downloaded.display(),
            weight.local.display()
        )
    })?;
    // The staging tree is left behind empty; removing it is best-effort, and a
    // failure here is not worth failing a download that succeeded.
    tidy(&staging);
    Ok(())
}

/// Bring in a file the Hugging Face cache already holds, if it does.
///
/// Five gigabytes the user already has on disk are not worth fetching again
/// because another tool put them under `~/.cache/huggingface`. A **hard link**,
/// so it costs no space and survives that cache being emptied; a copy when the
/// two are on different volumes, which a hard link cannot cross.
///
/// Best effort: anything that does not work out leaves the download to do its
/// job, and says nothing louder than a debug line.
fn adopt_from_hub_cache(weight: &Weight) {
    let Some(snapshots) = hub_cache()
        .map(|hub| {
            hub.join(format!("models--{}", weight.repo.replace('/', "--")))
                .join("snapshots")
        })
        .filter(|dir| dir.is_dir())
    else {
        return;
    };
    let found = std::fs::read_dir(&snapshots)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|snapshot| snapshot.path().join(weight.remote))
        .find(|candidate| candidate.is_file());
    let Some(found) = found else {
        return;
    };
    // The cache keeps symlinks to blobs on some systems: resolve them, so the
    // link points at the bytes and not at a link.
    let source = std::fs::canonicalize(&found).unwrap_or(found);
    if let Some(parent) = weight.local.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let adopted = std::fs::hard_link(&source, &weight.local)
        .or_else(|_| std::fs::copy(&source, &weight.local).map(|_| ()));
    match adopted {
        Ok(()) => tracing::info!(
            "{} repris du cache Hugging Face ({})",
            weight.remote,
            source.display()
        ),
        Err(e) => tracing::debug!("{} : reprise du cache impossible ({e})", weight.remote),
    }
}

/// `~/.cache/huggingface/hub`, or where `HF_HOME` puts it.
fn hub_cache() -> Option<PathBuf> {
    if let Ok(cache) = std::env::var("HF_HUB_CACHE") {
        return Some(PathBuf::from(cache));
    }
    std::env::var("HF_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|home| home.join(".cache").join("huggingface")))
        .map(|home| home.join("hub"))
}

/// Remove the staging tree if nothing is left in it.
fn tidy(staging: &Path) {
    fn empty_dirs(dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                empty_dirs(&entry.path());
            }
        }
        let _ = std::fs::remove_dir(dir);
    }
    empty_dirs(staging);
}

/// The Hub client, with our cache and our token stated outright.
///
/// Shared with [`crate::tts::phonon`], and that sharing is the point: `hf-hub`
/// 1.0 reads **no** environment variable, talks to the Hub unauthenticated and
/// caches relative to the current working directory unless told otherwise. One
/// place to get that wrong is enough.
///
/// `cache` is where the client may keep its own copies. The ear never uses it —
/// `local_dir` bypasses the cache entirely — but the builder has to be given
/// one, or it falls back to the working directory.
pub fn client_with_cache(cache: &Path) -> Result<hf_hub::HFClientSync> {
    std::fs::create_dir_all(cache)
        .with_context(|| format!("impossible de créer {}", cache.display()))?;
    let mut builder = hf_hub::HFClient::builder().cache_dir(cache);
    if let Some(token) = token() {
        builder = builder.token(token);
    }
    builder
        .build_sync()
        .map_err(|e| anyhow::anyhow!("client Hugging Face indisponible : {e}"))
}

/// The client the ear uses, with a cache beside the models.
fn client() -> Result<hf_hub::HFClientSync> {
    client_with_cache(&crate::models_root().join(".hub"))
}

/// The token, if the user has one.
///
/// Read **here** rather than left to the library, because `hf-hub` 1.0 reads no
/// environment variable and talks to the Hub unauthenticated. Neither model of
/// the ear is gated, so for them this only buys a better rate limit — but the
/// same function serves [`crate::tts::phonon`], **whose French voices are all
/// gated** and answer 401 without it.
///
/// Two sources, and the second is the one that matters for someone who has
/// already done the work: `HF_TOKEN` (or `HUGGING_FACE_HUB_TOKEN`), else the
/// token `hf auth login` wrote. Somebody who has logged in once should not have
/// to learn about an environment variable to use a model whose terms they have
/// already accepted.
pub fn token() -> Option<String> {
    for key in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"] {
        if let Ok(token) = std::env::var(key) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    let token = std::fs::read_to_string(token_file()?).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Where `hf auth login` keeps the token, and where [`save_token`] writes it.
fn token_file() -> Option<PathBuf> {
    let home = std::env::var("HF_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|home| home.join(".cache").join("huggingface")))?;
    Some(home.join("token"))
}

/// Store a token the way `hf auth login` does, for the first-run screen (T12):
/// the French voices are gated, and a newcomer should not need the `hf` CLI.
///
/// Shared with every Hugging Face tool on the machine — that is the point of
/// writing it there rather than in `aria.toml`, and it keeps the secret out of
/// a file people copy around.
pub fn save_token(token: &str) -> Result<()> {
    let token = token.trim();
    anyhow::ensure!(token.starts_with("hf_"), "un jeton Hugging Face commence par « hf_ »");
    let path = token_file().context("dossier personnel introuvable")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("impossible de créer {}", parent.display()))?;
    }
    std::fs::write(&path, token).with_context(|| format!("impossible d'écrire {}", path.display()))
}

/// `~`, without pulling in a crate for it.
pub fn dirs_home() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five files, and each one pointing at the folder its loader reads.
    /// A download that lands anywhere else is 2.4 GB spent for nothing.
    #[test]
    fn the_ear_asks_for_five_files_where_the_loaders_look() {
        let all = ear();
        assert_eq!(all.len(), 5);

        let asr = crate::asr::default_model_dir();
        for weight in all.iter().take(4) {
            assert_eq!(weight.local.parent(), Some(asr.as_path()));
            assert!(
                weight.remote.starts_with("parakeet-tdt-0.6b-v3/"),
                "{}",
                weight.remote
            );
        }
        assert_eq!(all[4].local, crate::turn::Detector::default_model_path());
    }

    /// The local name and the remote path differ for Parakeet, and that is the
    /// mapping a refactor would quietly break: the repository keeps the four
    /// files in a folder, we keep them flat in a folder of the same name.
    #[test]
    fn a_remote_path_is_not_a_local_name() {
        let all = ear();
        let encoder = &all[0];
        assert_eq!(encoder.remote, "parakeet-tdt-0.6b-v3/encoder-model.onnx");
        assert_eq!(
            encoder.local.file_name().unwrap().to_string_lossy(),
            "encoder-model.onnx"
        );
    }

    /// Staging sits beside the destination, because a rename across volumes is
    /// a copy of 2.4 GB or an error.
    #[test]
    fn staging_shares_the_destination_parent() {
        let weight = Weight {
            repo: "o/n",
            remote: "a/b.onnx",
            local: PathBuf::from("/models/ear/b.onnx"),
        };
        assert_eq!(
            weight.staging().unwrap(),
            PathBuf::from("/models/ear").join(STAGING)
        );
    }

    /// The listener is the ear minus Parakeet: one file, Smart Turn's.
    #[test]
    fn the_listener_needs_only_the_turn_detector() {
        let listener = listener();
        assert_eq!(listener.len(), 1);
        assert_eq!(listener[0].local, crate::turn::Detector::default_model_path());
    }

    /// A local agent's files sit under the one models root — decision 104's
    /// rule, which a second folder elsewhere is how it was broken once.
    #[test]
    fn a_brain_lands_under_the_models_root() {
        let files = brain("o/n", "gemma-4-e4b", ["m.gguf", "mmproj.gguf"]);
        assert_eq!(files.len(), 2);
        for file in &files {
            assert!(file.local.starts_with(crate::models_root().join("gemma-4-e4b")));
            assert_eq!(file.local.file_name().unwrap().to_string_lossy(), file.remote);
        }
    }

    /// Present means on disk, and missing is what gets fetched.
    #[test]
    fn only_what_is_absent_is_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let there = dir.path().join("there.onnx");
        std::fs::write(&there, b"x").expect("write");
        let all = vec![
            Weight {
                repo: "o/n",
                remote: "there.onnx",
                local: there,
            },
            Weight {
                repo: "o/n",
                remote: "absent.onnx",
                local: dir.path().join("absent.onnx"),
            },
        ];
        let wanted = missing(&all);
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].remote, "absent.onnx");
    }

    /// The whole mechanism against the real Hub, for 8 MB.
    ///
    /// `#[ignore]` because it goes to the network, and Smart Turn rather than
    /// Parakeet because 8.3 MB proves exactly the same four things as 2.4 GB:
    /// the client builds, `local_dir` puts the file where we asked, the rename
    /// lands it at its final name, and the staging folder is gone afterwards.
    /// Run with `cargo test -p aria-voice --lib models -- --ignored`.
    #[test]
    #[ignore = "télécharge 8,3 Mo depuis le Hub"]
    fn a_real_download_lands_where_the_loader_looks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let weight = Weight {
            repo: "pipecat-ai/smart-turn-v3",
            remote: "smart-turn-v3.2-cpu.onnx",
            local: dir.path().join("smart-turn-v3.2-cpu.onnx"),
        };
        assert!(!weight.present());

        let client = client_with_cache(&dir.path().join(".hub")).expect("client");
        fetch(&client, &weight, 1, 1).expect("le fichier descend");

        assert!(weight.present());
        let size = std::fs::metadata(&weight.local).expect("metadata").len();
        println!("{} octets", size);
        assert!(size > 8_000_000, "{size} octets : fichier tronqué ?");
        assert!(
            !dir.path().join(STAGING).exists(),
            "le dossier de transit est resté"
        );
    }

    /// An empty staging tree is removed; one that still holds a fragment is
    /// left alone, so the next attempt overwrites it instead of starting over.
    #[test]
    fn a_fragment_left_behind_is_not_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staging = dir.path().join(STAGING);
        std::fs::create_dir_all(staging.join("sub")).expect("mkdir");
        std::fs::write(staging.join("sub").join("half.onnx"), b"half").expect("write");

        tidy(&staging);

        assert!(staging.join("sub").join("half.onnx").is_file());
    }
}

// ----------------------------------------------------------------- observing

/// One file coming down, as the screen shows it.
///
/// The first-run screen (T12) is the reason: 2.4 GB fetched behind a loading
/// bar that only counts models looks like a hang. The crate cannot see
/// `aria-core`, so `app/` registers an [`observe`]r that forwards this to the
/// event stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub file: String,
    pub index: usize,
    pub count: usize,
    pub received: u64,
    /// `0` until the Hub has answered the HEAD request.
    pub size: u64,
}

type Observer = Box<dyn Fn(Option<Progress>) + Send + Sync>;
static OBSERVER: std::sync::OnceLock<Observer> = std::sync::OnceLock::new();

/// Be told about every download: `Some` while a file comes down, `None` once
/// the batch is over — finished or failed. Set once per process.
pub fn observe(observer: impl Fn(Option<Progress>) + Send + Sync + 'static) {
    let _ = OBSERVER.set(Box::new(observer));
}

pub(crate) fn tell(progress: Option<Progress>) {
    if let Some(observer) = OBSERVER.get() {
        observer(progress);
    }
}

/// Says `None` when the batch ends, whichever way it ends.
pub(crate) struct Over;
impl Drop for Over {
    fn drop(&mut self) {
        tell(None);
    }
}

/// Turns the Hub's events into [`Progress`], four times a second at most.
pub(crate) struct Relay {
    file: String,
    index: usize,
    count: usize,
    last: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Relay {
    fn send(&self, received: u64, size: u64, force: bool) {
        let mut last = self.last.lock().unwrap();
        let now = std::time::Instant::now();
        if !force && last.is_some_and(|at| now.duration_since(at).as_millis() < 250) {
            return;
        }
        *last = Some(now);
        tell(Some(Progress {
            file: self.file.clone(),
            index: self.index,
            count: self.count,
            received,
            size,
        }));
    }
}

impl hf_hub::progress::ProgressHandler for Relay {
    fn on_progress(&self, event: &hf_hub::progress::ProgressEvent) {
        use hf_hub::progress::{DownloadEvent, ProgressEvent};
        let ProgressEvent::Download(event) = event else { return };
        match event {
            DownloadEvent::Start { total_bytes, .. } => self.send(0, *total_bytes, true),
            DownloadEvent::Progress { files } => {
                for file in files {
                    self.send(file.bytes_completed, file.total_bytes, false);
                }
            }
            DownloadEvent::AggregateProgress {
                bytes_completed,
                total_bytes,
                ..
            } => self.send(*bytes_completed, *total_bytes, false),
            _ => {}
        }
    }
}

/// For the other engines that fetch through the Hub — Phonon: the relay for
/// one file, and the guard that says `None` when it is over.
pub(crate) fn watched(file: &str) -> (Relay, Over) {
    (
        Relay {
            file: file.rsplit('/').next().unwrap_or(file).to_string(),
            index: 1,
            count: 1,
            last: std::sync::Mutex::new(None),
        },
        Over,
    )
}
