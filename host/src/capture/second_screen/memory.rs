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
}

#[derive(Debug, Default)]
pub(crate) struct Memory {
    store: Option<Store>,
    failed: Option<Failed>,
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
                remove(&path);
                None
            }
            Ok(Some((record, _))) if record.fingerprint != fingerprint => {
                tracing::info!(
                    reason = %record.reason,
                    changed = %record.fingerprint.changes(&fingerprint).join(","),
                    "The second screen failed on this PC before, with another driver, Windows \
                     build or app version — trying it again"
                );
                remove(&path);
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
            }),
            failed,
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

    /// Whether a failure is saved, held or due.
    pub(crate) fn saved(&self) -> bool {
        self.failed.is_some()
    }

    /// An attempt failed for `reason`. Saves it when it is one to remember,
    /// counting it after the failures before, and returns the wait.
    pub(crate) fn note_failure(&mut self, reason: &'static str, now: Instant) -> Option<Hold> {
        let reason = remembered(reason)?;
        let failed_at = self.wall(now)?;
        let store = self.store.as_ref()?;
        let failures = self.failed.map_or(0, |f| f.failures).saturating_add(1);
        write(
            &store.path,
            Some(Record {
                fingerprint: store.fingerprint.clone(),
                reason: reason.to_string(),
                failures,
                failed_at,
            }),
        );
        self.failed = Some(Failed {
            reason,
            failures,
            failed_at,
        });
        self.hold(now)
    }

    /// Forget the saved failure. Whether there was one. A file that can't
    /// be removed (another program holds it) is replaced by one that says
    /// there is no failure, so the next app run doesn't hold either.
    pub(crate) fn forget(&mut self) -> bool {
        if self.failed.take().is_none() {
            return false;
        }
        if let Some(store) = &self.store {
            if !remove(&store.path) {
                write(&store.path, None);
            }
        }
        true
    }
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

/// Replace the file at once, as the settings file is: a crash leaves the old
/// record or the new one.
fn write(path: &Path, failure: Option<Record>) {
    let file = File {
        format: FORMAT,
        failure,
    };
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| serde_json::to_vec_pretty(&file).map_err(std::io::Error::other))
        .and_then(|data| {
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path))
        });
    if let Err(error) = saved {
        warn!(%error, "Could not save the second screen record");
    }
}

/// Whether the file is gone.
fn remove(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            warn!(%error, "Could not remove the second screen record");
            false
        }
        _ => true,
    }
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

    /// Review round 1: a success whose file could not be removed left the
    /// failure for the next app run. The file then says there is none.
    #[test]
    fn a_cleared_record_loads_as_none() {
        let path = temp_file("cleared");
        let (mut memory, start) = app_run(&path, Duration::ZERO);
        memory.note_failure("no_frames", start).unwrap();
        write(&path, None);
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
        assert!(!memory.saved() && !memory.forget());
    }
}
