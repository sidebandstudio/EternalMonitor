use tracing::{info, warn};
use windows::core::Interface;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ERROR_NOT_FOUND,
};

const VENDOR_NVIDIA: u32 = 0x10DE;
const VENDOR_AMD: u32 = 0x1002;
const VENDOR_INTEL: u32 = 0x8086;

/// Software adapter flag (DXGI_ADAPTER_FLAG_SOFTWARE = 2).
const FLAG_SOFTWARE: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Unknown,
}

impl GpuVendor {
    fn from_vendor_id(id: u32) -> Self {
        match id {
            VENDOR_NVIDIA => Self::Nvidia,
            VENDOR_AMD => Self::Amd,
            VENDOR_INTEL => Self::Intel,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for GpuVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nvidia => write!(f, "NVIDIA"),
            Self::Amd => write!(f, "AMD"),
            Self::Intel => write!(f, "Intel"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    pub name: String,
    pub adapter_index: u32,
    pub dedicated_vram_mb: u64,
    pub encoder_name: String,
    pub codec_display_name: String,
}

impl GpuInfo {
    /// Detect the best GPU via DXGI and resolve the preferred encoder.
    /// Falls back to software encoding if detection fails.
    pub fn detect() -> Self {
        match detect_from_dxgi() {
            Ok(info) => info,
            Err(e) => {
                warn!(error = %e, "DXGI GPU detection failed, falling back to software encoding");
                Self::software_fallback()
            }
        }
    }

    pub fn software_fallback() -> Self {
        Self {
            vendor: GpuVendor::Unknown,
            name: "Software".to_string(),
            adapter_index: 0,
            dedicated_vram_mb: 0,
            encoder_name: "libx264".to_string(),
            codec_display_name: "H.264 (x264)".to_string(),
        }
    }
}

/// Vendors of the hardware adapters present now. Read at each encoder
/// fallback rather than at launch, so a card connected since (an eGPU, a
/// dock) or a driver installed since comes first. None unless DXGI listed
/// and described every adapter.
pub fn vendors_present() -> Option<Vec<GpuVendor>> {
    let factory = match unsafe { CreateDXGIFactory1::<IDXGIFactory1>() } {
        Ok(factory) => factory,
        Err(error) => {
            info!(%error, "Could not list the graphics cards");
            return None;
        }
    };
    vendors_listed(|index| {
        let adapter = unsafe { factory.EnumAdapters1(index) }?;
        let desc = unsafe { adapter.GetDesc1() }?;
        Ok((desc.VendorId, desc.Flags))
    })
}

/// The vendors of the adapters `adapter` describes (vendor ID and flags) by
/// index until DXGI_ERROR_NOT_FOUND, without software adapters or repeats.
/// None on any other error: a list missing a card that is there would put
/// that card's encoder behind ones the PC may not have.
fn vendors_listed(
    mut adapter: impl FnMut(u32) -> windows::core::Result<(u32, u32)>,
) -> Option<Vec<GpuVendor>> {
    let mut vendors = Vec::new();
    for index in 0u32.. {
        match adapter(index) {
            Ok((vendor_id, flags)) => {
                let vendor = GpuVendor::from_vendor_id(vendor_id);
                if flags & FLAG_SOFTWARE == 0 && !vendors.contains(&vendor) {
                    vendors.push(vendor);
                }
            }
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => {
                info!(adapter_index = index, %error, "Could not list every graphics card");
                return None;
            }
        }
    }
    Some(vendors)
}

/// Windows driver versions of the NVIDIA adapters, packed as DXGI reports
/// them (32.0.15.6607). Read when NVENC refuses to open, to tell an old
/// driver from other refusals.
pub fn nvidia_driver_versions() -> Vec<u64> {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut versions = Vec::new();
    let mut index = 0u32;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.VendorId != VENDOR_NVIDIA {
            continue;
        }
        if let Ok(version) = unsafe {
            adapter.CheckInterfaceSupport(&windows::Win32::Graphics::Dxgi::IDXGIDevice::IID)
        } {
            versions.push(version as u64);
        }
    }
    versions
}

fn detect_from_dxgi() -> Result<GpuInfo, Box<dyn std::error::Error>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };

    let mut best: Option<(u32, String, GpuVendor, u64)> = None;
    let mut index = 0u32;

    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(index) } {
            Ok(a) => a,
            Err(_) => break,
        };

        let desc = unsafe { adapter.GetDesc1()? };

        // Skip software adapters (e.g. Microsoft Basic Render Driver)
        if desc.Flags & FLAG_SOFTWARE != 0 {
            index += 1;
            continue;
        }

        let name = String::from_utf16_lossy(
            &desc
                .Description
                .iter()
                .copied()
                .take_while(|&c| c != 0)
                .collect::<Vec<_>>(),
        );
        crate::telemetry::note_hardware(&name);
        let vendor = GpuVendor::from_vendor_id(desc.VendorId);
        let vram_mb = desc.DedicatedVideoMemory as u64 / (1024 * 1024);
        let driver_version = match unsafe {
            adapter.CheckInterfaceSupport(&windows::Win32::Graphics::Dxgi::IDXGIDevice::IID)
        } {
            Ok(version) => {
                let version = version as u64;
                format!(
                    "{}.{}.{}.{}",
                    version >> 48,
                    (version >> 32) & 0xffff,
                    (version >> 16) & 0xffff,
                    version & 0xffff
                )
            }
            Err(error) => format!("unavailable: {error}"),
        };

        info!(
            adapter_index = index,
            name = %name,
            vendor = %vendor,
            vram_mb,
            vendor_id = desc.VendorId,
            device_id = desc.DeviceId,
            subsystem_id = desc.SubSysId,
            revision = desc.Revision,
            shared_memory_mb = desc.SharedSystemMemory / (1024 * 1024),
            driver_version,
            "Found GPU adapter"
        );

        let is_better = best
            .as_ref()
            .is_none_or(|(_, _, _, best_vram)| vram_mb > *best_vram);
        if is_better {
            best = Some((index, name, vendor, vram_mb));
        }

        index += 1;
    }

    let (adapter_index, name, vendor, vram_mb) =
        best.ok_or("No non-software GPU adapters found via DXGI")?;

    let (encoder_name, codec_display_name) = resolve_encoder(vendor);

    Ok(GpuInfo {
        vendor,
        name,
        adapter_index,
        dedicated_vram_mb: vram_mb,
        encoder_name,
        codec_display_name,
    })
}

/// The names of the PC's graphics cards, as Windows lists them, without the
/// software renderer or repeats. Empty if DXGI can't be asked.
pub fn adapter_names() -> Vec<String> {
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    let mut index = 0u32;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.Flags & FLAG_SOFTWARE != 0 {
            continue;
        }
        let name = String::from_utf16_lossy(
            &desc
                .Description
                .iter()
                .copied()
                .take_while(|&c| c != 0)
                .collect::<Vec<_>>(),
        )
        .trim()
        .to_string();
        crate::telemetry::note_hardware(&name);
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Walk a vendor-preferred fallback chain and return the first encoder
/// that is compiled into the linked FFmpeg build.
fn resolve_encoder(vendor: GpuVendor) -> (String, String) {
    let chain: &[(&str, &str)] = match vendor {
        GpuVendor::Nvidia => &[
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_amf", "H.264 (AMF)"),
            ("h264_qsv", "H.264 (QSV)"),
            ("libx264", "H.264 (x264)"),
        ],
        GpuVendor::Amd => &[
            ("h264_amf", "H.264 (AMF)"),
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_qsv", "H.264 (QSV)"),
            ("libx264", "H.264 (x264)"),
        ],
        GpuVendor::Intel => &[
            ("h264_qsv", "H.264 (QSV)"),
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_amf", "H.264 (AMF)"),
            ("libx264", "H.264 (x264)"),
        ],
        GpuVendor::Unknown => &[
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_amf", "H.264 (AMF)"),
            ("h264_qsv", "H.264 (QSV)"),
            ("libx264", "H.264 (x264)"),
        ],
    };

    for (name, display_name) in chain {
        if ffmpeg_next::encoder::find_by_name(name).is_some() {
            info!(encoder = name, codec = display_name, "Selected encoder");
            return (name.to_string(), display_name.to_string());
        }
        info!(encoder = name, "Encoder not available, trying next");
    }

    warn!("No hardware or software encoder found in FFmpeg — defaulting to libx264");
    ("libx264".to_string(), "H.264 (x264)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::E_FAIL;
    use windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_REMOVED;

    /// `vendors_listed` over these adapters (vendor ID and flags, or the
    /// error describing it gave), then DXGI_ERROR_NOT_FOUND.
    fn listed(adapters: &[windows::core::Result<(u32, u32)>]) -> Option<Vec<GpuVendor>> {
        vendors_listed(|index| match adapters.get(index as usize) {
            Some(adapter) => adapter.clone(),
            None => Err(DXGI_ERROR_NOT_FOUND.into()),
        })
    }

    #[test]
    fn every_adapter_is_listed_or_there_is_no_list() {
        use GpuVendor::{Amd, Intel, Nvidia, Unknown};
        // RTX + Radeon iGPU, the basic render driver skipped, repeats once.
        assert_eq!(
            listed(&[
                Ok((VENDOR_NVIDIA, 0)),
                Ok((VENDOR_AMD, 0)),
                Ok((VENDOR_NVIDIA, 0)),
                Ok((0x1414, FLAG_SOFTWARE)),
            ]),
            Some(vec![Nvidia, Amd])
        );
        assert_eq!(listed(&[]), Some(vec![]));
        // NVIDIA described, then Intel's description failed: no list, not
        // [NVIDIA].
        assert_eq!(listed(&[Ok((VENDOR_NVIDIA, 0)), Err(E_FAIL.into())]), None);
        // The adapter list itself failed after one adapter.
        assert_eq!(
            listed(&[Ok((VENDOR_INTEL, 0)), Err(DXGI_ERROR_DEVICE_REMOVED.into())]),
            None
        );
        // A vendor ID with no encoder here is listed as unknown.
        assert_eq!(
            listed(&[Ok((0x1414, 0)), Ok((VENDOR_INTEL, 0))]),
            Some(vec![Unknown, Intel])
        );
    }
}
