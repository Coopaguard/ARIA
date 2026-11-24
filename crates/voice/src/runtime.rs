//! Which ONNX Runtime this process loads, and where it comes from.
//!
//! Decision 131, 2026-11-22. ONNX Runtime used to be linked into the binary,
//! which fixed one stack per build: the prebuilt static libraries of `ort`
//! carry CUDA and DirectML together, or OpenVINO alone, never both. The user
//! wants the card's own stack — CUDA on NVIDIA, oneAPI (OpenVINO) on Intel — so
//! the runtime is now a **DLL loaded at start-up**, one per stack:
//!
//! | Stack | Runtime | From |
//! |---|---|---|
//! | DirectML | Microsoft ORT DirectML 1.24.4 + DirectML 1.15.4 | **shipped** beside the executable (`app/build.rs`) |
//! | CUDA | Microsoft ORT GPU 1.31.1 + CUDA 13 / cuDNN 9 (1.3 GB) | downloaded |
//! | oneAPI | Intel ORT OpenVINO 1.24.1 | downloaded |
//!
//! ROCm has no Windows build at all: an AMD card runs DirectML.
//!
//! # When a stack is downloaded, and when it is used
//!
//! Downloaded when GPU inference is on and the card's maker asks for it
//! ([`ensure_wanted`], called by `Engine::load`, under the ear's loading bar).
//! **Used from the next start**: one ONNX Runtime per process, and it is loaded
//! before anything else touches ONNX — the memory's vectoriser included. Once
//! the card's own runtime is there, it is loaded whatever the switch says: it
//! carries the CPU provider too, so switching the GPU off or on needs no
//! restart.
//!
//! # API 24
//!
//! `ort` is built against API 24 rather than its default 31, because Microsoft
//! stopped DirectML at 1.24 and Intel's OpenVINO build is at 1.24 too. A newer
//! runtime (CUDA's 1.31) is accepted; an older one is refused by `ort` itself.
//!
//! # Never a half folder
//!
//! Archives land in `<stack>/.downloads`, checked against a pinned size and
//! sha256, and are kept until the whole set is unpacked. DLLs go to
//! `<stack>.partial`, which is renamed to `<stack>` only once complete. Then the
//! archives are deleted.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use parakeet_rs::ort;
use sha2::{Digest, Sha256};

use crate::gpu::Stack;

/// One pinned file: a NuGet package or a wheel, both zips — or the weights of
/// the French voice ([`crate::tts::phonon`]).
pub(crate) struct Archive {
    pub(crate) url: &'static str,
    pub(crate) size: u64,
    pub(crate) sha256: &'static str,
    /// Only DLLs under this prefix are kept; empty keeps every DLL. Means
    /// nothing for a file that is not unpacked.
    pub(crate) prefix: &'static str,
}

impl Archive {
    fn name(&self) -> &'static str {
        self.url.rsplit('/').next().unwrap_or(self.url)
    }
}

/// What a downloaded stack is made of.
struct Bundle {
    stack: Stack,
    folder: &'static str,
    archives: &'static [Archive],
}

const NATIVE: &str = "runtimes/win-x64/native/";

const CUDA: Bundle = Bundle {
    stack: Stack::Cuda,
    folder: "cuda",
    archives: &[
        Archive {
            url: "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.gpu.windows/1.31.1/microsoft.ml.onnxruntime.gpu.windows.1.31.1.nupkg",
            size: 160_446_793,
            sha256: "9c9c7c951b9e76036b5e90d2a519359969d717d1c16dfc718c808afb42d778b1",
            prefix: NATIVE,
        },
        Archive {
            url: "https://files.pythonhosted.org/packages/35/99/e07dd9565d2e0317c1a62ff39ae9fc0542c048f74bcfc777c8be97710347/nvidia_cuda_runtime-13.5.37-py3-none-win_amd64.whl",
            size: 2_780_254,
            sha256: "f225d3fa5ea6bfbb58fc4979ef72375cbe661d9107f0d8a4ff22cc151525e4a9",
            prefix: "",
        },
        Archive {
            url: "https://files.pythonhosted.org/packages/a3/df/f1246959833e2c437db8be3e5b477f66b87f8817821ed40de6c7561c9a36/nvidia_cublas-13.8.0.4-py3-none-win_amd64.whl",
            size: 423_266_897,
            sha256: "8c5494423bb8a46822cb6b0cb95d7fa4be2d7b96a31155dff083839ec8297910",
            prefix: "",
        },
        Archive {
            url: "https://files.pythonhosted.org/packages/94/79/8f2a7125f560b86571c0be3dbf10e33041187bc4a8cf66107541fe8e5cce/nvidia_cufft-12.5.0.14-py3-none-win_amd64.whl",
            size: 164_476_208,
            sha256: "78e7585159ecf37d73af1ce43a5dea7fd3360e1af71cde08f070c07966a336bc",
            prefix: "",
        },
        Archive {
            url: "https://files.pythonhosted.org/packages/9f/6c/241c5155199a6de55bad8862213ed64b487dc9526aae817e4af31afea1a4/nvidia_cuda_nvrtc-13.5.37-py3-none-win_amd64.whl",
            size: 49_248_991,
            sha256: "e8e6c10b251385707ae3abd7b4ee118de6736fd7678d0f0f4eab4d759f679c21",
            prefix: "",
        },
        Archive {
            url: "https://files.pythonhosted.org/packages/48/9c/26e6bdd31c97c389aa565b49c916be0dec17dd61dad6cd33fee286d1b946/nvidia_cudnn_cu13-9.28.1.4-py3-none-win_amd64.whl",
            size: 407_065_099,
            sha256: "73d874c6bd8a1c75ca040228f77c4e163efe9bab3b65bd85914e2b7334ac68ee",
            prefix: "",
        },
    ],
};

const ONEAPI: Bundle = Bundle {
    stack: Stack::OneApi,
    folder: "oneapi",
    archives: &[Archive {
        url: "https://api.nuget.org/v3-flatcontainer/intel.ml.onnxruntime.openvino/1.24.1/intel.ml.onnxruntime.openvino.1.24.1.nupkg",
        size: 43_011_684,
        sha256: "f53ad5f90e3d616970a5c65e4880ebbe92c9774e9727020661db591cea74a110",
        prefix: NATIVE,
    }],
};

fn bundle(stack: Stack) -> Option<&'static Bundle> {
    match stack {
        Stack::Cuda => Some(&CUDA),
        Stack::OneApi => Some(&ONEAPI),
        Stack::Rocm | Stack::DirectMl => None,
    }
}

/// The file in every runtime folder that is the runtime.
const DYLIB: &str = "onnxruntime.dll";

// ------------------------------------------------------------------ where

static SHIPPED: OnceLock<PathBuf> = OnceLock::new();

/// Where the shipped DirectML runtime is: `<resources>/ort`, told by `app/`.
/// Called once, before [`init`].
pub fn set_shipped(dir: PathBuf) {
    let _ = SHIPPED.set(dir);
}

fn shipped() -> PathBuf {
    if let Some(dir) = SHIPPED.get() {
        return dir.clone();
    }
    if let Ok(dir) = std::env::var("ARIA_ORT_DIR") {
        return PathBuf::from(dir);
    }
    // Tests and the crate's own binaries: what `app/build.rs` fetched.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/resources/ort")
}

fn downloaded(bundle: &Bundle) -> PathBuf {
    crate::local_root().join("runtime").join(bundle.folder)
}

pub fn installed(stack: Stack) -> bool {
    match bundle(stack) {
        Some(b) => downloaded(b).join(DYLIB).is_file(),
        None => stack == Stack::DirectMl,
    }
}

// ------------------------------------------------------------------- choose

/// The runtime loaded in this process.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub stack: Stack,
    pub dir: PathBuf,
}

static LOADED: OnceLock<Loaded> = OnceLock::new();

/// Load the runtime: the card's own stack if it is installed, DirectML
/// otherwise. Once per process, and before anything else uses ONNX.
pub fn init() -> Result<&'static Loaded> {
    if let Some(loaded) = LOADED.get() {
        return Ok(loaded);
    }
    let native = crate::gpu::card().native;
    let (stack, dir) = match native {
        Some(stack) if installed(stack) && stack != Stack::DirectMl => {
            (stack, downloaded(bundle(stack).expect("installed implies a bundle")))
        }
        _ => (Stack::DirectMl, shipped()),
    };
    let dylib = dir.join(DYLIB);
    if !dylib.is_file() {
        bail!(
            "pas d'ONNX Runtime dans {} — construire l'application le dépose \
             (`app/build.rs`), ou ARIA_ORT_DIR pointe ailleurs",
            dir.display()
        );
    }
    // On `PATH` first: DirectML.dll, OpenVINO's plugins, cuDNN's engines and
    // NVRTC are opened **by name**, and Windows does not look beside the DLL
    // asking. Without it, CUDA's first `Conv` fails (2026-11-22) and the
    // system's older DirectML.dll is picked up instead of ours.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.clone()];
    paths.extend(std::env::split_paths(&path));
    if let Ok(joined) = std::env::join_paths(paths) {
        // SAFETY: once, at start-up, before any ONNX library is loaded.
        unsafe { std::env::set_var("PATH", joined) };
    }
    if stack == Stack::Cuda {
        ort::ep::cuda::preload_dylibs(Some(&dir), Some(&dir))
            .map_err(|e| anyhow!("bibliothèques CUDA de {} : {e}", dir.display()))?;
    }
    // Loads the DLL and checks its version; the environment is built by
    // `asr::environment`.
    ort::init_from(&dylib).map_err(|e| anyhow!("{e}"))?;
    tracing::info!(?stack, dossier = %dir.display(), "ONNX Runtime chargé");
    Ok(LOADED.get_or_init(|| Loaded { stack, dir }))
}

/// The loaded runtime's stack, loading it if nothing has yet.
pub fn stack() -> Stack {
    init().map(|l| l.stack).unwrap_or(Stack::DirectMl)
}

/// The provider for the card, registered with the CPU behind it — the
/// configure hook of `parakeet-rs`, which is itself asked for the CPU so that
/// the choice, and OpenVINO's device, stay ours.
pub fn register(
    builder: ort::session::builder::SessionBuilder,
    stack: Stack,
) -> ort::Result<ort::session::builder::SessionBuilder> {
    let cpu = ort::ep::CPU::default().build().error_on_failure();
    Ok(match stack {
        Stack::Cuda => builder.with_execution_providers([ort::ep::CUDA::default().build(), cpu])?,
        // `GPU`, not the provider's default `CPU`: the point is Intel's card.
        Stack::OneApi => builder.with_execution_providers([
            ort::ep::OpenVINO::default().with_device_type("GPU").build(),
            cpu,
        ])?,
        Stack::DirectMl | Stack::Rocm => {
            builder.with_execution_providers([ort::ep::DirectML::default().build(), cpu])?
        }
    })
}

/// The loaded stack's provider registers — asked once, with
/// `error_on_failure`, because the registration [`register`] makes falls back
/// to the CPU without a word, which is the very thing this exists to see.
pub fn ready() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        let stack = stack();
        let probe = || -> Result<()> {
            let environment = crate::asr::environment()?;
            let builder =
                ort::session::Session::builder(&environment).map_err(|e| anyhow!("{e}"))?;
            match stack {
                Stack::Cuda => builder.with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()]),
                Stack::OneApi => builder.with_execution_providers([ort::ep::OpenVINO::default()
                    .with_device_type("GPU")
                    .build()
                    .error_on_failure()]),
                _ => builder.with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()]),
            }
            .map_err(|e| anyhow!("{e}"))?;
            Ok(())
        };
        match probe() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(?stack, "le fournisseur de la carte refuse de s'enregistrer : {e:#}");
                false
            }
        }
    })
}

// ----------------------------------------------------------------- download

/// Fetch the card's own runtime when GPU inference is on and it is missing.
/// Returns the stack downloaded, which is used from the next start.
///
/// Never fatal for the caller: an error is logged and DirectML goes on.
pub fn ensure_wanted() -> Option<Stack> {
    if !crate::asr::gpu() {
        return None;
    }
    let card = crate::gpu::card();
    let stack = card.native.filter(|s| bundle(*s).is_some())?;
    if installed(stack) {
        return None;
    }
    if stack == Stack::Cuda && !nvidia_driver() {
        tracing::warn!("carte NVIDIA sans pilote CUDA (nvcuda.dll) : DirectML");
        return None;
    }
    match ensure(bundle(stack).expect("filtered above")) {
        Ok(()) => {
            tracing::info!(
                ?stack,
                "runtime de la carte installé : il sera utilisé au prochain démarrage d'ARIA"
            );
            Some(stack)
        }
        Err(e) => {
            tracing::warn!(?stack, "runtime de la carte indisponible, DirectML à la place : {e:#}");
            None
        }
    }
}

/// `nvcuda.dll`, the NVIDIA driver's user-mode half: without it CUDA cannot
/// run, whatever the card.
fn nvidia_driver() -> bool {
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Path::new(&system).join("System32").join("nvcuda.dll").is_file()
}

static DOWNLOADING: Mutex<()> = Mutex::new(());

fn ensure(bundle: &Bundle) -> Result<()> {
    let _one = DOWNLOADING.lock().expect("never panicked through");
    let target = downloaded(bundle);
    if target.join(DYLIB).is_file() {
        return Ok(());
    }
    let parent = target.parent().expect("under the runtime root");
    let cache = parent.join(format!("{}.downloads", bundle.folder));
    std::fs::create_dir_all(&cache).with_context(|| format!("impossible de créer {}", cache.display()))?;
    let total: u64 = bundle.archives.iter().map(|a| a.size).sum();
    tracing::info!(
        stack = ?bundle.stack,
        mo = total / 1_000_000,
        "téléchargement du runtime de la carte"
    );

    let _over = crate::models::Over;
    let count = bundle.archives.len();
    for (index, archive) in bundle.archives.iter().enumerate() {
        let path = cache.join(archive.name());
        if verified(&path, archive)? {
            continue;
        }
        download(archive, &path, index + 1, count)
            .with_context(|| format!("{} n'a pas pu être téléchargé", archive.name()))?;
    }

    let partial = parent.join(format!("{}.partial", bundle.folder));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial)?;
    for archive in bundle.archives {
        unpack(&cache.join(archive.name()), archive.prefix, &partial)
            .with_context(|| format!("{} n'a pas pu être décompressé", archive.name()))?;
    }
    if !partial.join(DYLIB).is_file() {
        bail!("les paquets ne contiennent pas {DYLIB}");
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&partial, &target)
        .with_context(|| format!("{} n'a pas pu devenir {}", partial.display(), target.display()))?;
    let _ = std::fs::remove_dir_all(&cache);
    Ok(())
}

fn verified(path: &Path, archive: &Archive) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() == archive.size => {}
        _ => return Ok(false),
    }
    let mut hasher = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    Ok(hex(&hasher.finalize()) == archive.sha256)
}

/// Into a `.part` beside `path`, renamed only once its size and sha256 are the
/// pinned ones: what is at `path` is whole.
pub(crate) fn download(archive: &Archive, path: &Path, index: usize, count: usize) -> Result<()> {
    tracing::info!("{index}/{count} : {}", archive.name());
    let response = ureq::get(archive.url).call().map_err(|e| anyhow!("{e}"))?;
    let mut body = response.into_body().into_reader();
    let partial = path.with_extension("part");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&partial)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut received = 0u64;
    let mut told = std::time::Instant::now();
    loop {
        let n = body.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        out.write_all(&buffer[..n])?;
        hasher.update(&buffer[..n]);
        received += n as u64;
        if told.elapsed().as_millis() >= 250 {
            told = std::time::Instant::now();
            crate::models::tell(Some(crate::models::Progress {
                file: archive.name().to_string(),
                index,
                count,
                received,
                size: archive.size,
            }));
        }
    }
    out.flush()?;
    drop(out);
    if received != archive.size {
        bail!("{received} octets reçus sur {}", archive.size);
    }
    let digest = hex(&hasher.finalize());
    if digest != archive.sha256 {
        let _ = std::fs::remove_file(&partial);
        bail!("empreinte {digest} au lieu de {}", archive.sha256);
    }
    std::fs::rename(&partial, path)?;
    Ok(())
}

/// The archive's DLLs under `prefix`, flat in `into` — debug builds and
/// TensorRT's providers left out, nothing here loads them.
fn unpack(archive: &Path, prefix: &str, into: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let full = entry.name().replace('\\', "/");
        if !full.starts_with(prefix) {
            continue;
        }
        let Some(name) = entry.enclosed_name().and_then(|p| p.file_name().map(|n| n.to_owned())) else {
            continue;
        };
        let lower = name.to_string_lossy().to_ascii_lowercase();
        if !lower.ends_with(".dll") || lower.contains("debug") || lower.contains("tensorrt") {
            continue;
        }
        let mut out = std::fs::File::create(into.join(&name))?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_archive_is_pinned() {
        for bundle in [&CUDA, &ONEAPI] {
            for archive in bundle.archives {
                assert!(archive.url.starts_with("https://"), "{}", archive.url);
                assert_eq!(archive.sha256.len(), 64, "{}", archive.name());
                assert!(archive.size > 0);
            }
        }
    }

    #[test]
    fn only_the_makers_with_a_build_have_a_bundle() {
        assert!(bundle(Stack::Cuda).is_some());
        assert!(bundle(Stack::OneApi).is_some());
        assert!(bundle(Stack::Rocm).is_none());
        assert!(bundle(Stack::DirectMl).is_none());
    }

    /// A file that is not the pinned one never reaches its place — the weights
    /// of the French voice go through here too.
    #[test]
    fn only_the_pinned_file_lands() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url: &'static str =
            Box::leak(format!("http://{}/voix.safetensors", server.local_addr().unwrap()).into_boxed_str());
        std::thread::spawn(move || {
            for stream in server.incoming().take(2) {
                let mut stream = stream.unwrap();
                let _ = stream.read(&mut [0u8; 2048]);
                let _ = stream.write_all(b"HTTP/1.1 200 OK
Content-Length: 4
Connection: close

voix");
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voix.safetensors");
        let good: &'static str = Box::leak(hex(&Sha256::digest(b"voix")).into_boxed_str());
        let archive = |sha256| Archive { url, size: 4, sha256, prefix: "" };

        assert!(download(&archive("0000"), &path, 1, 1).is_err());
        assert!(!path.exists());
        download(&archive(good), &path, 1, 1).expect("the pinned one");
        assert_eq!(std::fs::read(&path).unwrap(), b"voix");
    }

    /// The real thing for this machine's card: fetch, unpack, rename.
    ///
    /// ```sh
    /// cargo test -p aria-voice --lib runtime::tests::installs -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "télécharge le runtime de la carte (jusqu'à 1,5 Go)"]
    fn installs_the_cards_runtime() {
        let stack = crate::gpu::card().native.expect("une carte reconnue");
        ensure(bundle(stack).expect("une pile téléchargeable")).expect("installé");
        assert!(installed(stack));
    }

    /// The HTTP path on the smallest archive: received, sized, hashed.
    #[test]
    #[ignore = "réseau"]
    fn downloads_and_checks_the_smallest_archive() {
        let dir = std::env::temp_dir().join("aria-runtime-test");
        std::fs::create_dir_all(&dir).unwrap();
        let archive = &CUDA.archives[1];
        let path = dir.join(archive.name());
        let _ = std::fs::remove_file(&path);
        download(archive, &path, 1, 1).expect("téléchargé");
        assert!(verified(&path, archive).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
