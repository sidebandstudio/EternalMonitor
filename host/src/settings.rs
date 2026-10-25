use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const SETTINGS_FILE_NAME: &str = "settings.json";
const APP_FOLDER: &str = "EternalMonitor";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    pub bitrate_mbps: f32,
    pub target_fps: u32,
    #[serde(default = "default_max_dgram")]
    pub max_dgram: u16,
    #[serde(default)]
    pub target_ip: Option<String>,
    #[serde(default)]
    pub encoder_override: Option<String>,
    #[serde(default)]
    pub encoder_input: crate::encoder::input::EncoderInput,
    #[serde(default = "default_true")]
    pub check_for_updates: bool,
    /// DXGI `DeviceName` of the capture display (e.g. `\\.\DISPLAY3`).
    /// `None` means auto (primary monitor).
    #[serde(default)]
    pub capture_display: Option<String>,
    /// Prefer H.265/HEVC when the connected client can decode it.
    /// Off by default on Windows until verified on real encoder hardware; on
    /// by default on macOS, where the direct VideoToolbox encoder needs HEVC
    /// to reach 120 fps at a legal level.
    #[serde(default = "default_hevc_enabled")]
    pub hevc_enabled: bool,
    #[serde(default = "default_true")]
    pub stream_audio: bool,
    #[serde(default = "default_true")]
    pub require_pairing: bool,
    #[serde(default)]
    pub auth_token_hex: String,
    /// Ask the virtual display driver to offer the connected iPad's native
    /// resolution/refresh (writes vdd_settings.xml before enabling it).
    #[serde(default = "default_true")]
    pub vdd_match_resolution: bool,
    pub start_on_boot: bool,
    /// Streaming sessions of five minutes or more, counted up to the
    /// one-time "How's EternalMonitor working for you?" card.
    #[serde(default)]
    pub feedback_sessions: u32,
    /// The card was answered (Send feedback or Not now), or feedback was
    /// sent another way. It never shows again.
    #[serde(default)]
    pub feedback_ask_done: bool,
    /// The sidebar's GitHub star card was clicked; it says thank you since.
    #[serde(default)]
    pub star_clicked: bool,
    /// The second-screen display mode chosen for each client device, by
    /// `device_id` in hex (`display_profile.rs`). Only the macOS host's
    /// virtual display uses it.
    #[serde(default)]
    pub display_profiles:
        std::collections::BTreeMap<String, crate::display_profile::DisplayProfile>,
    /// macOS: capture the real screen, on a second-screen virtual display
    /// while a client is connected (mirroring the main display otherwise).
    /// Off by default: the macOS host then streams its synthetic test
    /// pattern, and nothing creates a display by itself.
    #[serde(default)]
    pub mac_second_screen: bool,
    /// macOS: how long a second-screen display outlives its client, counted
    /// from the disconnect, so a short disconnect or restart leaves the
    /// user's windows where they are. At least 5 s (`MIN_GRACE`); 0 means
    /// that minimum.
    #[serde(default = "default_second_screen_grace_secs")]
    pub mac_second_screen_grace_secs: u32,
}

fn default_true() -> bool {
    true
}

pub fn default_hevc_enabled() -> bool {
    cfg!(target_os = "macos")
}

fn default_second_screen_grace_secs() -> u32 {
    60
}

fn default_max_dgram() -> u16 {
    eternal_wire::v2::MAX_DGRAM_SIZE as u16
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            bitrate_mbps: 15.0,
            target_fps: 60,
            max_dgram: default_max_dgram(),
            target_ip: None,
            encoder_override: None,
            encoder_input: Default::default(),
            check_for_updates: true,
            capture_display: None,
            hevc_enabled: default_hevc_enabled(),
            stream_audio: true,
            require_pairing: true,
            auth_token_hex: String::new(),
            vdd_match_resolution: true,
            start_on_boot: false,
            feedback_sessions: 0,
            feedback_ask_done: false,
            star_clicked: false,
            display_profiles: Default::default(),
            mac_second_screen: false,
            mac_second_screen_grace_secs: default_second_screen_grace_secs(),
        }
    }
}

impl SettingsFile {
    /// The saved display profile for a client device. Device 0 is a client
    /// that sent no identity, which never has one.
    pub fn display_profile(
        &self,
        device_id: u64,
    ) -> Option<crate::display_profile::DisplayProfile> {
        if device_id == 0 {
            return None;
        }
        self.display_profiles
            .get(&format!("{device_id:016x}"))
            .copied()
    }

    /// Remember `profile` for a client device. Returns whether it changed.
    pub fn remember_display_profile(
        &mut self,
        device_id: u64,
        profile: crate::display_profile::DisplayProfile,
    ) -> bool {
        if device_id == 0 || self.display_profile(device_id) == Some(profile) {
            return false;
        }
        self.display_profiles
            .insert(format!("{device_id:016x}"), profile);
        true
    }

    /// Upgrade old settings once, before networking starts. An invalid or
    /// missing secret is replaced; entropy failures never disable pairing.
    pub fn ensure_auth_token(&mut self) -> Result<[u8; 16], getrandom::Error> {
        if let Some(token) = crate::pairing::parse_token(&self.auth_token_hex) {
            return Ok(token);
        }
        let token = crate::pairing::new_token()?;
        self.auth_token_hex = crate::pairing::token_hex(&token);
        self.save();
        Ok(token)
    }

    /// Load settings from `%APPDATA%/EternalMonitor/settings.json`. On any failure
    /// (missing file, parse error, permission denied) returns `SettingsFile::default()`.
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<SettingsFile>(&text) {
                Ok(settings) => {
                    info!(path = %path.display(), "Loaded settings from disk");
                    settings
                }
                Err(error) => {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "Failed to parse settings.json — using defaults"
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// When settings.json last changed, if it exists: lets a long-running
    /// reader re-load only after a change.
    pub fn modified() -> Option<std::time::SystemTime> {
        fs::metadata(settings_path()?).ok()?.modified().ok()
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                warn!(
                    path = %parent.display(),
                    error = %error,
                    "Failed to create settings directory"
                );
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                // Atomic tmp+rename: a crash mid-write must never leave a
                // truncated settings.json (load() would silently reset every
                // preference to defaults).
                let tmp = path.with_extension("json.tmp");
                let result = fs::write(&tmp, text).and_then(|()| fs::rename(&tmp, &path));
                if let Err(error) = result {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "Failed to write settings.json"
                    );
                }
            }
            Err(error) => {
                warn!(error = %error, "Failed to serialize settings");
            }
        }
    }
}

/// The per-user writable data directory for EternalMonitor (`%APPDATA%/EternalMonitor` on
/// Windows, `~/Library/Application Support/EternalMonitor` elsewhere). Used for settings, the
/// session log, and AMF diagnostics so they keep working when the app is installed read-only
/// under `Program Files`. Returns `None` only if neither `APPDATA` nor `HOME` is set.
pub fn app_data_dir() -> Option<PathBuf> {
    let base = std::env::var("APPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|home| {
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
            })
        })?;
    Some(base.join(APP_FOLDER))
}

fn settings_path() -> Option<PathBuf> {
    app_data_dir().map(|dir| dir.join(SETTINGS_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_default_packet_size_and_new_settings_round_trip() {
        let old = r#"{"bitrate_mbps":15,"target_fps":60,"start_on_boot":false}"#;
        let parsed: SettingsFile = serde_json::from_str(old).unwrap();
        assert!(parsed.require_pairing);
        assert!(parsed.auth_token_hex.is_empty());
        assert!(
            serde_json::from_str::<SettingsFile>(old)
                .unwrap()
                .stream_audio
        );
        assert_eq!(
            serde_json::from_str::<SettingsFile>(old).unwrap().max_dgram,
            1400
        );
        let settings = SettingsFile {
            max_dgram: 1200,
            stream_audio: false,
            require_pairing: false,
            auth_token_hex: "0123456789abcdef0123456789abcdef".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: SettingsFile = serde_json::from_str(&json).unwrap();
        assert!(!parsed.require_pairing);
        assert_eq!(parsed.auth_token_hex, settings.auth_token_hex);
        assert!(
            !serde_json::from_str::<SettingsFile>(&json)
                .unwrap()
                .stream_audio
        );
        assert_eq!(
            serde_json::from_str::<SettingsFile>(&json)
                .unwrap()
                .max_dgram,
            1200
        );
    }

    #[test]
    fn star_and_feedback_ask_persist_and_default_off() {
        let old = r#"{"bitrate_mbps":15,"target_fps":60,"start_on_boot":false}"#;
        let parsed: SettingsFile = serde_json::from_str(old).unwrap();
        assert!(!parsed.star_clicked);
        assert!(!parsed.feedback_ask_done);
        assert_eq!(parsed.feedback_sessions, 0);

        let dir = std::env::temp_dir().join(format!("eternal-star-{}", std::process::id()));
        let path = dir.join(SETTINGS_FILE_NAME);
        fs::create_dir_all(&dir).unwrap();
        let settings = SettingsFile {
            star_clicked: true,
            feedback_sessions: 2,
            feedback_ask_done: true,
            ..Default::default()
        };
        fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
        let parsed: SettingsFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(parsed.star_clicked);
        assert!(parsed.feedback_ask_done);
        assert_eq!(parsed.feedback_sessions, 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn display_profiles_are_kept_per_device_and_survive_a_round_trip() {
        use crate::display_profile::{profile_for, ClientScreen, DisplayProfile};
        let old = r#"{"bitrate_mbps":15,"target_fps":60,"start_on_boot":false}"#;
        let mut settings: SettingsFile = serde_json::from_str(old).unwrap();
        assert!(settings.display_profiles.is_empty());

        // A Duo that first connects folded still saves the inner size.
        let duo = ClientScreen {
            device_id: 0x0123_4567_89ab_cdef,
            screen_px: (1398, 2034),
            refresh_hz: 120,
        };
        let profile = profile_for(duo, settings.display_profile(duo.device_id));
        assert_eq!((profile.width_px, profile.height_px), (2670, 1878));
        assert!(settings.remember_display_profile(duo.device_id, profile));
        assert!(!settings.remember_display_profile(duo.device_id, profile));
        // A client with no identity is never saved.
        assert!(!settings.remember_display_profile(0, profile));
        assert_eq!(settings.display_profile(0), None);

        let ipad = DisplayProfile {
            width_px: 2420,
            height_px: 1668,
            refresh_hz: 120,
            hidpi: true,
        };
        settings.remember_display_profile(7, ipad);
        assert!(!settings.mac_second_screen, "off unless asked for");
        assert_eq!(settings.mac_second_screen_grace_secs, 60);
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"0123456789abcdef\""), "{json}");
        let parsed: SettingsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.display_profile(duo.device_id), Some(profile));
        assert_eq!(parsed.display_profile(7), Some(ipad));
        assert_eq!(parsed.display_profile(8), None);
        // The saved profile is used when the Duo comes back on its inner
        // screen at 60 Hz.
        let back = ClientScreen {
            screen_px: (1878, 2670),
            refresh_hz: 60,
            ..duo
        };
        assert_eq!(
            profile_for(back, parsed.display_profile(back.device_id)),
            profile
        );
    }
}
