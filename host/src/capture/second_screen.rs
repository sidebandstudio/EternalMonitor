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
//! already running. One worker thread here turns the display on and off, fed
//! by a queue, so capture never waits for the driver, and the gap between
//! changes, a start cancelled by a mode change and quitting are decided in
//! one place. Once the display attaches the transport restarts the pipeline
//! onto it, and the second screen must deliver a real image within
//! [`PROVE_TIMEOUT`] of that switch, across any restarts meanwhile;
//! otherwise the stream goes back to the mirror. A start that failed is
//! remembered: later connections stay on the mirror until the user clicks
//! Try again, restarts the stream, or changes the mode.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
#[cfg(any(windows, test))]
use tracing::info;
use tracing::warn;

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

/// How long the second screen has, from the switch onto it, to capture a
/// real image. Until then the iPad keeps showing the last mirror frame; a
/// display that fails to duplicate, or only ever times out, goes back to the
/// mirror after this.
pub const PROVE_TIMEOUT: Duration = Duration::from_secs(5);

/// What proves the second screen. True: an image read back for the encoder
/// from a frame Windows presented (`LastPresentTime != 0`). False: any frame
/// the duplication delivers, even one that only moves the pointer.
#[cfg(windows)]
pub(crate) const PROOF_NEEDS_IMAGE: bool = true;

#[cfg(any(windows, test))]
const WAIT_STEP: Duration = Duration::from_millis(100);

/// How long a stopped pipeline may take to come back before the display is
/// left off: a restart joins the old generation for up to 5 s, while a quit
/// or Stop leaves it stopped.
#[cfg(any(windows, test))]
const STOPPED_WAIT: Duration = Duration::from_secs(6);

/// How long quitting waits for the worker to exit after the last Enable.
const QUIT_JOIN: Duration = Duration::from_secs(5);

/// How long quitting waits for an Enable in flight: the driver task's own
/// limit is 30 s.
const QUIT_ENABLE_WAIT: Duration = Duration::from_secs(35);

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
    /// The display showed no picture: restart onto the mirror, turn it off.
    Fallback(&'static str),
}

/// The second screen being streamed, before its first image. It outlives the
/// capture generation that started it: a restart meanwhile keeps the
/// deadline and any capture failure.
#[derive(Debug, Clone, Copy)]
struct Proving {
    generation: u64,
    since: Instant,
    capture_failed: bool,
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
    /// A start is claimed: its Attach is queued or running.
    attaching: bool,
    proving: Option<Proving>,
    /// Why the second screen last failed, for the log and the backend.
    failure: Option<&'static str>,
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
                attaching: self.attaching,
                ..State::default()
            };
            return Step::Nothing;
        }
        if let Some(reason) = self.fail_unproven(status, now) {
            return Step::Fallback(reason);
        }
        match *status {
            VddStatus::WaitingForClient if connected => {
                *status = VddStatus::Attaching;
                self.attaching = true;
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

    /// End a second screen that failed to prove itself: a capture error, or
    /// no image within [`PROVE_TIMEOUT`] of the switch. Returns why.
    fn fail_unproven(&mut self, status: &mut VddStatus, now: Instant) -> Option<&'static str> {
        let proving = self.proving?;
        let reason = if proving.capture_failed {
            "duplication_failed"
        } else if now.duration_since(proving.since) >= PROVE_TIMEOUT {
            "no_frames"
        } else {
            return None;
        };
        *self = State {
            last_toggle: self.last_toggle,
            attaching: self.attaching,
            failure: Some(reason),
            ..State::default()
        };
        *status = VddStatus::Failed;
        Some(reason)
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

/// A new capture generation ends a second screen that already failed to
/// prove itself, before anything else: a supervisor restart (a capture
/// error that ended the stage, or the watchdog) must not erase the failure.
/// Returns why it failed.
#[cfg(any(windows, test))]
pub(crate) fn take_unproven(shared: &SharedControl, now: Instant) -> Option<&'static str> {
    let mut status = shared.vdd_status.lock();
    shared.second_screen.lock().fail_unproven(&mut status, now)
}

/// Whether the second screen is still proving itself. Nothing is sent from
/// it meanwhile, so the watchdog leaves the verdict to [`next_step`] and
/// [`take_unproven`].
pub(crate) fn proving(shared: &SharedControl) -> bool {
    shared.second_screen.lock().proving.is_some()
}

/// Why connections mirror instead of extending, after a failed start. The
/// value appears as `second_screen_fallback=` in the log, which the
/// diagnostics upload sends as is.
pub(crate) fn remembered_failure(shared: &SharedControl) -> Option<&'static str> {
    let status = *shared.vdd_status.lock();
    let failure = shared.second_screen.lock().failure;
    match status {
        VddStatus::Failed => Some(failure.unwrap_or("attach_timeout")),
        VddStatus::TaskFailed => Some(failure.unwrap_or("enable_failed")),
        _ => None,
    }
}

/// The user asked to try again: a failed start, or one whose thread is gone,
/// is tried once more at the next connection.
pub(crate) fn retry(shared: &SharedControl) {
    let mut status = shared.vdd_status.lock();
    let mut state = shared.second_screen.lock();
    let stale = *status == VddStatus::Attaching && !state.attaching;
    if stale || matches!(*status, VddStatus::Failed | VddStatus::TaskFailed) {
        *status = VddStatus::WaitingForClient;
        state.failure = None;
    }
}

/// The display attached and is the stream's next source.
pub fn note_attached(shared: &SharedControl, device_name: String) {
    *shared.vdd_output.lock() = Some(device_name);
    let mut status = shared.vdd_status.lock();
    *status = VddStatus::Active;
    let mut state = shared.second_screen.lock();
    state.switch_pending = true;
    state.proving = None;
}

/// Capture `generation` streams the second screen from `now`. The first
/// generation after the switch starts the [`PROVE_TIMEOUT`] deadline; a
/// later one inherits it, and any capture failure, until an image arrives.
pub fn start_proving(shared: &SharedControl, generation: u64, now: Instant) {
    let mut state = shared.second_screen.lock();
    match state.proving.as_mut() {
        Some(proving) => proving.generation = generation,
        None => {
            state.proving = Some(Proving {
                generation,
                since: now,
                capture_failed: false,
            })
        }
    }
}

/// Capture `generation` captured a real image, ready for the encoder.
pub fn note_image(shared: &SharedControl, generation: u64) {
    let mut state = shared.second_screen.lock();
    if let Some(proving) = state.proving.filter(|p| p.generation == generation) {
        state.proving = None;
        tracing::info!(
            after_ms = proving.since.elapsed().as_millis() as u64,
            "Second screen captured its first image — streaming it"
        );
    }
}

/// Capture `generation` failed to open or read its display, for example
/// DuplicateOutput returned DXGI_ERROR_UNSUPPORTED. On the second screen,
/// before its first image, that ends the attempt at the next tick, or when
/// the next capture generation starts.
pub fn note_capture_error(shared: &SharedControl, generation: u64) {
    let mut state = shared.second_screen.lock();
    if let Some(proving) = state.proving.as_mut() {
        if proving.generation == generation {
            proving.capture_failed = true;
        }
    }
}

/// What the virtual display worker is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    /// A viewer arrived: turn the display on beside the mirror.
    Attach,
    /// Turn off the display this host turned on, unless a start claimed it
    /// again since.
    Release,
}

/// The one thread that turns the virtual display on and off, and its queue.
#[derive(Default)]
pub(crate) struct Worker {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Held across each Enable. Quitting takes it after setting
    /// [`Queue::quitting`], so no Enable runs or starts once it has.
    enable: Mutex<()>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Default)]
struct Queue {
    commands: VecDeque<Command>,
    quitting: bool,
}

/// Ask the worker to change the display. Returns at once.
pub(crate) fn send(shared: &SharedControl, command: Command) {
    if !driver_available() {
        without_driver(shared, command);
        return;
    }
    {
        let mut queue = shared.vdd_worker.queue.lock();
        if queue.quitting {
            return;
        }
        queue.commands.push_back(command);
    }
    shared.vdd_worker.wake.notify_one();
    // Unit tests run the queue themselves, with a fake driver.
    #[cfg(windows)]
    if cfg!(test) {
        return;
    }
    #[cfg(windows)]
    if let Err(error) = spawn_worker(shared) {
        warn!(%error, "Could not start the virtual display worker");
        shared.vdd_worker.queue.lock().commands.clear();
        if command == Command::Attach {
            warn!(
                second_screen_fallback = "enable_failed",
                "Could not start the second screen — mirroring the primary display instead"
            );
            fail_start(shared, "enable_failed");
        }
    }
}

/// Unit tests queue commands too; they never reach the installed driver
/// tasks.
fn driver_available() -> bool {
    #[cfg(windows)]
    {
        cfg!(test) || !super::synthetic_requested()
    }
    #[cfg(not(windows))]
    {
        cfg!(test)
    }
}

fn without_driver(shared: &SharedControl, command: Command) {
    match command {
        Command::Attach => {
            tracing::info!(
                second_screen_fallback = "no_driver",
                "No virtual display driver with this capture source — mirroring the primary display"
            );
            fail_start(shared, "no_driver");
        }
        Command::Release => *shared.vdd_output.lock() = None,
    }
}

fn fail_start(shared: &SharedControl, reason: &'static str) {
    let mut status = shared.vdd_status.lock();
    *status = VddStatus::TaskFailed;
    let mut state = shared.second_screen.lock();
    state.attaching = false;
    state.failure = Some(reason);
}

#[cfg(windows)]
fn spawn_worker(shared: &SharedControl) -> std::io::Result<()> {
    let mut thread = shared.vdd_worker.thread.lock();
    if thread.is_none() {
        let worker = shared.clone();
        *thread = Some(
            std::thread::Builder::new()
                .name("second-screen".into())
                .spawn(move || {
                    run(
                        &worker,
                        &WindowsDriver { shared: &worker },
                        super::vdd_attach_timeout(),
                        true,
                    )
                })?,
        );
    }
    Ok(())
}

/// Quitting: once this returns no Enable runs or will start, so the caller's
/// Disable is the last change. Waits for an Enable in flight, then for the
/// worker to exit, both bounded.
pub fn shut_down(shared: &SharedControl) {
    let worker = &shared.vdd_worker;
    worker.queue.lock().quitting = true;
    worker.wake.notify_all();
    if worker.enable.try_lock_for(QUIT_ENABLE_WAIT).is_none() {
        warn!("An Enable of the virtual display is still running — turning it off after it");
    }
    let Some(thread) = worker.thread.lock().take() else {
        return;
    };
    let deadline = Instant::now() + QUIT_JOIN;
    while !thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if thread.is_finished() {
        let _ = thread.join();
    } else {
        warn!("The virtual display worker did not stop in time");
    }
}

#[cfg(any(windows, test))]
fn quitting(shared: &SharedControl) -> bool {
    shared.vdd_worker.queue.lock().quitting
}

/// The worker: one command at a time, until the app quits. With `wait`
/// false it returns once the queue is empty, for tests on a paused clock.
#[cfg(any(windows, test))]
pub(crate) fn run(
    shared: &SharedControl,
    driver: &impl Driver,
    attach_timeout: Duration,
    wait: bool,
) {
    loop {
        let command = {
            let mut queue = shared.vdd_worker.queue.lock();
            loop {
                if queue.quitting {
                    return;
                }
                if let Some(command) = queue.commands.pop_front() {
                    break command;
                }
                if !wait {
                    return;
                }
                shared.vdd_worker.wake.wait(&mut queue);
            }
        };
        match command {
            Command::Attach => attach(shared, driver, attach_timeout),
            Command::Release => release(shared, driver),
        }
    }
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

/// However [`attach`] ends, its claim ends with it: a start that did not
/// finish leaves the status for the next tick to claim again.
#[cfg(any(windows, test))]
struct AttachExit<'a>(&'a SharedControl);

#[cfg(any(windows, test))]
impl Drop for AttachExit<'_> {
    fn drop(&mut self) {
        let mut status = self.0.vdd_status.lock();
        if *status == VddStatus::Attaching {
            *status = VddStatus::WaitingForClient;
        }
        self.0.second_screen.lock().attaching = false;
    }
}

/// Turn the display on, wait for it to attach, and record the result. A
/// viewer leaving does not cancel it; a change of mode or quitting does.
/// Capture keeps mirroring throughout.
#[cfg(any(windows, test))]
pub(crate) fn attach(shared: &SharedControl, driver: &impl Driver, timeout: Duration) {
    let _exit = AttachExit(shared);
    let selected =
        || *shared.capture_target.lock() == CaptureTarget::VirtualExtended && !quitting(shared);
    if !selected() {
        return;
    }
    // Still on from before (its release saw this start coming): reuse it,
    // with no change to wait for.
    let outputs = driver.outputs();
    let owned = shared.vdd_output.lock().clone();
    if let Some(name) = owned.filter(|name| {
        outputs
            .iter()
            .any(|output| !output.is_primary && &output.device_name == name)
    }) {
        info!(device = %name, "Virtual display still attached — capturing it");
        note_attached(shared, name);
        return;
    }
    if !wait_for_toggle_gap(shared, driver, &selected) {
        return;
    }
    if !shared.client_connected() {
        info!("The iPad left before the virtual display was turned on — leaving it off");
        return;
    }
    let stopped_at = driver.now();
    while !shared.running.load(std::sync::atomic::Ordering::SeqCst) {
        if !selected() || driver.now().duration_since(stopped_at) >= STOPPED_WAIT {
            info!("The stream stopped before the virtual display was turned on — leaving it off");
            return;
        }
        driver.sleep(WAIT_STEP);
    }
    let outputs = driver.outputs();
    driver.prepare();
    let before: Vec<String> = outputs
        .into_iter()
        .map(|output| output.device_name)
        .collect();
    let Some(enabled) = enable(shared, driver) else {
        info!("Quitting before the virtual display was turned on — leaving it off");
        return;
    };
    if !enabled {
        warn!(
            second_screen_fallback = "enable_failed",
            "Virtual display could not be enabled (installer task missing?) — \
             capturing the primary display instead"
        );
        let mut status = shared.vdd_status.lock();
        *status = VddStatus::TaskFailed;
        shared.second_screen.lock().failure = Some("enable_failed");
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
        AttachResult::Cancelled if quitting(shared) => {
            info!("Quitting while the virtual display started — the quit turns it off");
        }
        AttachResult::Cancelled => {
            // The user chose another mode while the display came up. Nothing
            // records it as ours, so turn it off, after the gap like every
            // change: the mirror is already streaming.
            info!("Display mode changed while the virtual display started — turning it off");
            disable(shared, driver);
        }
        AttachResult::Attached(chosen) => {
            info!(
                device = %chosen.device_name,
                width = chosen.width,
                height = chosen.height,
                "Virtual display attached — capturing it"
            );
            note_attached(shared, chosen.device_name);
        }
        AttachResult::TimedOut => {
            warn!(
                second_screen_fallback = "attach_timeout",
                timeout_ms = timeout.as_millis() as u64,
                "Virtual display did not attach in time — capturing the primary display instead"
            );
            // Enable succeeded but nothing attached: turn it back off so no
            // half-enabled device lingers as a phantom monitor.
            disable(shared, driver);
            let mut status = shared.vdd_status.lock();
            *status = VddStatus::Failed;
            shared.second_screen.lock().failure = Some("attach_timeout");
        }
    }
}

/// The only Enable: never once quitting has begun, and quitting waits for
/// one in flight. None when quitting.
#[cfg(any(windows, test))]
fn enable(shared: &SharedControl, driver: &impl Driver) -> Option<bool> {
    let _enable = shared.vdd_worker.enable.lock();
    if quitting(shared) {
        return None;
    }
    let enabled = driver.set_enabled(true);
    shared.second_screen.lock().note_toggle(driver.now());
    Some(enabled)
}

/// Turn the display off after the gap. Quitting skips it: the quit turns it
/// off at once.
#[cfg(any(windows, test))]
fn disable(shared: &SharedControl, driver: &impl Driver) {
    if wait_for_toggle_gap(shared, driver, &|| !quitting(shared)) {
        driver.set_enabled(false);
        shared.second_screen.lock().note_toggle(driver.now());
    }
    *shared.vdd_output.lock() = None;
}

/// Turn off a display this host turned on, unless a start claimed it since.
/// Only the claim counts, not the status: a capture generation may publish
/// the status after asking for this.
#[cfg(any(windows, test))]
pub(crate) fn release(shared: &SharedControl, driver: &impl Driver) {
    let wanted = || {
        *shared.capture_target.lock() == CaptureTarget::VirtualExtended
            && shared.second_screen.lock().attaching
    };
    if shared.vdd_output.lock().is_none() || wanted() {
        return;
    }
    if !wait_for_toggle_gap(shared, driver, &|| !wanted() && !quitting(shared)) {
        return;
    }
    driver.set_enabled(false);
    *shared.vdd_output.lock() = None;
    shared.second_screen.lock().note_toggle(driver.now());
    info!("Virtual display turned off");
}

/// Wait out [`MIN_TOGGLE_GAP`]. False when `go_on` turned false meanwhile.
#[cfg(any(windows, test))]
fn wait_for_toggle_gap(
    shared: &SharedControl,
    driver: &impl Driver,
    go_on: &dyn Fn() -> bool,
) -> bool {
    let delay = shared.second_screen.lock().toggle_delay(driver.now());
    if delay.is_zero() {
        return go_on();
    }
    info!(
        wait_ms = delay.as_millis() as u64,
        "Waiting before changing the virtual display again, so Windows doesn't add and \
         remove it in quick succession"
    );
    let until = driver.now() + delay;
    while driver.now() < until {
        if !go_on() {
            return false;
        }
        driver.sleep(until.saturating_duration_since(driver.now()).min(WAIT_STEP));
    }
    go_on()
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
    use crate::capture::reconcile_virtual_display;
    use crate::capture::tests::connect_viewer;
    use crate::transport::link::LinkId;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::Ordering;

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
        assert_eq!(remembered_failure(&shared), Some("attach_timeout"));

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

    /// Review round 2, defect 5: Enable done at 3 s, Mirror chosen at 5 s.
    /// The Disable waited for nothing and came 2 s after the Enable; now it
    /// waits out the gap, like every change, while the mirror streams.
    #[test]
    fn a_mode_change_after_the_enable_turns_the_display_back_off() {
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
        let toggles = driver.toggles.borrow().clone();
        assert_eq!(
            toggles.iter().map(|(_, on)| *on).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(toggles[0].0, secs(3.0));
        assert!(toggles[1].0 - toggles[0].0 >= MIN_TOGGLE_GAP, "{toggles:?}");
        assert_eq!(*shared.vdd_output.lock(), None);
        assert_eq!(*shared.vdd_status.lock(), VddStatus::WaitingForClient);
        assert_eq!(next_step(&shared, driver.now()), Step::Nothing);
    }

    /// Review round 1, defect 3: Mirror then Second screen again during the
    /// start, before any capture generation reconciled Mirror. The start must
    /// not stay claimed with nobody running it.
    #[test]
    fn a_mode_change_during_the_start_never_leaves_it_claimed() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        let on_sleep = |_| {};
        let driver = FakeDriver::new(secs(3.0), None, &on_sleep);
        // A change a moment ago: the start waits out the gap first.
        shared.second_screen.lock().note_toggle(driver.now());
        assert_eq!(next_step(&shared, driver.now()), Step::Attach);
        let mirror = Once::at(secs(2.0));
        let on_sleep = |elapsed: Duration| {
            mirror.run(elapsed, || {
                *shared.capture_target.lock() = CaptureTarget::PrimaryAuto
            })
        };
        let driver = FakeDriver {
            on_sleep: &on_sleep,
            ..driver
        };
        attach(&shared, &driver, ATTACH_TIMEOUT);
        assert!(mirror.done.get());
        assert!(driver.toggles.borrow().is_empty());
        *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
        assert_eq!(*shared.vdd_status.lock(), VddStatus::WaitingForClient);
        assert_eq!(next_step(&shared, driver.now()), Step::Attach);
    }

    #[test]
    fn try_again_recovers_a_start_whose_thread_is_gone() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        // Its thread is running: Try again leaves it alone.
        shared.retry_second_screen();
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Attaching);
        // A claim with no thread behind it.
        shared.second_screen.lock().attaching = false;
        shared.retry_second_screen();
        assert_eq!(*shared.vdd_status.lock(), VddStatus::WaitingForClient);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
    }

    /// Review round 1, defect 1: the switch happened, then the capture
    /// generation streaming the display only ever timed out (or read the
    /// black placeholder). It goes back to the mirror, and stays there.
    #[test]
    fn a_second_screen_that_never_shows_an_image_goes_back_to_the_mirror() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        let start = Instant::now();
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 7, start);
        let mut elapsed = Duration::ZERO;
        while elapsed < PROVE_TIMEOUT {
            assert_eq!(next_step(&shared, start + elapsed), Step::Nothing);
            elapsed += secs(0.25);
        }
        assert_eq!(
            next_step(&shared, start + PROVE_TIMEOUT),
            Step::Fallback("no_frames")
        );
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
        assert_eq!(remembered_failure(&shared), Some("no_frames"));
        // Acquisition only timing out never sends the black placeholder.
        assert!(!super::super::keepalive_allowed(false, true));
        assert!(super::super::keepalive_allowed(false, false));
        assert!(super::super::keepalive_allowed(true, true));
        // Later connections mirror without touching the driver.
        for second in 1..30 {
            disconnect(&shared);
            connect_viewer(&shared);
            let now = start + PROVE_TIMEOUT + Duration::from_secs(second);
            assert_eq!(next_step(&shared, now), Step::Nothing);
        }
        let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        assert_eq!(
            reconcile_virtual_display(&CaptureTarget::VirtualExtended, &shared, generation),
            Some(CaptureTarget::PrimaryAuto)
        );
    }

    #[test]
    fn a_duplication_error_on_the_second_screen_goes_back_to_the_mirror_at_once() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        let start = Instant::now();
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 7, start);
        // An older generation's error is not this display's.
        note_capture_error(&shared, 6);
        assert_eq!(next_step(&shared, start), Step::Nothing);
        // DuplicateOutput returned DXGI_ERROR_UNSUPPORTED.
        note_capture_error(&shared, 7);
        assert_eq!(
            next_step(&shared, start),
            Step::Fallback("duplication_failed")
        );
        assert_eq!(remembered_failure(&shared), Some("duplication_failed"));
    }

    #[test]
    fn a_second_screen_that_shows_an_image_stays() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        let start = Instant::now();
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 7, start);
        note_image(&shared, 6);
        note_image(&shared, 7);
        note_capture_error(&shared, 7);
        for second in 0..60 {
            let now = start + Duration::from_secs(second);
            assert_eq!(next_step(&shared, now), Step::Nothing);
        }
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
    }

    #[test]
    fn a_fallback_turns_the_display_off_after_the_gap() {
        let shared = second_screen_selected();
        let on_sleep = |_| {};
        let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
        note_attached(&shared, "virtual".into());
        shared.second_screen.lock().note_toggle(driver.now());
        *shared.vdd_status.lock() = VddStatus::Failed;
        release(&shared, &driver);
        assert_eq!(*driver.toggles.borrow(), vec![(MIN_TOGGLE_GAP, false)]);
        assert_eq!(*shared.vdd_output.lock(), None);
        // A second request finds nothing left to turn off.
        release(&shared, &driver);
        assert_eq!(driver.toggles.borrow().len(), 1);
    }

    /// Review round 1, defect 2: no capture generation may wait for the
    /// driver, whatever the status, the mode or the viewer. Before, a
    /// generation that read `WaitingForClient` (Try again had just reset it)
    /// waited for the lock behind the attach thread's whole Enable.
    #[test]
    fn mirror_startup_never_waits_for_the_driver_lock() {
        let shared = second_screen_selected();
        let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        // What a slow Enable on the worker holds.
        let _held = shared.vdd_worker.enable.lock();
        for target in [
            CaptureTarget::VirtualExtended,
            CaptureTarget::PrimaryAuto,
            CaptureTarget::Output("physical".into()),
        ] {
            for status in [
                VddStatus::Inactive,
                VddStatus::WaitingForClient,
                VddStatus::Attaching,
                VddStatus::Active,
                VddStatus::Failed,
                VddStatus::TaskFailed,
            ] {
                for connected in [false, true] {
                    if connected != shared.client_connected() {
                        if connected {
                            connect_viewer(&shared);
                        } else {
                            disconnect(&shared);
                        }
                    }
                    *shared.vdd_status.lock() = status;
                    let (done, finished) = std::sync::mpsc::channel();
                    let worker = shared.clone();
                    let target = target.clone();
                    std::thread::spawn(move || {
                        let _ = done.send(reconcile_virtual_display(&target, &worker, generation));
                    });
                    let chosen = finished
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap_or_else(|_| panic!("{status:?} connected={connected} waited"));
                    assert!(chosen.is_some(), "{status:?}");
                }
            }
        }
    }

    /// The same, with the real attach thread: Try again after a failure, its
    /// thread blocked in a slow Enable, and a new capture generation.
    #[test]
    fn try_again_during_a_slow_enable_still_mirrors_at_once() {
        /// Blocks in Enable until released, on a paused clock otherwise.
        struct SlowEnable {
            entered: std::sync::mpsc::SyncSender<()>,
            release: std::sync::mpsc::Receiver<()>,
            start: Instant,
            elapsed: Cell<Duration>,
        }
        impl Driver for SlowEnable {
            fn outputs(&self) -> Vec<OutputInfo> {
                vec![output("primary", true)]
            }
            fn prepare(&self) {}
            fn set_enabled(&self, enabled: bool) -> bool {
                if enabled {
                    let _ = self.entered.send(());
                    let _ = self.release.recv();
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
        let shared = second_screen_selected();
        connect_viewer(&shared);
        *shared.vdd_status.lock() = VddStatus::Failed;
        shared.retry_second_screen();
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        let (entered_tx, entered) = std::sync::mpsc::sync_channel(1);
        let (release, release_rx) = std::sync::mpsc::channel();
        let worker = shared.clone();
        let attach_thread = std::thread::spawn(move || {
            let driver = SlowEnable {
                entered: entered_tx,
                release: release_rx,
                start: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
            };
            attach(&worker, &driver, Duration::ZERO);
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        let (done, finished) = std::sync::mpsc::channel();
        let capture = shared.clone();
        std::thread::spawn(move || {
            let _ = done.send(reconcile_virtual_display(
                &CaptureTarget::VirtualExtended,
                &capture,
                generation,
            ));
        });
        assert_eq!(
            finished.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some(CaptureTarget::PrimaryAuto)
        );
        release.send(()).unwrap();
        attach_thread.join().unwrap();
        // No output ever appeared: the start failed and is remembered.
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
    }

    /// Review round 2, defect 1: the generation streaming the second screen
    /// was replaced before the transport's tick ended its proof (by the
    /// watchdog, or a capture error that ended the stage). The new generation
    /// started the 5 s over and forgot the capture error.
    #[test]
    fn a_restart_keeps_the_proof_deadline_and_its_capture_error() {
        let start = Instant::now();
        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 7, start);
        start_proving(&shared, 8, start + secs(4.0));
        assert_eq!(next_step(&shared, start + secs(4.9)), Step::Nothing);
        assert_eq!(
            next_step(&shared, start + PROVE_TIMEOUT),
            Step::Fallback("no_frames")
        );

        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 7, start);
        note_capture_error(&shared, 7);
        start_proving(&shared, 8, start + secs(1.0));
        assert_eq!(
            next_step(&shared, start + secs(1.0)),
            Step::Fallback("duplication_failed")
        );
    }

    /// Review round 2, defect 1: nothing is sent from the second screen
    /// before its first image, so the watchdog's 5 s without a frame came
    /// before the proof's own deadline and restarted the pipeline.
    #[test]
    fn the_watchdog_leaves_a_second_screen_proving_itself_alone() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        let machine = crate::supervisor::Machine::new(1);
        let now_ms = 100_000;
        shared.hb_capture_loop_ms.store(now_ms, Ordering::Relaxed);
        shared
            .hb_capture_frame_ms
            .store(now_ms - 5_500, Ordering::Relaxed);
        shared
            .hb_encode_frame_ms
            .store(now_ms - 5_500, Ordering::Relaxed);
        note_attached(&shared, "virtual".into());
        start_proving(&shared, 7, Instant::now());
        assert_eq!(
            crate::supervisor::wedge_reason_at(&shared, &machine, now_ms),
            None
        );
        // After its first image a stalled second screen is a wedge again.
        note_image(&shared, 7);
        assert!(crate::supervisor::wedge_reason_at(&shared, &machine, now_ms).is_some());
    }

    /// Review round 2, defect 1: when no tick ended the proof before the
    /// restart, the next capture generation does, before anything else.
    #[test]
    fn the_next_capture_generation_ends_a_second_screen_that_failed() {
        let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        let previous = generation.wrapping_sub(1);
        let long_ago = Instant::now().checked_sub(secs(6.0)).unwrap();
        for (failed_at, capture_error, reason) in [
            (Instant::now(), true, "duplication_failed"),
            (long_ago, false, "no_frames"),
        ] {
            let shared = second_screen_selected();
            connect_viewer(&shared);
            note_attached(&shared, "virtual".into());
            assert_eq!(next_step(&shared, failed_at), Step::Switch);
            start_proving(&shared, previous, failed_at);
            if capture_error {
                note_capture_error(&shared, previous);
            }
            assert_eq!(
                reconcile_virtual_display(&CaptureTarget::VirtualExtended, &shared, generation),
                Some(CaptureTarget::PrimaryAuto)
            );
            assert_eq!(*shared.vdd_status.lock(), VddStatus::Failed);
            assert_eq!(remembered_failure(&shared), Some(reason));
            assert!(!proving(&shared));
            // The display is turned off, and the tick finds nothing to do.
            let on_sleep = |_| {};
            let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
            run(&shared, &driver, ATTACH_TIMEOUT, false);
            assert_eq!(*driver.toggles.borrow(), vec![(Duration::ZERO, false)]);
            assert_eq!(next_step(&shared, Instant::now()), Step::Nothing);
        }
    }

    /// Review round 2, defect 2: the proof was noted before the readback, so
    /// a frame that never reached the encoder counted, and the capture error
    /// that followed found no proof left to fail.
    #[test]
    fn only_an_image_read_back_proves_the_second_screen() {
        use crate::capture::read_back_image;
        let start = Instant::now();
        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 7, start);
        let mut seen_image = false;
        // The texture could not be cast: nothing for the encoder.
        assert_eq!(
            read_back_image(&shared, 7, &mut seen_image, || Ok::<Option<u8>, &str>(None)),
            Ok(None)
        );
        // Map failed.
        assert_eq!(
            read_back_image(&shared, 7, &mut seen_image, || Err::<Option<u8>, _>("map")),
            Err("map")
        );
        assert!(proving(&shared) && !seen_image);
        note_capture_error(&shared, 7);
        assert_eq!(
            next_step(&shared, start),
            Step::Fallback("duplication_failed")
        );

        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        assert_eq!(next_step(&shared, start), Step::Switch);
        start_proving(&shared, 7, start);
        let mut seen_image = false;
        assert_eq!(
            read_back_image(&shared, 7, &mut seen_image, || Ok::<_, ()>(Some(1u8))),
            Ok(Some(1))
        );
        assert!(!proving(&shared) && seen_image);
    }

    /// A driver for a worker thread. Prepare or Enable can be held until
    /// the test lets it go.
    struct Gated {
        hold_prepare: bool,
        hold_enable: bool,
        entered: std::sync::mpsc::SyncSender<&'static str>,
        go: std::sync::mpsc::Receiver<()>,
        toggles: std::sync::Arc<Mutex<Vec<bool>>>,
        start: Instant,
        elapsed: Cell<Duration>,
    }

    impl Driver for Gated {
        fn outputs(&self) -> Vec<OutputInfo> {
            vec![output("primary", true)]
        }
        fn prepare(&self) {
            if self.hold_prepare {
                let _ = self.entered.send("prepare");
                let _ = self.go.recv();
            }
        }
        fn set_enabled(&self, enabled: bool) -> bool {
            if enabled && self.hold_enable {
                let _ = self.entered.send("enable");
                let _ = self.go.recv();
            }
            self.toggles.lock().push(enabled);
            true
        }
        fn now(&self) -> Instant {
            self.start + self.elapsed.get()
        }
        fn sleep(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
        }
    }

    type GatedWorker = (
        std::sync::mpsc::Receiver<&'static str>,
        std::sync::mpsc::Sender<()>,
        std::sync::Arc<Mutex<Vec<bool>>>,
    );

    /// The worker thread as the app runs it, with a [`Gated`] driver.
    fn spawn_gated_worker(
        shared: &SharedControl,
        hold_prepare: bool,
        hold_enable: bool,
    ) -> GatedWorker {
        let (entered_tx, entered) = std::sync::mpsc::sync_channel(4);
        let (go, go_rx) = std::sync::mpsc::channel();
        let toggles = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (worker, record) = (shared.clone(), toggles.clone());
        let handle = std::thread::spawn(move || {
            let driver = Gated {
                hold_prepare,
                hold_enable,
                entered: entered_tx,
                go: go_rx,
                toggles: record,
                start: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
            };
            run(&worker, &driver, ATTACH_TIMEOUT, true);
        });
        *shared.vdd_worker.thread.lock() = Some(handle);
        (entered, go, toggles)
    }

    /// Review round 2, defect 3: quitting turned the display off while the
    /// start prepared the driver's settings, and its Enable came after: a
    /// phantom monitor outlived the app.
    #[test]
    fn quitting_while_a_start_prepares_never_enables() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        let (entered, go, toggles) = spawn_gated_worker(&shared, true, false);
        send(&shared, Command::Attach);
        assert_eq!(entered.recv_timeout(Duration::from_secs(5)), Ok("prepare"));
        let quitter = shared.clone();
        let quit = std::thread::spawn(move || shut_down(&quitter));
        while !quitting(&shared) {
            std::thread::sleep(Duration::from_millis(1));
        }
        go.send(()).unwrap();
        quit.join().unwrap();
        assert!(shared.vdd_worker.thread.lock().is_none(), "joined");
        assert!(toggles.lock().is_empty(), "{:?}", toggles.lock());
        // Nothing asked for after the quit reaches the driver.
        send(&shared, Command::Attach);
        assert!(shared.vdd_worker.queue.lock().commands.is_empty());
    }

    /// The other half of defect 3: an Enable already running when the app
    /// quits finishes before quitting goes on to its Disable.
    #[test]
    fn quitting_waits_for_an_enable_in_flight() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        let (entered, go, toggles) = spawn_gated_worker(&shared, false, true);
        send(&shared, Command::Attach);
        assert_eq!(entered.recv_timeout(Duration::from_secs(5)), Ok("enable"));
        // Without joining it: an Enable can outlast the join's bound.
        let worker = shared.vdd_worker.thread.lock().take().unwrap();
        let (done_tx, done) = std::sync::mpsc::channel();
        let quitter = shared.clone();
        let quit = std::thread::spawn(move || {
            shut_down(&quitter);
            let _ = done_tx.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_millis(300)).is_err(),
            "quitting went on while an Enable ran"
        );
        go.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        quit.join().unwrap();
        worker.join().unwrap();
        // The worker leaves the Disable to the quit, which runs it next.
        assert_eq!(*toggles.lock(), vec![true]);
    }

    /// Review round 2, defect 4: the grace period ended and a capture
    /// generation asked for the display to be turned off, but the worker
    /// acted before the generation published the status. It read `Active`,
    /// took the display as wanted, and nothing asked again. Only a start
    /// claimed since keeps it now.
    #[test]
    fn a_release_turns_the_display_off_whatever_status_it_reads() {
        let shared = second_screen_selected();
        note_attached(&shared, "virtual".into());
        shared.second_screen.lock().release_requested = true;
        let on_sleep = |_| {};
        let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
        release(&shared, &driver);
        assert_eq!(*driver.toggles.borrow(), vec![(Duration::ZERO, false)]);
        assert_eq!(*shared.vdd_output.lock(), None);
    }

    /// The grace period's end, through a capture generation and the worker:
    /// one Disable.
    #[test]
    fn the_grace_period_ends_with_one_disable() {
        let shared = second_screen_selected();
        connect_viewer(&shared);
        note_attached(&shared, "virtual".into());
        let start = Instant::now();
        assert_eq!(next_step(&shared, start), Step::Switch);
        disconnect(&shared);
        assert_eq!(next_step(&shared, start), Step::Nothing);
        assert_eq!(next_step(&shared, start + RELEASE_GRACE), Step::Release);
        let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        assert_eq!(
            reconcile_virtual_display(&CaptureTarget::VirtualExtended, &shared, generation),
            Some(CaptureTarget::PrimaryAuto)
        );
        assert_eq!(*shared.vdd_status.lock(), VddStatus::WaitingForClient);
        let on_sleep = |_| {};
        let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
        run(&shared, &driver, ATTACH_TIMEOUT, false);
        assert_eq!(*driver.toggles.borrow(), vec![(Duration::ZERO, false)]);
        assert_eq!(*shared.vdd_output.lock(), None);
        run(&shared, &driver, ATTACH_TIMEOUT, false);
        assert_eq!(driver.toggles.borrow().len(), 1);
    }

    /// The iPad came back after the release was asked for, before the
    /// worker ran it: no change, and the start reuses the display.
    #[test]
    fn a_viewer_back_before_the_release_runs_keeps_the_display() {
        let shared = second_screen_selected();
        note_attached(&shared, "virtual".into());
        shared.second_screen.lock().release_requested = true;
        let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        assert_eq!(
            reconcile_virtual_display(&CaptureTarget::VirtualExtended, &shared, generation),
            Some(CaptureTarget::PrimaryAuto)
        );
        connect_viewer(&shared);
        assert_eq!(next_step(&shared, Instant::now()), Step::Attach);
        send(&shared, Command::Attach);
        let on_sleep = |_| {};
        let driver = FakeDriver::new(secs(3.0), Some(Duration::ZERO), &on_sleep);
        driver.on_since.set(Some(Duration::ZERO));
        run(&shared, &driver, ATTACH_TIMEOUT, false);
        assert!(driver.toggles.borrow().is_empty());
        assert_eq!(*shared.vdd_status.lock(), VddStatus::Active);
        assert_eq!(next_step(&shared, driver.now()), Step::Switch);
    }
}
