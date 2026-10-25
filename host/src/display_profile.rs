//! The host-side display profile saved per client device: the mode a
//! second-screen virtual display is created with for that device.
//!
//! The iPhone Duo is the reason it exists. Its inner screen is 2670×1878
//! landscape and its outer screen 1398×2034, and HELLO2 reports whichever
//! is showing when it connects. The Mac display always takes the inner
//! size, even on a first connection made folded, and keeps it across fold
//! changes: the client letterboxes it onto the outer screen
//! (`fit` below) instead of the host resizing the display, which would
//! move the user's windows (and on macOS 26 may leave a display that can
//! no longer be removed). See `~/Services/fleet/tasks/mac-host/plan-v2.md`,
//! point 5.
//!
//! A saved profile wins over the size the client reports, so a device keeps
//! the display its user last had. Profiles live in their own file,
//! `display-profiles.json` beside `settings.json` ([`ProfileStore`]), so
//! the capture path that saves them never rewrites the user's settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The Duo's inner screen, landscape.
pub const DUO_INNER: (u32, u32) = (2670, 1878);
/// The Duo's outer screen, portrait-native as the client reports it.
pub const DUO_OUTER: (u32, u32) = (1398, 2034);

/// A display mode in pixels. With `hidpi` the desktop is half that in
/// points (a @2x Retina display), so the pixel sizes are kept even.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayProfile {
    pub width_px: u32,
    pub height_px: u32,
    pub refresh_hz: u32,
    pub hidpi: bool,
}

impl DisplayProfile {
    /// The desktop size in points.
    pub fn points(&self) -> (u32, u32) {
        if self.hidpi {
            (self.width_px / 2, self.height_px / 2)
        } else {
            (self.width_px, self.height_px)
        }
    }
}

/// What a client said about its screen in HELLO2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientScreen {
    pub device_id: u64,
    pub screen_px: (u16, u16),
    pub refresh_hz: u8,
}

/// Whether a reported screen is either of the Duo's, in either orientation.
pub fn is_duo_screen(screen_px: (u16, u16)) -> bool {
    let (w, h) = (u32::from(screen_px.0), u32::from(screen_px.1));
    [DUO_INNER, DUO_OUTER]
        .iter()
        .any(|&(a, b)| (w, h) == (a, b) || (w, h) == (b, a))
}

/// The mode to create for `client`: its saved profile if it has one, the
/// Duo's inner size for a Duo whichever screen it reports, and otherwise
/// the client's own panel in landscape. The refresh is the client's (60
/// when it reports none); 120 Hz is best effort, so the display always
/// offers 60 too (`fallback_refresh`).
pub fn profile_for(client: ClientScreen, saved: Option<DisplayProfile>) -> DisplayProfile {
    if let Some(saved) = saved {
        return saved;
    }
    let refresh_hz = match client.refresh_hz {
        0 => 60,
        hz => u32::from(hz),
    };
    let (width_px, height_px) = if is_duo_screen(client.screen_px) {
        DUO_INNER
    } else {
        let (w, h) = (u32::from(client.screen_px.0), u32::from(client.screen_px.1));
        (w.max(h) & !1, w.min(h) & !1)
    };
    DisplayProfile {
        width_px,
        height_px,
        refresh_hz,
        hidpi: true,
    }
}

/// The rate every display also offers, so a 120 Hz mode that does not hold
/// has a 60 Hz one beside it.
pub fn fallback_refresh(profile: DisplayProfile) -> Option<u32> {
    (profile.refresh_hz != 60).then_some(60)
}

/// How the client shows a display whose shape differs from its screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitMode {
    /// All of the desktop, with bars.
    Fit,
    /// The whole screen, cropping the desktop.
    Fill,
}

/// Where a `content` picture lands on a `screen`, both in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub scale: f64,
    /// Unused screen, summed over both sides, on each axis (Fit).
    pub bars: (f64, f64),
    /// Desktop pixels cut off, summed over both sides, on each axis (Fill).
    pub cropped: (f64, f64),
}

pub fn fit(content: (u32, u32), screen: (u32, u32), mode: FitMode) -> Placement {
    let (cw, ch) = (f64::from(content.0), f64::from(content.1));
    let (sw, sh) = (f64::from(screen.0), f64::from(screen.1));
    let (sx, sy) = (sw / cw, sh / ch);
    let scale = match mode {
        FitMode::Fit => sx.min(sy),
        FitMode::Fill => sx.max(sy),
    };
    let (shown_w, shown_h) = (cw * scale, ch * scale);
    Placement {
        scale,
        bars: ((sw - shown_w).max(0.0), (sh - shown_h).max(0.0)),
        cropped: (
            ((shown_w - sw) / scale).max(0.0),
            ((shown_h - sh) / scale).max(0.0),
        ),
    }
}

/// The saved profiles, by `device_id` in hex.
type Profiles = BTreeMap<String, DisplayProfile>;

/// `display-profiles.json`: read when it changes, written atomically
/// (temporary file, then rename). The capture path is its only writer.
pub struct ProfileStore {
    path: Option<PathBuf>,
    cache: Profiles,
    seen: Option<Option<SystemTime>>,
}

/// Serialises writers in this process.
static WRITE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn key(device_id: u64) -> String {
    format!("{device_id:016x}")
}

fn read(path: &Path) -> Result<Profiles, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Profiles::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

impl ProfileStore {
    /// The store beside `settings.json`.
    pub fn beside_settings() -> Self {
        Self::at(crate::settings::app_data_dir().map(|dir| dir.join("display-profiles.json")))
    }

    pub fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            cache: Profiles::new(),
            seen: None,
        }
    }

    /// Re-reads the file if it changed. A file that cannot be read or
    /// parsed leaves the last good profiles in place.
    fn refresh(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        let modified = modified(path);
        if self.seen == Some(modified) {
            return;
        }
        match read(path) {
            Ok(profiles) => {
                self.cache = profiles;
                self.seen = Some(modified);
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Could not read the saved display profiles");
            }
        }
    }

    /// The saved profile for a client device. Device 0 is a client that
    /// sent no identity, which never has one.
    pub fn get(&mut self, device_id: u64) -> Option<DisplayProfile> {
        if device_id == 0 {
            return None;
        }
        self.refresh();
        self.cache.get(&key(device_id)).copied()
    }

    /// Saves `profile` for a client device if it differs. Returns whether
    /// it was written.
    pub fn remember(&mut self, device_id: u64, profile: DisplayProfile) -> bool {
        if device_id == 0 || self.get(device_id) == Some(profile) {
            return false;
        }
        let Some(path) = self.path.clone() else {
            return false;
        };
        let _write = WRITE.lock();
        // Start from what is on disk now, not the cache.
        let mut profiles = match read(&path) {
            Ok(profiles) => profiles,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Not overwriting display profiles that cannot be read");
                return false;
            }
        };
        profiles.insert(key(device_id), profile);
        let written = serde_json::to_string_pretty(&profiles)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, text)
                    .and_then(|()| std::fs::rename(&tmp, &path))
                    .map_err(|error| error.to_string())
            });
        match written {
            Ok(()) => {
                self.cache = profiles;
                self.seen = Some(modified(&path));
                true
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Could not save the display profile");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(screen_px: (u16, u16), refresh_hz: u8) -> ClientScreen {
        ClientScreen {
            device_id: 0xd0,
            screen_px,
            refresh_hz,
        }
    }

    const INNER: DisplayProfile = DisplayProfile {
        width_px: 2670,
        height_px: 1878,
        refresh_hz: 120,
        hidpi: true,
    };

    #[test]
    fn a_duo_gets_the_inner_size_whichever_screen_it_connects_on() {
        for screen in [(1878, 2670), (2670, 1878), (1398, 2034), (2034, 1398)] {
            assert_eq!(profile_for(client(screen, 120), None), INNER, "{screen:?}");
        }
        assert_eq!(INNER.points(), (1335, 939));
        assert_eq!(fallback_refresh(INNER), Some(60));
    }

    #[test]
    fn an_ipad_gets_its_panel_in_landscape() {
        let profile = profile_for(client((1668, 2420), 120), None);
        assert_eq!(
            profile,
            DisplayProfile {
                width_px: 2420,
                height_px: 1668,
                refresh_hz: 120,
                hidpi: true
            }
        );
        assert_eq!(profile.points(), (1210, 834));
        // Odd panels round down to even pixels; no refresh means 60.
        let odd = profile_for(client((1621, 2161), 0), None);
        assert_eq!(
            (odd.width_px, odd.height_px, odd.refresh_hz),
            (2160, 1620, 60)
        );
        assert_eq!(fallback_refresh(odd), None);
    }

    #[test]
    fn a_saved_profile_wins_over_what_the_client_reports() {
        let saved = DisplayProfile {
            width_px: 2034,
            height_px: 1398,
            refresh_hz: 60,
            hidpi: true,
        };
        assert_eq!(profile_for(client((1878, 2670), 120), Some(saved)), saved);
    }

    #[test]
    fn the_inner_display_on_the_folded_outer_screen() {
        // Landscape outer screen: about 47 px of side bars.
        let landscape = fit(DUO_INNER, (2034, 1398), FitMode::Fit);
        assert!((landscape.bars.0 - 46.4).abs() < 0.5, "{landscape:?}");
        assert_eq!(landscape.bars.1, 0.0);
        // Portrait outer screen: about 1,051 px of bars above and below.
        let portrait = fit(DUO_INNER, DUO_OUTER, FitMode::Fit);
        assert!((portrait.bars.1 - 1050.7).abs() < 0.5, "{portrait:?}");
        assert_eq!(portrait.bars.0, 0.0);
        // Fill on the portrait outer screen crops most of the desktop's
        // width: only 1398 / (2034 / 1878) ≈ 1291 of 2670 px stay visible.
        let fill = fit(DUO_INNER, DUO_OUTER, FitMode::Fill);
        assert_eq!(fill.bars, (0.0, 0.0));
        assert!((fill.cropped.0 - (2670.0 - 1290.8)).abs() < 0.5, "{fill:?}");
        assert_eq!(fill.cropped.1, 0.0);
        // On the inner screen itself nothing is lost either way.
        for mode in [FitMode::Fit, FitMode::Fill] {
            let same = fit(DUO_INNER, DUO_INNER, mode);
            assert_eq!(
                (same.scale, same.bars, same.cropped),
                (1.0, (0.0, 0.0), (0.0, 0.0))
            );
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "eternal-profiles-{name}-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn profiles_are_kept_per_device_in_their_own_file() {
        let dir = temp_dir("store");
        let path = dir.join("display-profiles.json");
        let mut store = ProfileStore::at(Some(path.clone()));
        let duo = ClientScreen {
            device_id: 0x0123_4567_89ab_cdef,
            screen_px: (1398, 2034),
            refresh_hz: 120,
        };
        // A Duo that first connects folded still saves the inner size.
        let profile = profile_for(duo, store.get(duo.device_id));
        assert_eq!((profile.width_px, profile.height_px), (2670, 1878));
        assert!(store.remember(duo.device_id, profile));
        assert!(!store.remember(duo.device_id, profile));
        // A client with no identity is never saved.
        assert!(!store.remember(0, profile));
        assert_eq!(store.get(0), None);
        let ipad = DisplayProfile {
            width_px: 2420,
            height_px: 1668,
            refresh_hz: 120,
            hidpi: true,
        };
        store.remember(7, ipad);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"0123456789abcdef\""), "{text}");
        assert!(!dir.join("display-profiles.json.tmp").exists());
        // Another reader sees both, and the Duo coming back on its inner
        // screen at 60 Hz keeps its saved profile.
        let mut reader = ProfileStore::at(Some(path.clone()));
        assert_eq!(reader.get(duo.device_id), Some(profile));
        assert_eq!(reader.get(7), Some(ipad));
        assert_eq!(reader.get(8), None);
        let back = ClientScreen {
            screen_px: (1878, 2670),
            refresh_hz: 60,
            ..duo
        };
        assert_eq!(profile_for(back, reader.get(back.device_id)), profile);
        // A damaged file keeps the last good profiles and is not overwritten.
        std::fs::write(&path, "{\"0123").unwrap();
        assert_eq!(reader.get(7), Some(ipad));
        assert!(!reader.remember(9, ipad));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"0123");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_the_duos_two_screens_count_as_a_duo() {
        assert!(is_duo_screen((2670, 1878)));
        assert!(is_duo_screen((2034, 1398)));
        assert!(!is_duo_screen((2420, 1668)));
        assert!(!is_duo_screen((2670, 1398)));
    }
}
