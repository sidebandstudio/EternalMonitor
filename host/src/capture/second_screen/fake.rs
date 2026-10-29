//! A virtual display driver for end-to-end tests, in debug builds only.
//!
//! The second screen is otherwise covered by unit tests that drive [`attach`]
//! and the transport's steps by hand. A test installs a [`FakeDisplay`] on a
//! [`SharedControl`]: the real worker then turns it on and off, synthetic
//! capture switches onto it the way DXGI capture switches onto the real one,
//! and a failure is saved in a folder the test chooses. Release builds
//! compile none of this, and nothing here reads the environment, so only a
//! test that calls [`FakeDisplay::install`] gets it.
//!
//! [`attach`]: super::attach

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use parking_lot::{Condvar, Mutex};

use super::memory::{Fingerprint, Memory, FILE_NAME};
use super::{Command, Driver};
use crate::capture::OutputInfo;
use crate::control::{CaptureTarget, SharedControl};

/// A display that attaches a set time after it is turned on, or never. Its
/// clock is the real one, plus whatever the test moved it on with
/// [`FakeDisplay::advance`].
#[derive(Clone)]
pub struct FakeDisplay(Arc<Inner>);

struct Inner {
    attach_after: Option<Duration>,
    attach_timeout: Duration,
    created: Instant,
    ahead: Mutex<Duration>,
    advanced: Condvar,
    /// When it was turned on, on its clock, while it is on.
    on_since: Mutex<Option<Instant>>,
    toggles: Mutex<Vec<(Duration, bool)>>,
}

impl FakeDisplay {
    /// A display that attaches `attach_after` after its Enable, or never.
    /// The worker waits `attach_timeout` for it, as it waits the update
    /// manifest's or `ETERNAL_VDD_TIMEOUT_SECS`'s.
    pub fn new(attach_after: Option<Duration>, attach_timeout: Duration) -> Self {
        Self(Arc::new(Inner {
            attach_after,
            attach_timeout,
            created: Instant::now(),
            ahead: Mutex::new(Duration::ZERO),
            advanced: Condvar::new(),
            on_since: Mutex::new(None),
            toggles: Mutex::new(Vec::new()),
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
        shared.second_screen.lock().memory =
            Memory::load(memory.join(FILE_NAME), fingerprint, wall, Instant::now());
        *shared.vdd_worker.fake.lock() = Some(self.clone());
    }

    /// Move its clock on by `by` at once: waits on it end early, and a
    /// display due to attach in that time attaches at the worker's next
    /// look. The host's own clock, and the saved failure's, don't move.
    pub fn advance(&self, by: Duration) {
        *self.0.ahead.lock() += by;
        self.0.advanced.notify_all();
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
            .zip(self.0.attach_after)
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
            adapter_name: "Fake GPU".to_string(),
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
