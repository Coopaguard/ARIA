//! Which graphics card is there, and so which stack can run on it.
//!
//! Asked for on 2026-11-22: the settings show the maker of the card that was
//! found, and the stack follows from it — CUDA for NVIDIA, ROCm for AMD,
//! oneAPI for Intel. With none of them, DirectML, and the switch is disabled.
//!
//! **CUDA runs; oneAPI is wired but not offered; ROCm is not wired**: ONNX
//! Runtime is loaded from a DLL per stack (`runtime.rs`, decision 131), AMD has
//! no Windows build at all, and Intel's does not read Parakeet
//! ([`Stack::available`]). Those cards run on DirectML, and [`Stack`] says
//! which one is meant and which one runs, rather than pretending.
//!
//! Read from DXGI, the list DirectX chooses from: the adapter with the most
//! dedicated memory among the three makers wins, which is the discrete card on
//! a laptop that has two. Microsoft's software adapter (vendor `0x1414`) is
//! never a card.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
}

impl Vendor {
    fn from_pci(id: u32) -> Option<Self> {
        match id {
            0x10DE => Some(Self::Nvidia),
            0x1002 | 0x1022 => Some(Self::Amd),
            0x8086 => Some(Self::Intel),
            _ => None,
        }
    }

    /// The maker's own stack.
    pub fn native(self) -> Stack {
        match self {
            Self::Nvidia => Stack::Cuda,
            Self::Amd => Stack::Rocm,
            Self::Intel => Stack::OneApi,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stack {
    Cuda,
    Rocm,
    #[serde(rename = "oneapi")]
    OneApi,
    DirectMl,
}

impl Stack {
    /// Whether this build can run it.
    ///
    /// oneAPI is wired (`runtime.rs`) but **not offered**, since 2026-11-27: on
    /// an Iris Xe, Intel's OpenVINO 1.24.1 refused Parakeet outright — « OpenVINO
    /// does not support the following ONNX operations: Attention » — and the
    /// ear was gone. DirectML had been running it on the same card. Back on
    /// the day an OpenVINO build reads the model.
    pub fn available(self) -> bool {
        matches!(self, Self::Cuda | Self::DirectMl)
    }
}

/// The card found, as the settings show it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Card {
    pub vendor: Option<Vendor>,
    /// What the driver calls it, `NVIDIA GeForce RTX 5070`. Empty without one.
    pub name: String,
    /// The maker's stack, which may not be available yet.
    pub native: Option<Stack>,
    /// What actually runs: the native stack when available, DirectML otherwise.
    pub runs: Stack,
}

impl Card {
    fn from(vendor: Option<Vendor>, name: String) -> Self {
        let native = vendor.map(Vendor::native);
        let runs = native.filter(|s| s.available()).unwrap_or(Stack::DirectMl);
        Self {
            vendor,
            name,
            native,
            runs,
        }
    }

    /// A card from a known maker was found — what enables the switch.
    pub fn recognised(&self) -> bool {
        self.vendor.is_some()
    }
}

/// Whether the ear computes on the card, given the setting.
///
/// With no card from a known maker the switch is disabled and DirectML is the
/// fallback, so the setting is not consulted.
pub fn wanted(setting: bool) -> bool {
    setting || !card().recognised()
}

/// The card, asked once per process: adapters do not come and go.
pub fn card() -> &'static Card {
    static CARD: std::sync::OnceLock<Card> = std::sync::OnceLock::new();
    CARD.get_or_init(|| {
        let card = detect();
        tracing::info!(?card, "carte graphique");
        card
    })
}

#[cfg(windows)]
fn detect() -> Card {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

    let mut best: Option<(u64, Vendor, String)> = None;
    // SAFETY: plain COM calls on objects owned by this function.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return Card::from(None, String::new());
        };
        let mut index = 0;
        while let Ok(adapter) = factory.EnumAdapters1(index) {
            index += 1;
            let Ok(desc) = adapter.GetDesc1() else { continue };
            let Some(vendor) = Vendor::from_pci(desc.VendorId) else { continue };
            let memory = desc.DedicatedVideoMemory as u64;
            let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..end]).trim().to_string();
            if best.as_ref().is_none_or(|(held, ..)| memory > *held) {
                best = Some((memory, vendor, name));
            }
        }
    }
    match best {
        Some((_, vendor, name)) => Card::from(Some(vendor), name),
        None => Card::from(None, String::new()),
    }
}

#[cfg(not(windows))]
fn detect() -> Card {
    Card::from(None, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_has_its_stack_and_amd_and_intel_fall_back() {
        assert_eq!(Card::from(Some(Vendor::Nvidia), String::new()).runs, Stack::Cuda);
        let amd = Card::from(Some(Vendor::Amd), String::new());
        assert_eq!((amd.native, amd.runs), (Some(Stack::Rocm), Stack::DirectMl));
        let intel = Card::from(Some(Vendor::Intel), String::new());
        assert_eq!((intel.native, intel.runs), (Some(Stack::OneApi), Stack::DirectMl));
        let none = Card::from(None, String::new());
        assert!(!none.recognised());
        assert_eq!(none.runs, Stack::DirectMl);
    }

    #[test]
    fn the_software_adapter_is_not_a_card() {
        assert_eq!(Vendor::from_pci(0x1414), None);
    }

    #[test]
    fn this_machine_answers() {
        // Not asserted: CI machines have no card. Printed, to read.
        println!("{:?}", card());
    }
}
