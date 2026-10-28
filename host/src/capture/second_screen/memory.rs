//! A second screen that could not start, remembered across app runs.
//!
//! On some PCs the virtual display never attaches or never shows a picture
//! (field reports: laptops with Intel and NVIDIA graphics). 0.3.3 remembered
//! that only until the app quit, so every app run spent the 30 s attempt
//! again and turned the driver on and off once. The failure is now saved
//! beside the settings with what decides it: this app's version, the virtual
//! display driver's version, the graphics cards with their driver versions,
//! and the Windows build. While those stay the same, Second screen sessions
//! start in the mirror and the host tries again after [`BACKOFF`]. A second
//! screen that shows a picture, a change in any of them, Try again, or
//! another mode forgets the failure.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::warn;

#[cfg(any(windows, test))]
pub(super) const FILE_NAME: &str = "second-screen-state.json";

/// The file's layout. Another value is ignored, like an unreadable file.
const FORMAT: u32 = 1;

const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;

/// How long after the 1st, 2nd, … consecutive failure the host tries again;
/// after the last, every 7 days. A try costs one driver toggle (the PC's
/// screens blink and Windows plays its device sound) and up to 30 s on the
/// mirror, which the iPad shows meanwhile. The first wait is short because
/// one failure may pass: Windows still setting up its displays after a boot
/// or an update, or a display state that signing in again clears. A PC that
/// fails again is most likely one where it never works, so the wait grows to
/// a day, then a week: a few toggles a month at most. What usually fixes it,
/// a graphics driver, Windows or EternalMonitor update, changes the
/// fingerprint and tries again at once.
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(HOUR),
    Duration::from_secs(6 * HOUR),
    Duration::from_secs(DAY),
    Duration::from_secs(3 * DAY),
    Duration::from_secs(7 * DAY),
];

/// The failures worth remembering: the driver was turned on and Windows gave
/// no usable display. A failed Enable task is not one: its notice already
/// says how to repair the install, and it costs no wait.
const REMEMBERED: [&str; 3] = ["attach_timeout", "no_frames", "duplication_failed"];

/// What decides whether the second screen can work on this PC. Every value
/// is one the session log already holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Fingerprint {
    pub app: String,
    pub display_driver: String,
    pub windows: String,
    /// Each graphics card's name and driver version, sorted.
    pub graphics: Vec<String>,
}

impl Fingerprint {
    /// This PC as it is now. Read before this app run turns the display on.
    #[cfg(windows)]
    pub(crate) fn current() -> Self {
        let os = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
        let mut graphics: Vec<String> = crate::vdd_snapshot::adapters()
            .into_iter()
            .map(|adapter| format!("{} {}", adapter.description.trim(), adapter.driver))
            .collect();
        graphics.sort();
        Self {
            app: env!("CARGO_PKG_VERSION").to_string(),
            display_driver: crate::vdd_settings::driver_version()
                .unwrap_or_else(|| "unknown".to_string()),
            windows: format!(
                "{}.{}",
                crate::diagnostics::registry_string(os, "CurrentBuildNumber"),
                crate::diagnostics::registry_dword(os, "UBR")
            ),
            graphics,
        }
    }

    /// Which parts differ from `other`, for the log.
    #[cfg(any(windows, test))]
    fn changes(&self, other: &Self) -> Vec<&'static str> {
        [
            ("app", self.app != other.app),
            (
                "display_driver",
                self.display_driver != other.display_driver,
            ),
            ("windows", self.windows != other.windows),
            ("graphics", self.graphics != other.graphics),
        ]
        .into_iter()
        .filter_map(|(name, changed)| changed.then_some(name))
        .collect()
    }
}

/// The saved file. Without a failure it says there is none, for when the
/// file could not be removed.
#[derive(Debug, Serialize, Deserialize)]
struct File {
    format: u32,
    failure: Option<Record>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    fingerprint: Fingerprint,
    reason: String,
    /// Consecutive failed attempts with this fingerprint.
    failures: u32,
    /// When the last one failed, in seconds since 1970.
    failed_at: u64,
}

/// Connections mirror without trying: why, and when the host tries again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hold {
    pub reason: &'static str,
    pub failures: u32,
    pub retry_in: Duration,
}

#[derive(Debug, Clone, Copy)]
struct Failed {
    reason: &'static str,
    failures: u32,
    failed_at: u64,
}

/// Where the failure is saved and what it is for. Without one, as in unit
/// tests and off Windows, nothing is read, saved or held.
#[derive(Debug)]
struct Store {
    path: PathBuf,
    fingerprint: Fingerprint,
    /// The wall clock, in seconds since 1970, at `since`.
    wall: u64,
    since: Instant,
    io: Io,
}

/// The file operations, so tests can make them fail.
#[derive(Debug, Clone, Copy)]
struct Io {
    remove: fn(&Path) -> std::io::Result<()>,
    replace: fn(&Path, &[u8]) -> std::io::Result<()>,
    overwrite: fn(&Path, &[u8]) -> std::io::Result<()>,
}

#[cfg(any(windows, test))]
impl Io {
    const FILES: Io = Io {
        remove: remove_file,
        replace: replace_file,
        overwrite: overwrite_file,
    };
}

/// How often a forgotten failure that could not be cleared from the file is
/// tried again, besides at each success and Try again.
const CLEAR_RETRY: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub(crate) struct Memory {
    store: Option<Store>,
    failed: Option<Failed>,
    /// The try after the saved failure's wait failed for a reason that
    /// passes. This app run then mirrors until Try again, as after any such
    /// failure, instead of trying again at each connection. The saved
    /// failure and its count stay for the next app run.
    excused: bool,
    /// A forgotten failure is still in the file, and when clearing it was
    /// last tried: removing, replacing and overwriting it all failed. This app
    /// run treats it as cleared and tries again at the next success or Try
    /// again, or a minute later; the next app run may hold until the wait
    /// ends or the user clicks Try again.
    uncleared: Option<Instant>,
    /// Clearing failed and was logged once in this app run.
    clear_failure_logged: bool,
}

impl Memory {
    /// Read the failure saved at `path`, if it is for `fingerprint`. `wall`
    /// is the time at `now` in seconds since 1970; later times count from
    /// `now` on the monotonic clock.
    #[cfg(any(windows, test))]
    pub(crate) fn load(path: PathBuf, fingerprint: Fingerprint, wall: u64, now: Instant) -> Self {
        let failed = match read(&path) {
            Ok(None) => None,
            Err(error) => {
                warn!(%error, "Ignoring an unreadable second screen record");
                let _ = remove_file(&path);
                None
            }
            Ok(Some((record, _))) if record.fingerprint != fingerprint => {
                tracing::info!(
                    reason = %record.reason,
                    changed = %record.fingerprint.changes(&fingerprint).join(","),
                    "The second screen failed on this PC before, with another driver, Windows \
                     build or app version — trying it again"
                );
                let _ = remove_file(&path);
                None
            }
            Ok(Some((record, reason))) => Some(Failed {
                reason,
                failures: record.failures,
                failed_at: record.failed_at,
            }),
        };
        let memory = Self {
            store: Some(Store {
                path,
                fingerprint,
                wall,
                since: now,
                io: Io::FILES,
            }),
            failed,
            excused: false,
            uncleared: None,
            clear_failure_logged: false,
        };
        if let Some(failed) = memory.failed {
            tracing::info!(
                reason = failed.reason,
                failures = failed.failures,
                retry_in_s = memory.hold(now).map_or(0, |hold| hold.retry_in.as_secs()),
                "The second screen failed on this PC before"
            );
        }
        memory
    }

    fn wall(&self, now: Instant) -> Option<u64> {
        let store = self.store.as_ref()?;
        Some(
            store
                .wall
                .saturating_add(now.saturating_duration_since(store.since).as_secs()),
        )
    }

    /// The saved failure, while its wait lasts. A clock set back before the
    /// failure, or a time past the end of the clock, tries again rather than
    /// waiting longer.
    pub(crate) fn hold(&self, now: Instant) -> Option<Hold> {
        let failed = self.failed?;
        let wall = self.wall(now)?;
        let retry_at = failed
            .failed_at
            .checked_add(backoff(failed.failures).as_secs())?;
        (failed.failed_at..retry_at).contains(&wall).then(|| Hold {
            reason: failed.reason,
            failures: failed.failures,
            retry_in: Duration::from_secs(retry_at - wall),
        })
    }

    /// Whether a failure is saved, held or due, and not
    /// [`Memory::excused`].
    pub(crate) fn saved(&self) -> bool {
        self.failed.is_some() && !self.excused
    }

    /// The try after the wait failed for a reason that passes; see
    /// [`Memory::excused`].
    pub(crate) fn excuse(&mut self) {
        self.excused = true;
    }

    /// An attempt failed for `reason`. Saves it when it is one to remember,
    /// counting it after the failures before, and returns the wait.
    pub(crate) fn note_failure(&mut self, reason: &'static str, now: Instant) -> Option<Hold> {
        let reason = remembered(reason)?;
        let failed_at = self.wall(now)?;
        let store = self.store.as_ref()?;
        let failures = self.failed.map_or(0, |f| f.failures).saturating_add(1);
        match store.write(Some(Record {
            fingerprint: store.fingerprint.clone(),
            reason: reason.to_string(),
            failures,
            failed_at,
        })) {
            Ok(()) => self.uncleared = None,
            Err(error) => warn!(%error, "Could not save the second screen record"),
        }
        self.excused = false;
        self.failed = Some(Failed {
            reason,
            failures,
            failed_at,
        });
        self.hold(now)
    }

    /// Forget the saved failure. Whether there was one. A file that can't
    /// be removed (another program holds it) is replaced by one that says
    /// there is no failure, or overwritten in place with that, so the next
    /// app run doesn't hold either. When none works, see
    /// [`Memory::uncleared`]. `now` paces those retries; `success` (a picture
    /// or Try again) retries at once.
    pub(crate) fn forget(&mut self, now: Instant, success: bool) -> bool {
        let forgot = self.failed.take().is_some();
        self.excused = false;
        if forgot || (success && self.uncleared.is_some()) {
            self.clear(now);
        } else {
            self.retry_clear(now);
        }
        forgot
    }

    /// Try again to clear a forgotten failure the file still holds, a
    /// minute after the last try. The transport calls this at every tick,
    /// whatever the mode and however the second screen is doing.
    pub(crate) fn retry_clear(&mut self, now: Instant) {
        if self
            .uncleared
            .is_some_and(|tried| now.saturating_duration_since(tried) >= CLEAR_RETRY)
        {
            self.clear(now);
        }
    }

    fn clear(&mut self, now: Instant) {
        let Some(store) = &self.store else {
            return;
        };
        match store.clear() {
            Ok(()) => self.uncleared = None,
            Err(error) => {
                if !std::mem::replace(&mut self.clear_failure_logged, true) {
                    warn!(
                        %error,
                        "Could not clear the second screen record — this app run treats it \
                         as cleared; the next may wait until it tries again or Try again is \
                         clicked"
                    );
                }
                self.uncleared = Some(now);
            }
        }
    }
}

impl Store {
    /// Replace the file at once, as the settings file is: a crash leaves the
    /// old record or the new one.
    fn write(&self, failure: Option<Record>) -> std::io::Result<()> {
        self.path.parent().map_or(Ok(()), std::fs::create_dir_all)?;
        (self.io.replace)(&self.path, &encode(failure)?)
    }

    /// Leave no failure in the file: remove it; else replace it with one that
    /// says there is none; else, when a handle another program holds allows
    /// writing but not deleting, overwrite it in place. Torn by a crash, that
    /// reads as unusable, which also means no failure. The last error.
    fn clear(&self) -> std::io::Result<()> {
        match (self.io.remove)(&self.path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {}
            _ => return Ok(()),
        }
        let empty = encode(None)?;
        (self.io.replace)(&self.path, &empty).or_else(|_| (self.io.overwrite)(&self.path, &empty))
    }
}

fn encode(failure: Option<Record>) -> std::io::Result<Vec<u8>> {
    let file = File {
        format: FORMAT,
        failure,
    };
    serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)
}

fn backoff(failures: u32) -> Duration {
    let index = (failures.max(1) as usize - 1).min(BACKOFF.len() - 1);
    BACKOFF[index]
}

fn remembered(reason: &str) -> Option<&'static str> {
    REMEMBERED.into_iter().find(|known| *known == reason)
}

/// The saved record and its reason. `Ok(None)` without a file or without a
/// failure in it; an error for one that cannot be used.
#[cfg(any(windows, test))]
fn read(path: &Path) -> Result<Option<(Record, &'static str)>, String> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let file: File = serde_json::from_slice(&data).map_err(|error| error.to_string())?;
    if file.format != FORMAT {
        return Err(format!("format {}", file.format));
    }
    let Some(record) = file.failure else {
        return Ok(None);
    };
    if record.failures == 0 {
        return Err("no failures".to_string());
    }
    let reason = remembered(&record.reason).ok_or_else(|| format!("reason {}", record.reason))?;
    Ok(Some((record, reason)))
}

#[cfg(any(windows, test))]
fn remove_file(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

/// Through a temporary file and a rename.
#[cfg(any(windows, test))]
fn replace_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path))
}

/// The existing file, truncated and written where it is.
#[cfg(any(windows, test))]
fn overwrite_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(data)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::super::{
        attach, next_step, note_attached, note_connected, note_image, remembered_failure,
        retries_later, start_proving, Driver, Step, PROVE_TIMEOUT,
    };
    use super::*;
    use crate::capture::tests::connect_viewer;
    use crate::capture::OutputInfo;
    use crate::control::{CaptureTarget, SharedControl, VddStatus};
    use crate::transport::link::LinkId;
    use std::cell::Cell;

    /// 2027-01-15, an arbitrary wall clock for the first app run.
    const T0: u64 = 1_800_000_000;
    const ATTACH_TIMEOUT: Duration =
        Duration::from_secs(crate::capture::VDD_ATTACH_TIMEOUT_DEFAULT_SECS);

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "eternal-second-screen-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join(FILE_NAME)
    }

    /// A laptop with Intel and NVIDIA graphics.
    fn laptop() -> Fingerprint {
        Fingerprint {
            app: "0.3.3".to_string(),
            display_driver: "25.7.23".to_string(),
            windows: "26100.2894".to_string(),
            graphics: vec![
                "Intel(R) UHD Graphics 31.0.101.4502".to_string(),
                "NVIDIA GeForce RTX 3050 Laptop GPU 32.0.15.6094".to_string(),
            ],
        }
    }

    /// An app run that starts `after` the first one, on the same PC.
    fn app_run(path: &Path, after: Duration) -> (Memory, Instant) {
        let start = Instant::now();
        let memory = Memory::load(path.to_path_buf(), laptop(), T0 + after.as_secs(), start);
        (memory, start)
    }

    fn hours(hours: u64) -> Duration {
        Duration::from_secs(hours * HOUR)
    }

    fn second_screen_with(memory: Memory) -> SharedControl {
        let shared = SharedControl::new(9876, 15_000_000);
        shared
            .running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
        *shared.vdd_status.lock() = VddStatus::WaitingForClient;
        shared.second_screen.lock().memory = memory;
        shared
    }

    fn reconnect(shared: &SharedControl, now: Instant) {
        assert!(shared.session.lock().link_lost(LinkId::Udp).client_lost);
        connect_viewer(shared);
        note_connected(shared, now);
    }

    fn saved(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// A display that never attaches, on a paused clock.
    struct NeverAttaches {
        start: Instant,
        elapsed: Cell<Duration>,
        enables: Cell<u32>,
    }

    impl NeverAttaches {
        fn new(start: Instant) -> Self {
            Self {
                start,
                elapsed: Cell::new(Duration::ZERO),
                enables: Cell::new(0),
            }
        }
    }

    impl Driver for NeverAttaches {
        fn outputs(&self) -> Vec<OutputInfo> {
            vec![OutputInfo {
                adapter_index: 0,
                output_index: 0,
                device_name: "primary".to_string(),
                width: 1920,
                height: 1080,
                left: 0,
                top: 0,
                is_primary: true,
                adapter_name: "Test GPU".to_string(),
            }]
        }

        fn prepare(&self, _outputs: &[OutputInfo]) {}

        fn set_enabled(&self, enabled: bool) -> bool {
            if enabled {
                self.enables.set(self.enables.get() + 1);
            }
            true
        }

        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }

        fn sleep(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
        }
    }

    /// The field case: the display never attaches. The next app run mirrors
    /// at once, without the driver or the 30 s wait, says why once per
    /// connection, and tries again after the wait.
    #[test]
    fn a_display_that_never_attaches_is_remembered_by_the_next_app_run() {
        let path = temp_file("never");
        let (memory, start) = app_run(&path, Duration::ZERO);
        assert_eq!(memory.hold(start), None, "nothing saved yet");
        let shared = second_screen_with(memory);
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, start), Step::Attach);
        let driver = NeverAttaches::new(start);
        attach(&shared, &driver, ATTACH_TIMEOUT);
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
        assert_eq!(saved(&path)["failure"]["reason"], "attach_timeout");
        assert_eq!(saved(&path)["failure"]["failures"], 1);
        assert_eq!(
            saved(&path)["failure"]["fingerprint"]["display_driver"],
            "25.7.23"
        );
        assert!(retries_later(&shared));

        // The next app run, ten minutes later.
        let (memory, start) = app_run(&path, Duration::from_secs(600));
        let shared = second_screen_with(memory);
        connect_viewer(&shared);
        note_connected(&shared, start);
        assert_eq!(next_step(&shared, start), Step::Nothing, "no Enable");
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
        assert_eq!(remembered_failure(&shared), Some("attach_timeout"));
        assert!(retries_later(&shared), "the notice says it tries again");
        let hold = shared.second_screen.lock().memory.hold(start).unwrap();
        assert_eq!(hold.failures, 1);
        // Saved when the 30 s wait ended, an hour before the next try.
        assert_eq!(
            hold.retry_in,
            hours(1) + ATTACH_TIMEOUT - Duration::from_secs(600)
        );

        // Reconnects during the wait stay on the mirror.
        for minute in 1..50 {
            let now = start + Duration::from_secs(60 * minute);
            reconnect(&shared, now);
            assert_eq!(next_step(&shared, now), Step::Nothing);
        }
        // After it, the next connection tries again, and failing again
        // doubles down on the wait.
        let later = start + hours(1);
        reconnect(&shared, later);
        assert_eq!(next_step(&shared, later), Step::Attach);
        let driver = NeverAttaches::new(later);
        attach(&shared, &driver, ATTACH_TIMEOUT);
        assert_eq!(driver.enables.get(), 1);
        assert_eq!(saved(&path)["failure"]["failures"], 2);
        let hold = shared.second_screen.lock().memory.hold(driver.now());
        assert_eq!(hold.map(|h| h.retry_in), Some(hours(6)));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_second_screen_without_a_picture_is_remembered_until_one_shows() {
        let path = temp_file("picture");
        let (memory, start) = app_run(&path, Duration::ZERO);
        let shared = second_screen_with(memory);
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, start), Step::Attach);
        note_attached(&shared, "virtual".to_string());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 1, start);
        assert_eq!(
            next_step(&shared, start + PROVE_TIMEOUT),
            Step::Fallback("no_frames")
        );
        assert_eq!(saved(&path)["failure"]["reason"], "no_frames");

        // An app run two hours later is past the hour: it tries, and this
        // time the picture comes.
        let (memory, start) = app_run(&path, hours(2));
        assert_eq!(memory.hold(start), None);
        assert!(memory.saved());
        let shared = second_screen_with(memory);
        connect_viewer(&shared);
        note_connected(&shared, start);
        assert_eq!(next_step(&shared, start), Step::Attach);
        note_attached(&shared, "virtual".to_string());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 1, start);
        note_image(&shared, 1);
        assert!(!path.exists(), "a success forgets the failure");
        assert!(!shared.second_screen.lock().memory.saved());
        let (memory, start) = app_run(&path, hours(3));
        assert_eq!(memory.hold(start), None);
        assert!(!memory.saved());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_wait_grows_after_each_failure_up_to_a_week() {
        let path = temp_file("backoff");
        let expected = [1, 6, 24, 72, 168, 168, 168].map(hours);
        let mut elapsed = Duration::ZERO;
        for (count, wait) in (1..).zip(expected) {
            // Each failure comes in a new app run, right when it may try.
            let (mut memory, start) = app_run(&path, elapsed);
            assert_eq!(memory.hold(start), None, "failure {count} may try");
            let hold = memory.note_failure("attach_timeout", start).unwrap();
            assert_eq!((hold.failures, hold.retry_in), (count, wait));
            let second = Duration::from_secs(1);
            assert!(memory.hold(start + wait - second).is_some());
            assert_eq!(memory.hold(start + wait), None);
            let (reloaded, at) = app_run(&path, elapsed + wait - second);
            assert_eq!(reloaded.hold(at).map(|h| h.failures), Some(count));
            elapsed += wait;
        }
        // A clock set back before the failure tries rather than waiting on.
        let (memory, start) = app_run(&path, elapsed - hours(169));
        assert_eq!(memory.hold(start), None);
        assert!(memory.saved());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_driver_windows_or_app_update_tries_again_at_once() {
        type Change = (&'static str, fn(&mut Fingerprint));
        let changes: [Change; 5] = [
            ("display_driver", |f| f.display_driver = "25.11.3".into()),
            ("windows", |f| f.windows = "26100.3194".into()),
            ("app", |f| f.app = "0.3.4".into()),
            ("graphics", |f| {
                f.graphics[1] = "NVIDIA GeForce RTX 3050 Laptop GPU 32.0.15.6614".into()
            }),
            ("graphics", |f| f.graphics.pop().map(drop).unwrap()),
        ];
        for (name, change) in changes {
            let path = temp_file("fingerprint");
            let (mut memory, start) = app_run(&path, Duration::ZERO);
            memory.note_failure("duplication_failed", start).unwrap();
            let (same, at) = app_run(&path, Duration::from_secs(60));
            assert!(same.hold(at).is_some(), "the same PC waits");

            let mut updated = laptop();
            change(&mut updated);
            assert_eq!(laptop().changes(&updated), [name]);
            let at = Instant::now();
            let memory = Memory::load(path.clone(), updated, T0 + 60, at);
            assert_eq!(memory.hold(at), None, "{name}");
            assert!(!memory.saved());
            assert!(!path.exists(), "{name}");
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn try_again_or_another_mode_forgets_the_failure() {
        for how in ["try again", "another mode"] {
            let path = temp_file("reset");
            let (mut memory, start) = app_run(&path, Duration::ZERO);
            memory.note_failure("attach_timeout", start).unwrap();
            let (memory, start) = app_run(&path, Duration::from_secs(60));
            let shared = second_screen_with(memory);
            connect_viewer(&shared);
            assert_eq!(next_step(&shared, start), Step::Nothing);
            assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);

            if how == "try again" {
                shared.retry_second_screen();
            } else {
                *shared.capture_target.lock() = CaptureTarget::PrimaryAuto;
                assert_eq!(next_step(&shared, start), Step::Nothing);
                // Back to Second screen: the next capture generation waits
                // for the viewer again.
                *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
                *shared.vdd_status.lock() = VddStatus::WaitingForClient;
            }
            assert!(!path.exists(), "{how}");
            assert_eq!(next_step(&shared, start), Step::Attach, "{how}");
            let (memory, start) = app_run(&path, Duration::from_secs(120));
            assert_eq!(memory.hold(start), None, "{how}");
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn an_unreadable_or_old_record_is_ignored() {
        let record = |format: u32, reason: &str, failures: u32| {
            serde_json::json!({
                "format": format,
                "failure": {
                    "fingerprint": laptop(),
                    "reason": reason,
                    "failures": failures,
                    "failed_at": T0,
                },
            })
            .to_string()
        };
        let contents = [
            String::new(),
            "{".to_string(),
            "not json".to_string(),
            "[]".to_string(),
            "null".to_string(),
            r#"{"format":1}"#.to_string(),
            // Older layouts: without a fingerprint, and the flat one.
            r#"{"reason":"attach_timeout","failed_at":1800000000}"#.to_string(),
            serde_json::json!({
                "format": 1,
                "fingerprint": laptop(),
                "reason": "attach_timeout",
                "failures": 1,
                "failed_at": T0,
            })
            .to_string(),
            record(2, "attach_timeout", 1),
            record(1, "enable_failed", 1),
            record(1, "attach_timeout", 0),
            r#"{"format":1,"failure":{"fingerprint":{"app":7},"reason":"no_frames","failures":1,"failed_at":1}}"#
                .to_string(),
        ];
        for content in contents {
            let path = temp_file("corrupt");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &content).unwrap();
            let (mut memory, start) = app_run(&path, Duration::ZERO);
            assert_eq!(memory.hold(start), None, "{content}");
            assert!(!memory.saved(), "{content}");
            // The next failure is saved as usual.
            memory.note_failure("no_frames", start).unwrap();
            let (reloaded, at) = app_run(&path, Duration::from_secs(1));
            assert_eq!(reloaded.hold(at).map(|h| h.reason), Some("no_frames"));
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
        // Bytes that are not text, and a record that matches, for contrast.
        let path = temp_file("binary");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert!(!app_run(&path, Duration::ZERO).0.saved());
        std::fs::write(&path, record(1, "attach_timeout", 3)).unwrap();
        let (memory, start) = app_run(&path, Duration::ZERO);
        assert_eq!(memory.hold(start).map(|h| h.failures), Some(3));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Review round 1: a PC that works, waking to the lock screen (or a UAC
    /// prompt) during the picture check, mirrored for an hour at the next
    /// app run. A failure Windows' desktop explains is only for this run.
    #[test]
    fn a_blocked_desktop_falls_back_for_this_app_run_only() {
        use crate::capture::recovery::{passing_error, CaptureBlock, CaptureUnavailable};
        for reason in ["duplication_failed", "no_frames"] {
            let path = temp_file("blocked");
            let (memory, start) = app_run(&path, Duration::ZERO);
            let shared = second_screen_with(memory);
            connect_viewer(&shared);
            assert_eq!(next_step(&shared, start), Step::Attach);
            note_attached(&shared, "virtual".to_string());
            assert_eq!(next_step(&shared, start), Step::Switch);
            start_proving(&shared, 1, start);
            // What the capture stage does with the lock screen's error, or
            // with an ACCESS_LOST it keeps retrying.
            let error: Box<dyn std::error::Error + Send + Sync> = Box::new(CaptureUnavailable {
                block: CaptureBlock::SecureDesktop,
                detail: String::new(),
            });
            assert!(passing_error(error.as_ref()));
            super::super::note_interruption(&shared, 1);
            if reason == "duplication_failed" {
                super::super::note_capture_error(&shared, 1);
            }
            assert_eq!(
                next_step(&shared, start + PROVE_TIMEOUT),
                Step::Fallback(reason)
            );
            assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
            assert!(!path.exists(), "{reason}: nothing saved");
            assert!(!retries_later(&shared));
            // This app run keeps mirroring until Try again, as in 0.3.3.
            let later = start + hours(2);
            reconnect(&shared, later);
            assert_eq!(next_step(&shared, later), Step::Nothing);
            // The next one tries at once.
            let (memory, start) = app_run(&path, Duration::from_secs(60));
            assert!(!memory.saved());
            let shared = second_screen_with(memory);
            connect_viewer(&shared);
            assert_eq!(next_step(&shared, start), Step::Attach, "{reason}");
        }
    }

    /// Round 5: a try after the wait that failed for a reason that passes
    /// kept the expired record, so every later connection in that app run
    /// turned the driver on again.
    #[test]
    fn a_passing_failure_after_the_wait_mirrors_for_the_rest_of_the_app_run() {
        use super::super::{note_capture_error, note_interruption};
        let path = temp_file("expired-then-passing");
        let (mut memory, start) = app_run(&path, Duration::ZERO);
        memory.note_failure("no_frames", start).unwrap();
        // The next app run is past the hour, so it tries, and the lock
        // screen ends the try.
        let (memory, start) = app_run(&path, hours(2));
        assert!(memory.saved() && memory.hold(start).is_none());
        let shared = second_screen_with(memory);
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, start), Step::Attach);
        note_attached(&shared, "virtual".to_string());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 1, start);
        note_interruption(&shared, 1);
        note_capture_error(&shared, 1);
        assert_eq!(
            next_step(&shared, start + secs(1)),
            Step::Fallback("duplication_failed")
        );
        assert!(!retries_later(&shared));
        // This app run mirrors until Try again, without trying by itself.
        for minute in 1..=180 {
            let now = start + Duration::from_secs(60 * minute);
            reconnect(&shared, now);
            assert_eq!(next_step(&shared, now), Step::Nothing, "minute {minute}");
            assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
        }
        // The saved count stays for the next app run's wait, and Try again
        // still forgets it.
        assert_eq!(saved(&path)["failure"]["failures"], 1);
        shared.retry_second_screen();
        assert!(!path.exists());
        assert_eq!(next_step(&shared, start + hours(3)), Step::Attach);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// The second screen shown up to the switch, then proving itself.
    fn proving_second_screen(path: &Path) -> (SharedControl, Instant) {
        let (memory, start) = app_run(path, Duration::ZERO);
        let shared = second_screen_with(memory);
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, start), Step::Attach);
        note_attached(&shared, "virtual".to_string());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 1, start);
        (shared, start)
    }

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    /// Review round 2: a display change that passes gives the second screen
    /// a fresh window, and the picture that follows proves it.
    #[test]
    fn one_lost_desktop_then_a_picture_is_a_success() {
        use super::super::{note_interruption, note_recovery};
        let path = temp_file("loss-then-frames");
        let (shared, start) = proving_second_screen(&path);
        note_interruption(&shared, 1);
        note_recovery(&shared, 1, start + secs(4));
        assert_eq!(next_step(&shared, start + PROVE_TIMEOUT), Step::Nothing);
        assert_eq!(
            next_step(&shared, start + secs(4) + PROVE_TIMEOUT - secs(1)),
            Step::Nothing
        );
        note_image(&shared, 1);
        assert!(!super::super::proving(&shared));
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
        assert!(!path.exists());
    }

    /// Review round 2: a lost desktop followed by an error that doesn't pass
    /// (the hybrid laptop's DXGI_ERROR_UNSUPPORTED) is the PC's failure.
    #[test]
    fn a_lost_desktop_then_an_unsupported_display_is_remembered() {
        use super::super::{note_interruption, note_persistent_error};
        let path = temp_file("loss-then-unsupported");
        let (shared, start) = proving_second_screen(&path);
        note_interruption(&shared, 1);
        note_persistent_error(&shared, 1);
        assert_eq!(
            next_step(&shared, start + PROVE_TIMEOUT),
            Step::Fallback("duplication_failed")
        );
        assert_eq!(saved(&path)["failure"]["reason"], "duplication_failed");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Round 5: a retry that failed with an error that doesn't pass kept the
    /// recovery after it from restarting the window, and a retry that then
    /// failed with one that passes still saved the failure. The last retry
    /// decides.
    #[test]
    fn the_last_retry_after_a_lost_desktop_decides() {
        use super::super::{
            note_interruption, note_passing_retry_error, note_persistent_error, note_recovery,
        };
        // Lost at 1 s, UNSUPPORTED at 2 s, back at 4 s: the window runs to 9 s.
        let path = temp_file("unsupported-then-back");
        let (shared, start) = proving_second_screen(&path);
        note_interruption(&shared, 1);
        note_persistent_error(&shared, 1);
        note_recovery(&shared, 1, start + secs(4));
        assert_eq!(next_step(&shared, start + PROVE_TIMEOUT), Step::Nothing);
        assert_eq!(next_step(&shared, start + secs(8)), Step::Nothing);
        assert_eq!(
            next_step(&shared, start + secs(9)),
            Step::Fallback("no_frames")
        );
        assert_eq!(saved(&path)["failure"]["reason"], "no_frames");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();

        // Lost, UNSUPPORTED, then another capturing app until the window
        // ends: this app run only.
        let path = temp_file("unsupported-then-passing");
        let (shared, start) = proving_second_screen(&path);
        note_interruption(&shared, 1);
        note_persistent_error(&shared, 1);
        note_passing_retry_error(&shared, 1);
        assert_eq!(
            next_step(&shared, start + PROVE_TIMEOUT),
            Step::Fallback("no_frames")
        );
        assert!(!path.exists());
        assert!(!retries_later(&shared));
    }

    /// Round 4: losses that pass never save a failure, however many. The
    /// picture that follows proves the second screen; without one the stream
    /// goes back to the mirror for this app run only.
    #[test]
    fn passing_losses_never_save_a_failure() {
        use super::super::{note_interruption, note_recovery, LOSS_LIMIT};
        // Four losses, then a picture.
        let path = temp_file("four-then-frames");
        let (shared, start) = proving_second_screen(&path);
        for second in 1..=4 {
            note_interruption(&shared, 1);
            note_recovery(&shared, 1, start + secs(second));
        }
        assert_eq!(
            next_step(&shared, start + secs(4) + PROVE_TIMEOUT - secs(1)),
            Step::Nothing
        );
        note_image(&shared, 1);
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
        assert!(!path.exists());

        // Four losses, and the desktop never comes back.
        let path = temp_file("four-then-nothing");
        let (shared, start) = proving_second_screen(&path);
        for second in 1..=4 {
            note_interruption(&shared, 1);
            if second < 4 {
                note_recovery(&shared, 1, start + secs(second));
            }
        }
        assert_eq!(
            next_step(&shared, start + secs(3) + PROVE_TIMEOUT),
            Step::Fallback("no_frames")
        );
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
        assert!(!path.exists(), "this app run only");
        assert!(!retries_later(&shared));

        // A display that keeps dropping out ends the proof early, still for
        // this app run only.
        let path = temp_file("loss-limit");
        let (shared, start) = proving_second_screen(&path);
        for second in 1..=u64::from(LOSS_LIMIT) + 1 {
            note_interruption(&shared, 1);
            note_recovery(&shared, 1, start + secs(second));
        }
        assert_eq!(
            next_step(&shared, start + secs(9)),
            Step::Fallback("duplication_failed")
        );
        assert!(!path.exists());
    }

    /// Round 4: a full window after the desktop came back, with no picture,
    /// is the PC's failure.
    #[test]
    fn a_full_window_without_a_picture_after_a_recovery_is_remembered() {
        use super::super::{note_interruption, note_recovery};
        let path = temp_file("loss-recovery-nothing");
        let (shared, start) = proving_second_screen(&path);
        note_interruption(&shared, 1);
        note_recovery(&shared, 1, start + secs(2));
        let deadline = start + secs(2) + PROVE_TIMEOUT;
        assert_eq!(next_step(&shared, deadline - secs(1)), Step::Nothing);
        assert_eq!(next_step(&shared, deadline), Step::Fallback("no_frames"));
        assert_eq!(saved(&path)["failure"]["reason"], "no_frames");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Review round 3: a loss behind the lock screen counted toward the
    /// three, so two more after unlocking saved a working display.
    #[test]
    fn losses_behind_a_secure_screen_do_not_count() {
        use super::super::{note_interruption, note_interruption_while, note_recovery};
        let path = temp_file("secure-loss");
        let (shared, start) = proving_second_screen(&path);
        note_interruption_while(&shared, 1, true);
        note_recovery(&shared, 1, start + secs(30));
        for second in [31, 32] {
            note_interruption(&shared, 1);
            note_recovery(&shared, 1, start + secs(second));
        }
        let unlocked = start + secs(32);
        assert_eq!(
            next_step(&shared, unlocked + PROVE_TIMEOUT - secs(1)),
            Step::Nothing
        );
        note_image(&shared, 1);
        assert!(!super::super::proving(&shared));
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
        assert!(!path.exists(), "nothing saved");
    }

    /// Review round 2: idle time said "the display may be off", but an iPad
    /// that connects sends Windows no input, so an idle, unlocked PC never
    /// saved a failure. Only a secure screen or displays that are off count.
    #[test]
    fn an_unlocked_pc_with_its_display_on_remembers_however_long_it_was_idle() {
        use super::super::away;
        assert!(!away(Some("Default"), Some(false)));
        assert!(!away(Some("Default"), None), "unknown power state");
        assert!(away(Some("Default"), Some(true)), "displays off");
        assert!(away(Some("Winlogon"), Some(false)), "secure screen");
        assert!(away(None, Some(false)), "the input desktop can't be opened");
        // An iPad-started attempt on such a PC, after any idle time: the
        // state machine has no idle input, and the failure is saved.
        let path = temp_file("idle");
        let (shared, start) = proving_second_screen(&path);
        let idle = hours(5);
        assert_eq!(
            next_step(&shared, start + PROVE_TIMEOUT + idle),
            Step::Fallback("no_frames")
        );
        assert_eq!(saved(&path)["failure"]["reason"], "no_frames");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn locked(_: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("sharing violation"))
    }

    fn locked_write(_: &Path, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("sharing violation"))
    }

    /// Review round 2: a file another program holds without FILE_SHARE_DELETE
    /// can be neither removed nor replaced. This app run then acts as if it
    /// were cleared, and clears it at the next success. Round 3: nor
    /// overwritten, here; Mirror mode retried that every transport tick and
    /// warned each time.
    #[test]
    fn a_record_that_cant_be_cleared_is_retried_now_and_then() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static REMOVES: AtomicU32 = AtomicU32::new(0);
        fn counted(_: &Path) -> std::io::Result<()> {
            REMOVES.fetch_add(1, Ordering::Relaxed);
            Err(std::io::Error::other("sharing violation"))
        }
        let path = temp_file("locked");
        let (mut memory, start) = app_run(&path, Duration::ZERO);
        memory.note_failure("attach_timeout", start).unwrap();
        memory.store.as_mut().unwrap().io = Io {
            remove: counted,
            replace: locked_write,
            overwrite: locked_write,
        };
        assert!(memory.forget(start, false));
        assert!(!memory.saved() && memory.uncleared.is_some());
        assert!(memory.clear_failure_logged);
        assert_eq!(memory.hold(start), None, "this app run acts as cleared");
        // Mirror mode's ticks for the next minute don't touch the file.
        for tick in 1..1200 {
            assert!(!memory.forget(start + Duration::from_millis(50 * tick), false));
        }
        assert_eq!(REMOVES.load(Ordering::Relaxed), 1);
        assert!(!memory.forget(start + CLEAR_RETRY, false));
        assert_eq!(REMOVES.load(Ordering::Relaxed), 2, "once a minute");
        assert!(!memory.forget(start + CLEAR_RETRY, true));
        assert_eq!(REMOVES.load(Ordering::Relaxed), 3, "and at each success");
        assert!(memory.uncleared.is_some());
        // The residual: an app run starting now would still hold.
        let (next, at) = app_run(&path, secs(60));
        assert!(next.hold(at).is_some());
        // Only replacing works: the file then says there is no failure.
        memory.store.as_mut().unwrap().io = Io {
            remove: locked,
            replace: Io::FILES.replace,
            overwrite: locked_write,
        };
        assert!(!memory.forget(start + CLEAR_RETRY, true));
        assert!(memory.uncleared.is_none());
        assert_eq!(saved(&path)["failure"], serde_json::Value::Null);
        let (next, at) = app_run(&path, secs(60));
        assert!(!next.saved() && next.hold(at).is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Round 4: the paced retry ran only while Second screen was not
    /// chosen, so a record that couldn't be cleared survived a working
    /// session and held again at the next app run.
    #[test]
    fn a_record_that_couldnt_be_cleared_is_cleared_during_a_working_session() {
        let path = temp_file("uncleared-session");
        let (mut memory, start) = app_run(&path, Duration::ZERO);
        memory.note_failure("attach_timeout", start).unwrap();
        memory.store.as_mut().unwrap().io = Io {
            remove: locked,
            replace: locked_write,
            overwrite: locked_write,
        };
        let later = start + hours(2);
        let shared = second_screen_with(memory);
        connect_viewer(&shared);
        // Past the wait: the second screen is tried and shows a picture, but
        // the file is still held.
        assert_eq!(next_step(&shared, later), Step::Attach);
        note_attached(&shared, "virtual".to_string());
        assert_eq!(next_step(&shared, later), Step::Switch);
        start_proving(&shared, 1, later);
        note_image(&shared, 1);
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
        assert!(shared.second_screen.lock().memory.uncleared.is_some());
        // The other program lets go; the transport's ticks clear it while
        // the second screen streams.
        shared
            .second_screen
            .lock()
            .memory
            .store
            .as_mut()
            .unwrap()
            .io = Io::FILES;
        let tried = shared.second_screen.lock().memory.uncleared.unwrap();
        assert_eq!(next_step(&shared, tried + secs(30)), Step::Nothing);
        assert!(path.exists(), "paced: not yet");
        assert_eq!(next_step(&shared, tried + CLEAR_RETRY), Step::Nothing);
        assert!(!path.exists());
        assert!(shared.second_screen.lock().memory.uncleared.is_none());
        let (next, at) = app_run(&path, hours(3));
        assert!(!next.saved() && next.hold(at).is_none());
    }

    /// Review round 3: a handle that allows writing but not deleting defeats
    /// both the removal and the rename; the file is then written in place.
    #[test]
    fn a_record_that_cant_be_removed_or_renamed_is_overwritten_in_place() {
        let path = temp_file("in-place");
        let (mut memory, start) = app_run(&path, Duration::ZERO);
        memory.note_failure("no_frames", start).unwrap();
        memory.store.as_mut().unwrap().io = Io {
            remove: locked,
            replace: locked_write,
            overwrite: Io::FILES.overwrite,
        };
        assert!(memory.forget(start, true));
        assert!(memory.uncleared.is_none() && !memory.clear_failure_logged);
        assert_eq!(saved(&path)["failure"], serde_json::Value::Null);
        let (next, at) = app_run(&path, secs(60));
        assert!(!next.saved() && next.hold(at).is_none());
        // Torn halfway, it reads as unusable: no failure either.
        std::fs::write(&path, &std::fs::read(&path).unwrap()[..10]).unwrap();
        let (next, at) = app_run(&path, secs(120));
        assert!(!next.saved() && next.hold(at).is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Review round 1: a success whose file could not be removed left the
    /// failure for the next app run. The file then says there is none.
    #[test]
    fn a_cleared_record_loads_as_none() {
        let path = temp_file("cleared");
        let (mut memory, start) = app_run(&path, Duration::ZERO);
        memory.note_failure("no_frames", start).unwrap();
        assert!(memory.store.as_ref().unwrap().write(None).is_ok());
        let (memory, start) = app_run(&path, Duration::from_secs(60));
        assert_eq!(memory.hold(start), None);
        assert!(!memory.saved());
        assert!(path.exists(), "a cleared file is not unreadable");
        assert_eq!(saved(&path)["failure"], serde_json::Value::Null);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Review round 1: a corrupt time near the end of the clock overflowed.
    #[test]
    fn a_failure_at_the_end_of_the_clock_tries_again() {
        let path = temp_file("overflow");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for failed_at in [u64::MAX, u64::MAX - 10] {
            let file = serde_json::json!({
                "format": 1,
                "failure": {
                    "fingerprint": laptop(),
                    "reason": "attach_timeout",
                    "failures": u32::MAX,
                    "failed_at": failed_at,
                },
            });
            std::fs::write(&path, file.to_string()).unwrap();
            let start = Instant::now();
            let mut memory = Memory::load(path.clone(), laptop(), u64::MAX - 5, start);
            assert!(memory.saved());
            assert_eq!(memory.hold(start), None);
            assert_eq!(memory.hold(start + hours(1_000_000)), None);
            let hold = memory.note_failure("attach_timeout", start + hours(1));
            assert_eq!(hold, None, "the count saturates, the wait can't end");
            assert_eq!(saved(&path)["failure"]["failures"], u32::MAX);
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Unit tests and builds off Windows have no store: nothing is held,
    /// saved or written, as in 0.3.3.
    #[test]
    fn without_a_store_nothing_is_saved() {
        let mut memory = Memory::default();
        let now = Instant::now();
        assert_eq!(memory.note_failure("attach_timeout", now), None);
        assert_eq!(memory.hold(now), None);
        assert!(!memory.saved() && !memory.forget(now, true));
    }
}
