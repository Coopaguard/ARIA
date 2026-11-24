//! Tauri's build, preceded by the one thing the installer cannot do without:
//! the DirectML ONNX Runtime, in `resources/ort/` (decision 131).
//!
//! ONNX Runtime is loaded at start-up from a DLL. The card's own stack (CUDA,
//! OpenVINO) is downloaded by ARIA when it is wanted; **DirectML is shipped**,
//! because it is the fallback on every machine. `bundle.resources` puts this
//! folder beside the executable, and `tauri dev` copies it under `target/`.
//!
//! Pinned and checked like the runtime downloads (`aria_voice::runtime`), and
//! cached in `target/ort-cache` so a rebuild does not fetch 200 MB again.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

struct Package {
    url: &'static str,
    sha256: &'static str,
    /// Entry in the zip → file name in `resources/ort`.
    take: &'static [(&'static str, &'static str)],
}

const PACKAGES: &[Package] = &[
    Package {
        url: "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg",
        sha256: "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681",
        take: &[
            ("runtimes/win-x64/native/onnxruntime.dll", "onnxruntime.dll"),
            (
                "runtimes/win-x64/native/onnxruntime_providers_shared.dll",
                "onnxruntime_providers_shared.dll",
            ),
        ],
    },
    // The version ORT DirectML 1.24.4 declares in its nuspec. Not the system's
    // own `System32\DirectML.dll`, which is older.
    Package {
        url: "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg",
        sha256: "4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9",
        take: &[("bin/x64-win/DirectML.dll", "DirectML.dll")],
    },
];

fn main() {
    let into = Path::new("resources/ort");
    std::fs::create_dir_all(into).expect("resources/ort");
    if cfg!(windows) {
        let cache = target_dir().join("ort-cache");
        std::fs::create_dir_all(&cache).expect("target/ort-cache");
        for package in PACKAGES {
            if package.take.iter().all(|(_, name)| into.join(name).is_file()) {
                continue;
            }
            let archive = fetch(package, &cache);
            unpack(&archive, package, into);
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    tauri_build::build()
}

/// `target/`, from `OUT_DIR` = `target/<profile>/build/<crate>/out`.
fn target_dir() -> PathBuf {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    out.ancestors().nth(4).expect("target/<profile>/build/<crate>/out").to_path_buf()
}

fn fetch(package: &Package, cache: &Path) -> PathBuf {
    let name = package.url.rsplit('/').next().expect("a file name");
    let path = cache.join(name);
    if path.is_file() && sha256(&std::fs::read(&path).expect("readable cache")) == package.sha256 {
        return path;
    }
    println!("cargo:warning=téléchargement de {name}");
    let mut bytes = Vec::new();
    ureq::get(package.url)
        .call()
        .unwrap_or_else(|e| panic!("{name} : {e}"))
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .unwrap_or_else(|e| panic!("{name} : {e}"));
    let digest = sha256(&bytes);
    assert_eq!(digest, package.sha256, "{name} : empreinte inattendue");
    std::fs::write(&path, &bytes).expect("cache writable");
    path
}

fn unpack(archive: &Path, package: &Package, into: &Path) {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).expect("archive")).expect("a zip");
    for (entry, name) in package.take {
        let mut file = zip.by_name(entry).unwrap_or_else(|_| panic!("{entry} absent"));
        let partial = into.join(format!("{name}.part"));
        let mut out = std::fs::File::create(&partial).expect("resources/ort writable");
        std::io::copy(&mut file, &mut out).expect("unpacked");
        drop(out);
        std::fs::rename(&partial, into.join(name)).expect("renamed");
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
