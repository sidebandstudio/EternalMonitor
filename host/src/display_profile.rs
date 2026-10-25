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
//! The display owner reads and saves them through [`ProfileIo`], a thread
//! of their own, so it never waits on the disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::RwLock;

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
/// (`settings::replace_file`) under a lock every process shares
/// ([`lock`]). Only explicit edits change a saved
/// profile ([`Self::remember`]); the display owner only adds one for a
/// device seen for the first time ([`Self::save_if_absent`]).
pub struct ProfileStore {
    path: Option<PathBuf>,
    cache: Profiles,
    seen: Option<Option<SystemTime>>,
}

/// How long an explicit edit waits for another writer, in any process, to
/// finish. A first-time save never waits ([`ProfileStore::save_if_absent`]).
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// The lock every writer of `path`, in any process, holds from reading the
/// file to renaming the new one over it: `flock` on macOS and Linux,
/// `LockFileEx` on Windows, on the sibling `<name>.lock` file. Released
/// when the returned file is dropped. `Ok(None)` if another writer still
/// holds it after `wait`.
fn lock(path: &Path, wait: Duration) -> std::io::Result<Option<std::fs::File>> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_file_name(format!("{name}.lock")))?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

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

    /// An explicit edit: saves `profile` for a client device if it
    /// differs. Returns whether it was written.
    pub fn remember(&mut self, device_id: u64, profile: DisplayProfile) -> bool {
        if device_id == 0 || self.get(device_id) == Some(profile) {
            return false;
        }
        self.write(device_id, profile, true, LOCK_WAIT)
    }

    /// Saves `profile` for a device that has no saved profile, never
    /// replacing one: the file is read again under the write lock, and an
    /// entry found there (saved since this store last read it, by any
    /// process) is kept. Returns whether it was written. It tries the lock
    /// once and never waits: if another writer holds it, nothing is
    /// written, and the caller tries again later.
    pub fn save_if_absent(&mut self, device_id: u64, profile: DisplayProfile) -> bool {
        if device_id == 0 || self.get(device_id).is_some() {
            return false;
        }
        self.write(device_id, profile, false, Duration::ZERO)
    }

    fn write(
        &mut self,
        device_id: u64,
        profile: DisplayProfile,
        replace: bool,
        wait: Duration,
    ) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                tracing::warn!(path = %parent.display(), %error, "Could not make the display profiles' directory");
                return false;
            }
        }
        let _lock = match lock(&path, wait) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                tracing::warn!(path = %path.display(), "Display profiles are busy; not saving this time");
                return false;
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Could not lock the display profiles");
                return false;
            }
        };
        // Start from what is on disk now, not the cache.
        let mut profiles = match read(&path) {
            Ok(profiles) => profiles,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Not overwriting display profiles that cannot be read");
                return false;
            }
        };
        if !replace && profiles.contains_key(&key(device_id)) {
            self.cache = profiles;
            self.seen = Some(modified(&path));
            return false;
        }
        profiles.insert(key(device_id), profile);
        let written = serde_json::to_string_pretty(&profiles)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                crate::settings::replace_file(&path, &text).map_err(|error| error.to_string())
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

/// How often the profile thread looks for edits made by any process.
const RELOAD_POLL: Duration = Duration::from_millis(200);

/// The saved profiles as the profile thread last read or wrote them. Reading
/// it touches no file.
#[derive(Clone, Default)]
pub struct ProfileView(Arc<RwLock<Profiles>>);

impl ProfileView {
    /// The saved profile for a client device, as [`ProfileStore::get`].
    pub fn get(&self, device_id: u64) -> Option<DisplayProfile> {
        if device_id == 0 {
            return None;
        }
        self.0.read().get(&key(device_id)).copied()
    }

    fn publish(&self, profiles: &Profiles) {
        if *self.0.read() != *profiles {
            *self.0.write() = profiles.clone();
        }
    }
}

/// A thread that makes every disk access to a [`ProfileStore`]: it re-reads
/// the file when it changes (every `RELOAD_POLL`), makes the first-time
/// saves it is asked for, and publishes what it has in a [`ProfileView`].
/// It stops when this handle is dropped.
pub struct ProfileIo {
    view: ProfileView,
    saves: mpsc::Sender<(u64, DisplayProfile)>,
}

impl ProfileIo {
    /// Starts the thread over `store`. The file is read once before this
    /// returns, so the first answer made from the view already has the
    /// saved profiles.
    pub fn spawn(mut store: ProfileStore) -> std::io::Result<Self> {
        let view = ProfileView::default();
        store.refresh();
        view.publish(&store.cache);
        let (saves, asked) = mpsc::channel();
        let published = view.clone();
        std::thread::Builder::new()
            .name("display-profiles".into())
            .spawn(move || loop {
                match asked.recv_timeout(RELOAD_POLL) {
                    Ok((device_id, profile)) => {
                        store.save_if_absent(device_id, profile);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => store.refresh(),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                published.publish(&store.cache);
            })?;
        Ok(Self { view, saves })
    }

    pub fn view(&self) -> ProfileView {
        self.view.clone()
    }

    /// Asks the thread for [`ProfileStore::save_if_absent`] and returns at
    /// once. Whether it saved shows in the view: a device with a profile
    /// there has one on disk.
    pub fn save_if_absent(&self, device_id: u64, profile: DisplayProfile) {
        let _ = self.saves.send((device_id, profile));
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
        // Path-safe on every platform: no Debug output in the name.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "eternal-profiles-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".tmp")),
            "{names:?}"
        );
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
    fn racing_first_saves_for_a_device_keep_one_entry_and_never_overwrite() {
        let dir = temp_dir("first-save");
        let path = dir.join("display-profiles.json");
        let profile = |width_px| DisplayProfile {
            width_px,
            height_px: 1878,
            refresh_hz: 120,
            hidpi: true,
        };
        // Two stores that both read the file before either saved.
        let mut early = ProfileStore::at(Some(path.clone()));
        let mut late = ProfileStore::at(Some(path.clone()));
        assert_eq!((early.get(0xd0), late.get(0xd0)), (None, None));
        assert!(early.save_if_absent(0xd0, profile(2670)));
        assert!(!late.save_if_absent(0xd0, profile(2034)), "found on disk");
        assert_eq!(late.get(0xd0), Some(profile(2670)));
        // An explicit edit is never undone by a first-time save.
        assert!(late.remember(0xd0, profile(2034)));
        assert!(!early.save_if_absent(0xd0, profile(2670)));
        assert_eq!(
            ProfileStore::at(Some(path.clone())).get(0xd0),
            Some(profile(2034))
        );
        // Many threads racing for a new device: exactly one writes.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let written: usize = (0..8u32)
            .map(|n| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let mut store = ProfileStore::at(Some(path));
                    assert_eq!(store.get(7), None);
                    barrier.wait();
                    store.save_if_absent(7, profile(2000 + n * 2))
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|racer| usize::from(racer.join().unwrap()))
            .sum();
        assert_eq!(written, 1);
        let on_disk: Profiles =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.len(), 2);
        assert_eq!(on_disk[&key(0xd0)], profile(2034));
        std::fs::remove_dir_all(dir).unwrap();
    }

    const EDITOR_DIR: &str = "EM_PROFILE_EDITOR_DIR";
    const RACES: u64 = 50;
    const EDIT: DisplayProfile = DisplayProfile {
        width_px: 2034,
        height_px: 1398,
        refresh_hz: 120,
        hidpi: true,
    };

    /// Waits for `path` to exist, spinning so a race stays tight.
    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "{} never appeared",
                path.display()
            );
            std::thread::yield_now();
        }
    }

    /// Not a test on its own: the other process of
    /// `an_edit_in_another_process_always_wins_over_a_first_save`, an editor
    /// that saves `EDIT` for device `n` as soon as it sees `go-n`.
    #[test]
    fn profile_editor_child() {
        let Some(dir) = std::env::var_os(EDITOR_DIR).map(PathBuf::from) else {
            return;
        };
        for device in 1..=RACES {
            wait_for(&dir.join(format!("go-{device}")));
            let mut editor = ProfileStore::at(Some(dir.join("display-profiles.json")));
            assert!(
                editor.remember(device, EDIT),
                "the edit of {device} was not written"
            );
            std::fs::write(dir.join(format!("done-{device}")), "").unwrap();
        }
    }

    /// The editor process, killed if the race fails so it does not wait on.
    struct Editor(std::process::Child);

    impl Drop for Editor {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn an_edit_in_another_process_always_wins_over_a_first_save() {
        let dir = temp_dir("two-processes");
        let path = dir.join("display-profiles.json");
        let mut editor = Editor(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "display_profile::tests::profile_editor_child",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(EDITOR_DIR, &dir)
                .spawn()
                .unwrap(),
        );
        let default = DisplayProfile {
            width_px: 2670,
            ..EDIT
        };
        let mut host = ProfileStore::at(Some(path.clone()));
        for device in 1..=RACES {
            // Both see no entry for `device`; they race to write it.
            assert_eq!(host.get(device), None);
            std::fs::write(dir.join(format!("go-{device}")), "").unwrap();
            host.save_if_absent(device, default);
            wait_for(&dir.join(format!("done-{device}")));
            let on_disk: Profiles = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .expect("the file always parses");
            assert_eq!(on_disk.get(&key(device)), Some(&EDIT), "race {device}");
            assert_eq!(on_disk.len() as u64, device);
        }
        assert!(
            editor.0.wait().unwrap().success(),
            "the editor process failed"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".tmp")),
            "{names:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_busy_lock_skips_the_first_save_until_the_next_attempt() {
        let dir = temp_dir("busy-lock");
        let path = dir.join("display-profiles.json");
        let held = lock(&path, Duration::ZERO)
            .unwrap()
            .expect("the lock is free");
        let mut host = ProfileStore::at(Some(path.clone()));
        let started = Instant::now();
        assert!(
            !host.save_if_absent(0xd0, EDIT),
            "skipped while another writer holds it"
        );
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "a first save never waits: {:?}",
            started.elapsed()
        );
        assert!(!path.exists());
        // An explicit edit waits for the lock, but only so long.
        let started = Instant::now();
        assert!(!host.remember(7, EDIT));
        assert!(started.elapsed() >= LOCK_WAIT);
        assert!(
            started.elapsed() < LOCK_WAIT + Duration::from_secs(2),
            "bounded"
        );
        assert!(!path.exists());
        drop(held);
        assert!(host.save_if_absent(0xd0, EDIT), "the next attempt saves");
        assert_eq!(ProfileStore::at(Some(path)).get(0xd0), Some(EDIT));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Waits for `view` to show `want` for `device_id`.
    fn until_viewed(view: &ProfileView, device_id: u64, want: Option<DisplayProfile>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while view.get(device_id) != want {
            assert!(
                Instant::now() < deadline,
                "{device_id} is {:?}",
                view.get(device_id)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_profile_thread_publishes_saved_edited_and_first_saved_profiles() {
        let dir = temp_dir("profile-thread");
        let path = dir.join("display-profiles.json");
        let default = DisplayProfile {
            width_px: 2670,
            ..EDIT
        };
        assert!(ProfileStore::at(Some(path.clone())).remember(7, EDIT));
        // Read before it starts: the view has what was saved already.
        let io = ProfileIo::spawn(ProfileStore::at(Some(path.clone()))).unwrap();
        let view = io.view();
        assert_eq!(
            (view.get(7), view.get(0xd0), view.get(0)),
            (Some(EDIT), None, None)
        );
        // A first save, asked for without waiting, shows once it is made.
        let asked = Instant::now();
        io.save_if_absent(0xd0, default);
        assert!(asked.elapsed() < Duration::from_millis(5));
        until_viewed(&view, 0xd0, Some(default));
        assert_eq!(
            ProfileStore::at(Some(path.clone())).get(0xd0),
            Some(default)
        );
        // An edit by another writer shows within a reload, and a later
        // first save for that device changes nothing.
        std::thread::sleep(Duration::from_millis(20));
        assert!(ProfileStore::at(Some(path.clone())).remember(0xd0, EDIT));
        until_viewed(&view, 0xd0, Some(EDIT));
        io.save_if_absent(0xd0, default);
        io.save_if_absent(0, default);
        std::thread::sleep(RELOAD_POLL * 2);
        assert_eq!(view.get(0xd0), Some(EDIT));
        assert_eq!(ProfileStore::at(Some(path)).get(0xd0), Some(EDIT));
        drop(io);
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
