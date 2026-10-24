//! Non-Windows GPU/encoder selection. There is no DXGI here; the "adapter" is
//! whatever the OS gives us. macOS always encodes with VideoToolbox, directly
//! (`encoder::videotoolbox`) unless FFmpeg's wrapper is asked for, and never
//! falls back to libx264: the Mac build links an LGPL FFmpeg. Other systems
//! (CI) try FFmpeg's VideoToolbox name, then libx264.

use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Unknown,
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
    /// Probe the platform encoder chain and describe the result.
    pub fn detect() -> Self {
        if cfg!(target_os = "macos") {
            info!("Selected the VideoToolbox encoder");
            return Self {
                vendor: GpuVendor::Unknown,
                name: std::env::consts::OS.to_string(),
                adapter_index: 0,
                dedicated_vram_mb: 0,
                // The FFmpeg loop's name, used with ETERNAL_MAC_ENCODER=ffmpeg.
                encoder_name: "h264_videotoolbox".to_string(),
                codec_display_name: "VideoToolbox".to_string(),
            };
        }
        let chain: &[(&str, &str)] = &[
            ("h264_videotoolbox", "H.264 (VideoToolbox)"),
            ("libx264", "H.264 (x264)"),
        ];

        for (name, display_name) in chain {
            if ffmpeg_next::encoder::find_by_name(name).is_some() {
                info!(encoder = name, codec = display_name, "Selected encoder");
                return Self {
                    vendor: GpuVendor::Unknown,
                    name: std::env::consts::OS.to_string(),
                    adapter_index: 0,
                    dedicated_vram_mb: 0,
                    encoder_name: name.to_string(),
                    codec_display_name: display_name.to_string(),
                };
            }
            info!(encoder = name, "Encoder not available, trying next");
        }

        warn!("No encoder found in FFmpeg — defaulting to libx264");
        Self::software_fallback()
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

/// No graphics card names off Windows; a feedback email leaves the line out.
pub fn adapter_names() -> Vec<String> {
    Vec::new()
}
