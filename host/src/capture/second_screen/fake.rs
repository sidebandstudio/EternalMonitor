//! A virtual display driver for end-to-end tests, in debug builds only.
//!
//! The second screen is otherwise covered by unit tests that drive [`attach`]
//! and the transport's steps by hand. A test installs a [`FakeDisplay`] on a
//! [`SharedControl`]: the real worker then turns it on and off, synthetic
//! capture switches onto it the way DXGI capture switches onto the real one,
//! a failure is saved in a folder the test chooses, and the fake, not
//! Windows, says whether the user's desktop is there and which graphics
//! card the main screen is on. Release builds
//! compile none of this, and nothing here reads the environment, so only a
//! test that calls [`FakeDisplay::install`] gets it.
//!
//! [`attach`]: super::attach

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use parking_lot::{Condvar, Mutex};

use super::memory::{Fingerprint, Memory, FILE_NAME};
use super::{Command, Driver};
use crate::capture::recovery::{CaptureBlock, CaptureUnavailable};
use crate::capture::OutputInfo;
use crate::control::{CaptureTarget, SharedControl};
use crate::gpu_preference::{Adapter, AdapterId, GpuFacts};

/// A display that attaches a set time after it is turned on, or never. Its
/// clock is the real one, plus whatever the test moved it on with
/// [`FakeDisplay::advance`].
#[derive(Clone)]
pub struct FakeDisplay(Arc<Inner>);

/// The card EternalMonitor runs on, and lists every screen under, as DXGI
/// does on a hybrid PC.
pub const APP_GPU: &str = "Fake GPU";

/// The LUIDs of [`APP_GPU`] and of the PC's other card.
const APP_CARD: AdapterId = (0, 1);
const OTHER_CARD: AdapterId = (0, 2);

struct Inner {
    /// See [`FakeDisplay::set_attach_after`].
    attach_after: Mutex<Option<Duration>>,
    attach_timeout: Duration,
    created: Instant,
    ahead: Mutex<Duration>,
    advanced: Condvar,
    /// When it was turned on, on its clock, while it is on.
    on_since: Mutex<Option<Instant>>,
    toggles: Mutex<Vec<(Duration, bool)>>,
    /// What it answers for Windows' input desktop: see
    /// [`FakeDisplay::set_desktop_away`].
    desktop_away: Arc<AtomicBool>,
    /// The display configuration: see [`FakeDisplay::move_screen`].
    facts: Arc<Mutex<Option<GpuFacts>>>,
}

impl FakeDisplay {
    /// A display that attaches `attach_after` after its Enable, or never.
    /// The worker waits `attach_timeout` for it, as it waits the update
    /// manifest's or `ETERNAL_VDD_TIMEOUT_SECS`'s.
    pub fn new(attach_after: Option<Duration>, attach_timeout: Duration) -> Self {
        Self(Arc::new(Inner {
            attach_after: Mutex::new(attach_after),
            attach_timeout,
            created: Instant::now(),
            ahead: Mutex::new(Duration::ZERO),
            advanced: Condvar::new(),
            on_since: Mutex::new(None),
            toggles: Mutex::new(Vec::new()),
            desktop_away: Arc::new(AtomicBool::new(false)),
            facts: Arc::new(Mutex::new(Some(screen_on(None)))),
        }))
    }

    /// Make it `shared`'s virtual display driver. `memory` stands for the app
    /// data folder, where a failed start is saved and read back by the next
    /// app run; this one reads it as at `wall`.
    pub fn install(&self, shared: &SharedControl, memory: &Path, wall: SystemTime) {
        let fingerprint = Fingerprint {
            app: env!("CARGO_PKG_VERSION").to_string(),
            display_driver: "fake".to_string(),
            windows: "fake".to_string(),
            graphics: vec!["Fake GPU 1.0".to_string()],
        };
        let wall = wall
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Loaded before the lock: loading starts the record's writer.
        let loaded = Memory::load(memory.join(FILE_NAME), fingerprint, wall, Instant::now());
        let mut state = shared.second_screen.lock();
        state.memory = loaded;
        state.fake_desktop_away = Some(self.0.desktop_away.clone());
        drop(state);
        super::use_fake_facts(shared, self.0.facts.clone());
        *shared.vdd_worker.fake.lock() = Some(self.clone());
    }

    /// Move its clock on by `by` at once: waits on it end early, and a
    /// display due to attach in that time attaches at the worker's next
    /// look. The host's own clock, and the saved failure's, don't move.
    pub fn advance(&self, by: Duration) {
        *self.0.ahead.lock() += by;
        self.0.advanced.notify_all();
    }

    /// Have the host see a secure screen or displays turned off (true) or
    /// the user's desktop (false, at first), in place of asking Windows, which
    /// has no input desktop in a service session such as CI's. A failure
    /// while it is away is not saved for later app runs.
    pub fn set_desktop_away(&self, away: bool) {
        self.0.desktop_away.store(away, Ordering::Relaxed);
    }

    /// Whether the display attaches `after` its next Enable, or never, as
    /// when Windows starts showing it as a copy of the main screen.
    pub fn set_attach_after(&self, after: Option<Duration>) {
        *self.0.attach_after.lock() = after;
    }

    /// Have Windows move the main screen to the PC's other card, named
    /// `card` (Some), as a laptop with two graphics cards may while the app
    /// runs, or back to [`APP_GPU`] (None, at first). On the other card,
    /// mirroring it fails as DXGI does then (`UnsupportedOnThisGpu`), and
    /// the display configuration says the screen is wired to it; the second
    /// screen still works.
    pub fn move_screen(&self, card: Option<&str>) {
        *self.0.facts.lock() = Some(screen_on(card));
    }

    /// Each time it was turned on (true) or off, on its clock, since it was
    /// made.
    pub fn toggles(&self) -> Vec<(Duration, bool)> {
        self.0.toggles.lock().clone()
    }
}

impl Driver for FakeDisplay {
    fn outputs(&self) -> Vec<OutputInfo> {
        let attached = self
            .0
            .on_since
            .lock()
            .zip(*self.0.attach_after.lock())
            .is_some_and(|(on, after)| self.now() >= on + after);
        let output = |name: &str, primary: bool| OutputInfo {
            adapter_index: 0,
            output_index: u32::from(!primary),
            device_name: name.to_string(),
            width: 1920,
            height: 1080,
            left: if primary { 0 } else { 1920 },
            top: 0,
            is_primary: primary,
            adapter_name: APP_GPU.to_string(),
        };
        let mut outputs = vec![output("primary", true)];
        if attached {
            outputs.push(output("virtual", false));
        }
        outputs
    }

    fn prepare(&self, _outputs: &[OutputInfo]) {}

    fn set_enabled(&self, enabled: bool) -> bool {
        let now = self.now();
        *self.0.on_since.lock() = enabled.then_some(now);
        self.0
            .toggles
            .lock()
            .push((now.duration_since(self.0.created), enabled));
        true
    }

    fn now(&self) -> Instant {
        Instant::now() + *self.0.ahead.lock()
    }

    fn sleep(&self, duration: Duration) {
        let until = self.now() + duration;
        let mut ahead = self.0.ahead.lock();
        loop {
            let now = Instant::now() + *ahead;
            if now >= until {
                return;
            }
            self.0.advanced.wait_for(&mut ahead, until - now);
        }
    }
}

pub(super) fn installed(shared: &SharedControl) -> Option<FakeDisplay> {
    shared.vdd_worker.fake.lock().clone()
}

/// [`super::send`] to the worker on `display`, which starts it as
/// `spawn_worker` starts the driver's.
pub(super) fn send(shared: &SharedControl, display: FakeDisplay, command: Command) {
    {
        let mut queue = shared.vdd_worker.queue.lock();
        if queue.quitting {
            return;
        }
        queue.commands.push_back(command);
    }
    shared.vdd_worker.wake.notify_one();
    let mut thread = shared.vdd_worker.thread.lock();
    if thread.is_none() {
        let worker = shared.clone();
        *thread = Some(
            std::thread::Builder::new()
                .name("second-screen".into())
                .spawn(move || super::run(&worker, &display, display.0.attach_timeout, true))
                .expect("start the fake display's worker"),
        );
    }
}

/// The PC's two cards, the main screen on the other one, named `card`, or
/// on [`APP_GPU`].
fn screen_on(card: Option<&str>) -> GpuFacts {
    GpuFacts {
        adapters: vec![
            Adapter {
                id: APP_CARD,
                name: APP_GPU.to_string(),
            },
            Adapter {
                id: OTHER_CARD,
                name: card.unwrap_or("Fake iGPU").to_string(),
            },
        ],
        power_saving: Some(OTHER_CARD),
        high_performance: Some(APP_CARD),
        screens: vec![(
            "primary".to_string(),
            if card.is_some() { OTHER_CARD } else { APP_CARD },
        )],
    }
}

/// What synthetic capture's mirror finds with a fake display installed: on
/// [`APP_GPU`] it works; after [`FakeDisplay::move_screen`] Windows refuses
/// it, as DXGI capture reports it.
pub(crate) fn mirror(shared: &SharedControl) -> Result<(), CaptureUnavailable> {
    let Some(display) = installed(shared) else {
        return Ok(());
    };
    let facts = display.0.facts.lock().clone();
    if !facts
        .as_ref()
        .is_some_and(|facts| facts.wired_elsewhere("primary", APP_CARD))
    {
        let app = Adapter {
            id: APP_CARD,
            name: APP_GPU.to_string(),
        };
        crate::capture::note_mirror_works(shared, Some(&app));
        return Ok(());
    }
    crate::capture::note_gpu_refused(shared, "primary", facts, Some(APP_CARD));
    Err(CaptureUnavailable {
        block: CaptureBlock::UnsupportedOnThisGpu,
        detail: "the fake main screen is on another graphics card".to_string(),
    })
}

/// The installed fake display's outputs, beside the primary one.
pub(crate) fn outputs(shared: &SharedControl) -> Option<Vec<OutputInfo>> {
    installed(shared).map(|display| display.outputs())
}

/// What synthetic capture `generation` does with a fake display installed:
/// whether it captures the second screen, as DXGI capture decides it, and
/// so proves it with its first frame. Without one, it mirrors. None when the
/// generation is already over.
pub(crate) fn capture_second_screen(shared: &SharedControl, generation: u64) -> Option<bool> {
    if installed(shared).is_none() {
        return Some(false);
    }
    let requested = shared.capture_target.lock().clone();
    let target = crate::capture::reconcile_virtual_display(&requested, shared, generation)?;
    Some(matches!(&target, CaptureTarget::Output(name)
        if shared.vdd_output.lock().as_deref() == Some(name.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fake, not Windows, says whether the desktop is there, so a test
    /// in a service session, which has no input desktop, still saves the
    /// failure.
    #[test]
    fn a_failure_is_saved_unless_the_fake_desktop_is_away() {
        for away in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "eternal-fake-desktop-{away}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let shared = SharedControl::new(9876, 15_000_000);
            let display = FakeDisplay::new(None, Duration::from_secs(1));
            display.install(&shared, &dir, SystemTime::now());
            display.set_desktop_away(away);
            shared
                .second_screen
                .lock()
                .remember("attach_timeout", Instant::now(), false, false);
            // The file may be written a moment after `remember` returns.
            let file = dir.join(FILE_NAME);
            if away {
                std::thread::sleep(Duration::from_millis(500));
                assert!(!file.exists(), "not saved while the desktop is away");
            } else {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !file.exists() {
                    assert!(Instant::now() < deadline, "the failure is saved");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The fake's answer for the desktop is read before the proof is
    /// locked to note a lost desktop, not under the same lock. A regression
    /// fails here instead of hanging the run.
    #[test]
    fn an_interruption_is_noted_with_the_fake_installed() {
        let dir =
            std::env::temp_dir().join(format!("eternal-fake-interruption-{}", std::process::id()));
        let shared = SharedControl::new(9876, 15_000_000);
        let display = FakeDisplay::new(None, Duration::from_secs(1));
        display.install(&shared, &dir, SystemTime::now());
        super::super::start_proving(&shared, 1, Instant::now());
        let (done, finished) = std::sync::mpsc::channel();
        let noting = shared.clone();
        std::thread::spawn(move || {
            super::super::note_interruption(&noting, 1);
            let _ = done.send(());
        });
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("note_interruption returns instead of deadlocking");
        let proving = shared.second_screen.lock().proving.unwrap();
        assert!(proving.interrupted);
        assert_eq!(proving.losses, 1, "a loss while the desktop is there");
    }
}
