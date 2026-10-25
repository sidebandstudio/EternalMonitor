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

/// The last settings each file was read with successfully, by path.
static LAST_GOOD: parking_lot::Mutex<std::collections::BTreeMap<PathBuf, SettingsFile>> =
    parking_lot::Mutex::new(std::collections::BTreeMap::new());

fn remember_good(path: &std::path::Path, settings: &SettingsFile) {
    LAST_GOOD
        .lock()
        .insert(path.to_path_buf(), settings.clone());
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
            mac_second_screen: false,
            mac_second_screen_grace_secs: default_second_screen_grace_secs(),
        }
    }
}

impl SettingsFile {
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
        Self::load_from(&path)
    }

    /// Reads a settings file strictly: an error instead of defaults when it
    /// cannot be read or parsed, so a live reader can keep what it had. A
    /// missing file is the defaults.
    pub fn read(path: &std::path::Path) -> Result<Self, String> {
        let settings = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error.to_string()),
        };
        remember_good(path, &settings);
        Ok(settings)
    }

    /// The last settings `path` was read with successfully in this process.
    pub fn last_good(path: &std::path::Path) -> Option<Self> {
        LAST_GOOD.lock().get(path).cloned()
    }

    /// The settings a save carries over for the fields its caller does not
    /// edit: the file as it is, or, while it cannot be read (a partial or
    /// broken write by another program), the last values it was read with.
    /// Defaults only if it was never readable in this process.
    pub fn current_or_last_good(path: &std::path::Path) -> Self {
        Self::read(path).unwrap_or_else(|error| {
            warn!(
                path = %path.display(),
                %error,
                "settings.json cannot be read; keeping its last good values"
            );
            Self::last_good(path).unwrap_or_default()
        })
    }

    /// `load` from a given file.
    pub fn load_from(path: &std::path::Path) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<SettingsFile>(&text) {
                Ok(settings) => {
                    info!(path = %path.display(), "Loaded settings from disk");
                    remember_good(path, &settings);
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

    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        self.save_to(&path);
    }

    /// `save` to a given file.
    pub fn save_to(&self, path: &std::path::Path) {
        let path = path.to_path_buf();
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

pub fn settings_path() -> Option<PathBuf> {
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
    fn second_screen_is_off_by_default_with_a_minute_of_grace() {
        let old = r#"{"bitrate_mbps":15,"target_fps":60,"start_on_boot":false}"#;
        let parsed: SettingsFile = serde_json::from_str(old).unwrap();
        assert!(!parsed.mac_second_screen, "off unless asked for");
        assert_eq!(parsed.mac_second_screen_grace_secs, 60);
    }

    fn temp_settings(name: &str) -> (PathBuf, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "eternal-settings-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE_NAME);
        (dir, path)
    }

    #[test]
    fn a_gui_save_over_a_broken_file_keeps_the_hosts_own_fields() {
        let (dir, path) = temp_settings("gui-save");
        SettingsFile {
            mac_second_screen: true,
            mac_second_screen_grace_secs: 42,
            ..Default::default()
        }
        .save_to(&path);
        // The GUI reads the file at startup.
        assert!(SettingsFile::load_from(&path).mac_second_screen);
        // Another program leaves it half-written.
        fs::write(&path, "{\"mac_second_screen\": tr").unwrap();
        // A GUI save carries the fields it does not edit over, as
        // `persist_settings` does, from the last good values.
        let on_disk = SettingsFile::current_or_last_good(&path);
        SettingsFile {
            bitrate_mbps: 20.0,
            mac_second_screen: on_disk.mac_second_screen,
            mac_second_screen_grace_secs: on_disk.mac_second_screen_grace_secs,
            ..Default::default()
        }
        .save_to(&path);
        let saved = SettingsFile::read(&path).unwrap();
        assert!(saved.mac_second_screen, "turned off by a GUI save");
        assert_eq!(saved.mac_second_screen_grace_secs, 42);
        assert_eq!(saved.bitrate_mbps, 20.0);
        // A readable file is carried over as it is.
        fs::write(
            &path,
            r#"{"bitrate_mbps":15,"target_fps":60,"start_on_boot":false,"mac_second_screen_grace_secs":7}"#,
        )
        .unwrap();
        let on_disk = SettingsFile::current_or_last_good(&path);
        assert!(!on_disk.mac_second_screen);
        assert_eq!(on_disk.mac_second_screen_grace_secs, 7);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_never_read_falls_back_to_defaults_and_last_good_is_per_file() {
        let (dir, path) = temp_settings("never-read");
        fs::write(&path, "not json").unwrap();
        assert!(SettingsFile::read(&path).is_err());
        assert_eq!(
            SettingsFile::last_good(&path).map(|s| s.mac_second_screen),
            None
        );
        assert!(!SettingsFile::current_or_last_good(&path).mac_second_screen);
        let (other_dir, other) = temp_settings("other");
        SettingsFile {
            mac_second_screen: true,
            ..Default::default()
        }
        .save_to(&other);
        assert!(SettingsFile::read(&other).unwrap().mac_second_screen);
        assert!(
            SettingsFile::last_good(&path).is_none(),
            "another file's values never leak"
        );
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(other_dir).unwrap();
    }

    #[test]
    fn a_leftover_display_profiles_key_is_ignored_and_dropped_on_save() {
        // Profiles lived in settings.json on this branch for a few hours
        // (unreleased); they are not migrated, and the key must do no harm.
        let (dir, path) = temp_settings("leftover");
        fs::write(
            &path,
            r#"{"bitrate_mbps":15,"target_fps":60,"start_on_boot":false,
                "display_profiles":{"00000000000000d0":{"width_px":2034,"height_px":1398,"refresh_hz":60,"hidpi":true}},
                "mac_second_screen":true,"mac_second_screen_grace_secs":30}"#,
        )
        .unwrap();
        let settings = SettingsFile::read(&path).expect("the strict reader accepts it");
        assert!(settings.mac_second_screen);
        assert_eq!(settings.mac_second_screen_grace_secs, 30);
        assert!(SettingsFile::load_from(&path).mac_second_screen);
        assert!(SettingsFile::current_or_last_good(&path).mac_second_screen);
        // The saved profile is not used: the profile store beside it has
        // none for that device.
        let mut profiles =
            crate::display_profile::ProfileStore::at(Some(dir.join("display-profiles.json")));
        assert_eq!(profiles.get(0xd0), None);
        // The next save writes the file without it.
        settings.save_to(&path);
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("display_profiles"), "{text}");
        assert!(SettingsFile::read(&path).unwrap().mac_second_screen);
        fs::remove_dir_all(dir).unwrap();
    }
}
