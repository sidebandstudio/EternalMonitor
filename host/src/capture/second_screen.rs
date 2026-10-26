//! Starting the second screen beside a live mirror.
//!
//! 0.3.2 held video back while it turned the virtual display on: about 3 s
//! for the Enable task (6 s behind a Disable still running), then up to 10 s
//! for the display to attach. The iPad gives up 10 s after the handshake when
//! no frame has decoded, so on a PC whose display attached slowly or never,
//! the iPad left first and the mirror fallback never reached it. Over USB the
//! host dialed again a second later and turned the display off and on again
//! for every attempt.
//!
//! Now a connecting iPad gets the mirror at once, from the capture generation
//! already running. The display is turned on by its own thread here, and the
//! transport restarts the pipeline onto it once it exists. A start that
//! failed is remembered: later connections stay on the mirror until the user
//! clicks Try again, restarts the stream, or changes the mode.

use std::time::{Duration, Instant};

#[cfg(any(windows, test))]
use tracing::{info, warn};

use crate::control::{CaptureTarget, SharedControl, VddStatus};

#[cfg(any(windows, test))]
use super::{wait_for_virtual_output, AttachResult, OutputInfo};

/// How long the second screen stays on after its iPad leaves. A USB redial
/// comes about 1 s after the tunnel closes, and an iPad returning from the
/// background or a short Wi-Fi drop is back within seconds; both find the
/// second screen still there.
pub(crate) const RELEASE_GRACE: Duration = Duration::from_secs(10);

/// The host never turns the virtual display on or off again sooner than this
/// after its previous change. Each change adds or removes a display adapter:
/// the PC's screens blink and Windows plays its device sound.
#[cfg(any(windows, test))]
pub(crate) const MIN_TOGGLE_GAP: Duration = Duration::from_secs(10);

#[cfg(any(windows, test))]
const WAIT_STEP: Duration = Duration::from_millis(100);

/// How long a stopped pipeline may take to come back before the display is
/// left off: a restart joins the old generation for up to 5 s, while a quit
/// or Stop leaves it stopped.
#[cfg(any(windows, test))]
const STOPPED_WAIT: Duration = Duration::from_secs(6);

/// What the transport does next for the second screen.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Nothing,
    /// A viewer arrived: start the display beside the mirror.
    Attach,
    /// The display attached while the mirror streamed: restart onto it.
    Switch,
    /// Nobody came back within the grace period: restart to turn it off.
    Release,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    /// The display attached; the stream has not moved onto it yet.
    switch_pending: bool,
    /// When the attached display was last seen without a viewer.
    viewer_left_at: Option<Instant>,
    /// The grace period ended; the next capture generation turns it off.
    release_requested: bool,
    /// When the host last turned the display on or off.
    last_toggle: Option<Instant>,
}

impl State {
    pub(crate) fn step(
        &mut self,
        selected: bool,
        status: &mut VddStatus,
        connected: bool,
        now: Instant,
    ) -> Step {
        if !selected {
            *self = State {
                last_toggle: self.last_toggle,
                ..State::default()
            };
            return Step::Nothing;
        }
        match *status {
            VddStatus::WaitingForClient if connected => {
                *status = VddStatus::Attaching;
                Step::Attach
            }
            VddStatus::Active if connected => {
                self.viewer_left_at = None;
                self.release_requested = false;
                if std::mem::take(&mut self.switch_pending) {
                    Step::Switch
                } else {
                    Step::Nothing
                }
            }
            VddStatus::Active if !self.release_requested => {
                let left = *self.viewer_left_at.get_or_insert(now);
                if now.duration_since(left) < RELEASE_GRACE {
                    return Step::Nothing;
                }
                self.viewer_left_at = None;
                self.switch_pending = false;
                self.release_requested = true;
                Step::Release
            }
            _ => Step::Nothing,
        }
    }

    /// Whether a capture generation with no viewer should leave an attached
    /// display on: only the grace period's end turns it off.
    #[cfg(any(windows, test))]
    pub(crate) fn keep_without_viewer(&self) -> bool {
        !self.release_requested
    }

    #[cfg(any(windows, test))]
    pub(crate) fn note_toggle(&mut self, now: Instant) {
        self.last_toggle = Some(now);
        self.release_requested = false;
    }

    #[cfg(any(windows, test))]
    fn toggle_delay(&self, now: Instant) -> Duration {
        self.last_toggle.map_or(Duration::ZERO, |at| {
            (at + MIN_TOGGLE_GAP).saturating_duration_since(now)
        })
    }
}

/// The transport's decision for this tick. Status before state, the order
/// every caller locks them in.
pub(crate) fn next_step(shared: &SharedControl, now: Instant) -> Step {
    let selected = *shared.capture_target.lock() == CaptureTarget::VirtualExtended;
    let connected = shared.client_connected();
    let mut status = shared.vdd_status.lock();
    shared
        .second_screen
        .lock()
        .step(selected, &mut status, connected, now)
}

/// Why connections mirror instead of extending, after a failed start. The
/// value appears as `second_screen_fallback=` in the log, which the
/// diagnostics upload sends as is.
pub(crate) fn remembered_failure(status: VddStatus) -> Option<&'static str> {
    match status {
        VddStatus::Failed => Some("attach_timeout"),
        VddStatus::TaskFailed => Some("enable_failed"),
        _ => None,
    }
}

/// Turn the display on beside the mirror. Returns at once.
pub(crate) fn start(shared: SharedControl) {
    #[cfg(windows)]
    if !super::synthetic_requested() {
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("second-screen".into())
            .spawn(move || {
                attach(
                    &worker,
                    &WindowsDriver { shared: &worker },
                    super::vdd_attach_timeout(),
                )
            });
        if let Err(error) = spawned {
            warn!(%error, second_screen_fallback = "enable_failed",
                "Could not start the second screen — mirroring the primary display instead");
            *shared.vdd_status.lock() = VddStatus::TaskFailed;
        }
        return;
    }
    tracing::info!(
        second_screen_fallback = "no_driver",
        "No virtual display driver with this capture source — mirroring the primary display"
    );
    *shared.vdd_status.lock() = VddStatus::TaskFailed;
}

/// What turning the display on needs from Windows, so tests can supply a
/// clock and a driver that attaches late or never.
#[cfg(any(windows, test))]
pub(crate) trait Driver {
    fn outputs(&self) -> Vec<OutputInfo>;
    /// Prepare the driver's settings for the connected iPad.
    fn prepare(&self);
    fn set_enabled(&self, enabled: bool) -> bool;
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// Turn the display on, wait for it to attach, and record the result. A
/// viewer leaving does not cancel it; only a change of mode does. Capture
/// keeps mirroring throughout.
#[cfg(any(windows, test))]
pub(crate) fn attach(shared: &SharedControl, driver: &impl Driver, timeout: Duration) {
    let selected = || *shared.capture_target.lock() == CaptureTarget::VirtualExtended;
    // Capture generations turn the display off under the same lock.
    let _lifecycle = loop {
        if let Some(guard) = shared.vdd_lifecycle.try_lock_for(Duration::from_millis(50)) {
            break guard;
        }
        if !selected() {
            return;
        }
    };
    if !wait_for_toggle_gap(shared, driver, &selected) {
        return;
    }
    if !shared.client_connected() {
        info!("The iPad left before the virtual display was turned on — leaving it off");
        *shared.vdd_status.lock() = VddStatus::WaitingForClient;
        return;
    }
    // Quitting turns the display off once; an Enable after that would
    // outlive the app.
    let stopped_at = driver.now();
    while !shared.running.load(std::sync::atomic::Ordering::SeqCst) {
        if driver.now().duration_since(stopped_at) >= STOPPED_WAIT {
            info!("The stream stopped before the virtual display was turned on — leaving it off");
            *shared.vdd_status.lock() = VddStatus::WaitingForClient;
            return;
        }
        driver.sleep(WAIT_STEP);
    }
    driver.prepare();
    let before: Vec<String> = driver
        .outputs()
        .into_iter()
        .map(|output| output.device_name)
        .collect();
    let enabled = driver.set_enabled(true);
    shared.second_screen.lock().note_toggle(driver.now());
    if !enabled {
        warn!(
            second_screen_fallback = "enable_failed",
            "Virtual display could not be enabled (installer task missing?) — \
             capturing the primary display instead"
        );
        *shared.vdd_status.lock() = VddStatus::TaskFailed;
        return;
    }
    let result = wait_for_virtual_output(
        &before,
        timeout,
        || selected().then(|| driver.outputs()),
        || driver.now(),
        |duration| driver.sleep(duration),
    );
    match result {
        // The user chose another mode; that generation turns the display off.
        AttachResult::Cancelled => {}
        AttachResult::Attached(chosen) => {
            info!(
                device = %chosen.device_name,
                width = chosen.width,
                height = chosen.height,
                "Virtual display attached — capturing it"
            );
            *shared.vdd_output.lock() = Some(chosen.device_name);
            let mut status = shared.vdd_status.lock();
            *status = VddStatus::Active;
            shared.second_screen.lock().switch_pending = true;
        }
        AttachResult::TimedOut => {
            warn!(
                second_screen_fallback = "attach_timeout",
                timeout_ms = timeout.as_millis() as u64,
                "Virtual display did not attach in time — capturing the primary display instead"
            );
            // Enable succeeded but nothing attached: turn it back off so no
            // half-enabled device lingers as a phantom monitor.
            wait_for_toggle_gap(shared, driver, &|| true);
            driver.set_enabled(false);
            let mut status = shared.vdd_status.lock();
            *status = VddStatus::Failed;
            shared.second_screen.lock().note_toggle(driver.now());
        }
    }
}

/// Wait out [`MIN_TOGGLE_GAP`]. False when the mode changed meanwhile.
#[cfg(any(windows, test))]
fn wait_for_toggle_gap(
    shared: &SharedControl,
    driver: &impl Driver,
    selected: &dyn Fn() -> bool,
) -> bool {
    let delay = shared.second_screen.lock().toggle_delay(driver.now());
    if delay.is_zero() {
        return selected();
    }
    info!(
        wait_ms = delay.as_millis() as u64,
        "Waiting before changing the virtual display again, so Windows doesn't add and \
         remove it in quick succession"
    );
    let until = driver.now() + delay;
    while driver.now() < until {
        if !selected() {
            return false;
        }
        driver.sleep(until.saturating_duration_since(driver.now()).min(WAIT_STEP));
    }
    selected()
}

#[cfg(windows)]
struct WindowsDriver<'a> {
    shared: &'a SharedControl,
}

#[cfg(windows)]
impl Driver for WindowsDriver<'_> {
    fn outputs(&self) -> Vec<OutputInfo> {
        super::enumerate_outputs()
    }

    // Best effort: match the virtual display's mode list to the connected
    // iPad's panel BEFORE enabling the driver (it reads vdd_settings.xml at
    // enable time). Failure is non-fatal — the driver's built-in modes apply.
    fn prepare(&self) {
        if !crate::settings::SettingsFile::load().vdd_match_resolution {
            return;
        }
        let Some(info) = self.shared.session.lock().client_info() else {
            return;
        };
        let modes = crate::vdd_settings::modes_for_client(
            info.screen_px.0,
            info.screen_px.1,
            info.refresh_hz,
        );
        let path = std::path::Path::new(r"C:\VirtualDisplayDriver\vdd_settings.xml");
        match crate::vdd_settings::write_to(path, &modes) {
            Ok(()) => info!(
                ?modes,
                "Wrote virtual display mode list for the connected client"
            ),
            Err(error) => warn!(
                %error,
                "Could not write vdd_settings.xml — driver default modes apply"
            ),
        }
    }

    fn set_enabled(&self, enabled: bool) -> bool {
        crate::vdd::set_enabled(enabled, || {})
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::tests::connect_viewer;
    use crate::transport::link::LinkId;
    use std::cell::{Cell, RefCell};

    /// The iPad's connect timeout in 0.3.1 and 0.3.2: it disconnects when no
    /// frame has decoded this long after the handshake.
    const IPAD_TIMEOUT: Duration = Duration::from_secs(10);
    /// The 0.3.2 default attach wait after Enable.
    const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);

    fn secs(seconds: f64) -> Duration {
        Duration::from_secs_f64(seconds)
    }

    /// Runs `action` once, the first time the paused clock reaches `at`.
    struct Once {
        at: Duration,
        done: Cell<bool>,
    }

    impl Once {
        fn at(at: Duration) -> Self {
            Self {
                at,
                done: Cell::new(false),
            }
        }

        fn run(&self, elapsed: Duration, action: impl FnOnce()) {
            if elapsed >= self.at && !self.done.replace(true) {
                action();
            }
        }
    }

    fn second_screen_selected() -> SharedControl {
        let shared = SharedControl::new(9876, 15_000_000);
        shared
            .running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
        *shared.vdd_status.lock() = VddStatus::WaitingForClient;
        shared
    }

    fn disconnect(shared: &SharedControl) {
        assert!(shared.session.lock().link_lost(LinkId::Udp).client_lost);
    }

    fn output(name: &str, primary: bool) -> OutputInfo {
        OutputInfo {
            adapter_index: 0,
            output_index: 0,
            device_name: name.to_string(),
            width: 1920,
            height: 1080,
            left: if primary { 0 } else { -1920 },
            top: 0,
            is_primary: primary,
            adapter_name: "Test GPU".to_string(),
        }
    }

    /// A driver on a paused clock. Enable takes `enable_takes`, and the
    /// display attaches `attach_after` later, or never. `on_sleep` sees the
    /// clock each time the host waits, as the transport's ticks would.
    struct FakeDriver<'a> {
        start: Instant,
        elapsed: Cell<Duration>,
        enable_takes: Duration,
        attach_after: Option<Duration>,
        on_since: Cell<Option<Duration>>,
        toggles: RefCell<Vec<(Duration, bool)>>,
        on_sleep: &'a dyn Fn(Duration),
    }

    impl<'a> FakeDriver<'a> {
        fn new(
            enable_takes: Duration,
            attach_after: Option<Duration>,
            on_sleep: &'a dyn Fn(Duration),
        ) -> Self {
            Self {
                start: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
                enable_takes,
                attach_after,
                on_since: Cell::new(None),
                toggles: RefCell::new(Vec::new()),
                on_sleep,
            }
        }
    }

    impl Driver for FakeDriver<'_> {
        fn outputs(&self) -> Vec<OutputInfo> {
            let mut outputs = vec![output("primary", true)];
            let attached = self
                .on_since
                .get()
                .zip(self.attach_after)
                .is_some_and(|(on, after)| self.elapsed.get() >= on + after);
            if attached {
                outputs.push(output("virtual", false));
            }
            outputs
        }

        fn prepare(&self) {}

        fn set_enabled(&self, enabled: bool) -> bool {
            if enabled {
                self.sleep(self.enable_takes);
            }
            self.toggles
                .borrow_mut()
                .push((self.elapsed.get(), enabled));
            self.on_since.set(enabled.then(|| self.elapsed.get()));
            true
        }

        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }

        fn sleep(&self, duration: Duration) {
            // Report every 50 ms, as often as the transport ticks.
            let end = self.elapsed.get() + duration;
            while self.elapsed.get() < end {
                let step = (end - self.elapsed.get()).min(Duration::from_millis(50));
                self.elapsed.set(self.elapsed.get() + step);
                (self.on_sleep)(self.elapsed.get());
            }
        }
    }

    /// Joy's 0.3.2 log and the 0.3.1 field data: the display never attaches,
    /// the iPad sends its goodbye at 10 s, and USB dials again 1 s later.
    /// Here no transport step restarts the pipeline while the display
    /// starts, so the running generation keeps mirroring to the iPad, with
    /// an Enable task far slower than the iPad's whole timeout. The start is
    /// not cancelled by the goodbye, ends in one Enable and one Disable, and
    /// every later connection mirrors without touching the driver.
    #[test]
    fn a_display_that_never_attaches_mirrors_at_once_and_never_loops() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        let start = Instant::now();
        assert_eq!(next_step(&shared, start), Step::Attach);
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Attaching);

        let steps = RefCell::new(Vec::new());
        let (goodbye, redial) = (Once::at(IPAD_TIMEOUT), Once::at(IPAD_TIMEOUT + secs(1.0)));
        let on_sleep = |elapsed: Duration| {
            goodbye.run(elapsed, || disconnect(&shared));
            redial.run(elapsed, || connect_viewer(&shared));
            steps.borrow_mut().push(next_step(&shared, start + elapsed));
        };
        let driver = FakeDriver::new(secs(8.0), None, &on_sleep);
        attach(&shared, &driver, ATTACH_TIMEOUT);

        assert!(goodbye.done.get() && redial.done.get());
        assert!(
            steps.borrow().iter().all(|step| *step == Step::Nothing),
            "no restart while the display starts"
        );
        let toggles = driver.toggles.borrow().clone();
        assert_eq!(toggles.len(), 2, "{toggles:?}");
        assert_eq!(toggles[0], (secs(8.0), true));
        assert!(!toggles[1].1 && toggles[1].0 - toggles[0].0 >= MIN_TOGGLE_GAP);
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
        assert_eq!(
            remembered_failure(*shared.vdd_status.lock()),
            Some("attach_timeout")
        );

        // Reconnects for the next minute stay on the mirror.
        for second in 0..60 {
            disconnect(&shared);
            connect_viewer(&shared);
            let now = driver.now() + Duration::from_secs(second);
            assert_eq!(next_step(&shared, now), Step::Nothing);
        }
        // Try again starts it once more at the next tick.
        shared.retry_second_screen();
        assert_eq!(next_step(&shared, driver.now()), Step::Attach);
    }

    #[test]
    fn the_stream_moves_to_the_second_screen_once_it_attaches() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        let steps = RefCell::new(Vec::new());
        let on_sleep = |_| steps.borrow_mut().push(next_step(&shared, Instant::now()));
        let driver = FakeDriver::new(secs(3.0), Some(secs(0.5)), &on_sleep);
        attach(&shared, &driver, ATTACH_TIMEOUT);
        assert!(steps.borrow().iter().all(|step| *step == Step::Nothing));
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
        assert_eq!(shared.vdd_output.lock().as_deref(), Some("virtual"));
        assert_eq!(driver.toggles.borrow().len(), 1);
        let now = driver.now();
        assert_eq!(next_step(&shared, now), Step::Switch);
        assert_eq!(next_step(&shared, now), Step::Nothing, "one switch");
    }

    #[test]
    fn a_quick_reconnect_keeps_the_second_screen_and_a_long_absence_releases_it() {
        let shared = second_screen_selected();
        *shared.vdd_status.lock() = VddStatus::Active;
        connect_viewer(&shared);
        let start = Instant::now();
        assert_eq!(next_step(&shared, start), Step::Nothing);

        disconnect(&shared);
        let mut elapsed = Duration::ZERO;
        while elapsed < secs(9.9) {
            assert_eq!(next_step(&shared, start + elapsed), Step::Nothing);
            assert!(shared.second_screen.lock().keep_without_viewer());
            elapsed += secs(0.25);
        }
        // The iPad comes back within the grace period, then leaves again.
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, start + elapsed), Step::Nothing);
        disconnect(&shared);
        let left = start + elapsed + secs(0.25);
        assert_eq!(next_step(&shared, left), Step::Nothing);
        assert_eq!(
            next_step(&shared, left + RELEASE_GRACE - secs(0.25)),
            Step::Nothing
        );
        assert_eq!(next_step(&shared, left + RELEASE_GRACE), Step::Release);
        assert!(!shared.second_screen.lock().keep_without_viewer());
        assert_eq!(
            next_step(&shared, left + RELEASE_GRACE + secs(1.0)),
            Step::Nothing,
            "one release"
        );
    }

    #[test]
    fn the_display_is_never_turned_on_within_the_gap_after_it_was_turned_off() {
        let shared = second_screen_selected();
        let on_sleep = |_| {};
        let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
        // The grace period ended and the next generation turned it off.
        shared.second_screen.lock().note_toggle(driver.now());
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, driver.now()), Step::Attach);
        attach(&shared, &driver, ATTACH_TIMEOUT);
        let toggles = driver.toggles.borrow().clone();
        assert_eq!(toggles, vec![(MIN_TOGGLE_GAP + secs(3.0), true)]);
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
    }

    #[test]
    fn a_viewer_leaving_during_the_gap_leaves_the_display_off() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        let goodbye = Once::at(secs(2.0));
        let on_sleep = |elapsed: Duration| goodbye.run(elapsed, || disconnect(&shared));
        let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
        shared.second_screen.lock().note_toggle(driver.now());
        attach(&shared, &driver, ATTACH_TIMEOUT);
        assert!(goodbye.done.get());
        assert!(driver.toggles.borrow().is_empty());
        assert_eq!(*shared.vdd_status.lock(), VddStatus::WaitingForClient);
    }

    #[test]
    fn a_quit_before_the_enable_leaves_the_display_off() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        shared.stop();
        let on_sleep = |_| {};
        let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
        attach(&shared, &driver, ATTACH_TIMEOUT);
        assert!(driver.toggles.borrow().is_empty());
        assert_eq!(*shared.vdd_status.lock(), VddStatus::WaitingForClient);
    }

    #[test]
    fn changing_the_mode_ends_the_start_and_clears_the_timers() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        let switch = Once::at(secs(5.0));
        let on_sleep = |elapsed: Duration| {
            switch.run(elapsed, || {
                *shared.capture_target.lock() = CaptureTarget::PrimaryAuto
            })
        };
        let driver = FakeDriver::new(secs(3.0), None, &on_sleep);
        attach(&shared, &driver, ATTACH_TIMEOUT);
        assert!(
            driver.elapsed.get() < secs(5.5),
            "{:?}",
            driver.elapsed.get()
        );
        assert_eq!(driver.toggles.borrow().len(), 1, "only the Enable");
        assert_eq!(next_step(&shared, driver.now()), Step::Nothing);
    }
}
