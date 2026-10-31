use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Off by default until verified on real encoder hardware.
    #[serde(default)]
    pub hevc_enabled: bool,
    #[serde(default = "default_true")]
    pub stream_audio: bool,
    #[serde(default = "default_true")]
    pub require_pairing: bool,
    #[serde(default)]
    pub auth_token_hex: String,
    /// Troubleshooting: which network the stream listens on.
    #[serde(default)]
    pub listen_on: crate::network::ListenOn,
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
    /// Troubleshooting: how the window is drawn, from the next start.
    #[serde(default)]
    pub window_drawing: crate::troubleshooting::WindowDrawing,
    /// A graphics card switch made from the Stream page, for its outcome
    /// after the relaunch and for Undo.
    #[serde(default)]
    pub gpu_switch: Option<crate::gpu_preference::SwitchRecord>,
    /// The window's language: a code such as `de`, `system` for the
    /// Windows display language, or none until one is picked in the app,
    /// when the installer's choice applies (see `i18n::resolve`).
    #[serde(default)]
    pub language: Option<String>,
}

fn default_true() -> bool {
    true
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
            hevc_enabled: false,
            stream_audio: true,
            require_pairing: true,
            auth_token_hex: String::new(),
            listen_on: Default::default(),
            vdd_match_resolution: true,
            start_on_boot: false,
            feedback_sessions: 0,
            feedback_ask_done: false,
            star_clicked: false,
            window_drawing: Default::default(),
            gpu_switch: None,
            language: None,
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
        let _ = self.save();
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

    /// Writes settings.json atomically. Failures are logged here too;
    /// callers that can go on without the file ignore the error.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = settings_path() else {
            warn!("Failed to save settings: no folder for settings (APPDATA and HOME unset)");
            return Err("no folder for settings".into());
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                warn!(
                    path = %parent.display(),
                    error = %error,
                    "Failed to create settings directory"
                );
                return Err(error.to_string());
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
                    return Err(error.to_string());
                }
                Ok(())
            }
            Err(error) => {
                warn!(error = %error, "Failed to serialize settings");
                Err(error.to_string())
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

static FRESH_INSTALL: AtomicBool = AtomicBool::new(false);

/// Notes whether this is a fresh install, when the window opens first-run
/// setup. Call first thing at launch: the single-instance check, the
/// session log and diagnostics create the settings folder and files in it.
pub fn note_launch() {
    let fresh = app_data_dir().is_some_and(|dir| holds_nothing(&dir));
    FRESH_INSTALL.store(fresh, Ordering::SeqCst);
}

/// See [`note_launch`]. False when it was never called (tests).
pub fn fresh_install() -> bool {
    FRESH_INSTALL.load(Ordering::SeqCst)
}

/// A fresh install has no settings folder yet, or an empty one. Anything in
/// it is from an earlier run, so this is an upgrade, with or without
/// settings.json: versions before 0.3.0 saved it only once a setting
/// changed, but every version since 0.1.2 keeps its session log in `logs`
/// there. A folder that cannot be read counts as an upgrade too.
fn holds_nothing(dir: &Path) -> bool {
    match fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
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
    fn language_defaults_to_unset_and_round_trips() {
        let old = r#"{"bitrate_mbps":15,"target_fps":60,"start_on_boot":false}"#;
        let parsed: SettingsFile = serde_json::from_str(old).unwrap();
        assert_eq!(parsed.language, None);
        let settings = SettingsFile {
            language: Some("pt-BR".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let parsed: SettingsFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.language.as_deref(), Some("pt-BR"));
    }

    #[test]
    fn only_a_missing_or_empty_settings_folder_is_a_fresh_install() {
        let root = std::env::temp_dir().join(format!("eternal-fresh-{}", std::process::id()));
        let dir = root.join(APP_FOLDER);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert!(holds_nothing(&dir), "no folder");
        fs::create_dir_all(&dir).unwrap();
        assert!(holds_nothing(&dir), "an empty folder");

        // Only a session log, as an older version that never saved settings
        // leaves it.
        fs::create_dir_all(dir.join("logs")).unwrap();
        fs::write(dir.join("logs").join("session.log"), "earlier run\n").unwrap();
        assert!(!holds_nothing(&dir), "logs only");
        fs::remove_dir_all(dir.join("logs")).unwrap();

        fs::write(dir.join(SETTINGS_FILE_NAME), "{ not json").unwrap();
        assert!(!holds_nothing(&dir), "malformed settings.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
            assert!(!holds_nothing(&dir), "a folder that cannot be read");
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
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
}
