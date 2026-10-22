//! Lifecycle control for the bundled third-party virtual display driver (VDD).
//!
//! The driver is left DISABLED by default so no phantom monitor exists when EternalMonitor
//! isn't using it. We enable it only while actively capturing the virtual extended display.
//!
//! Enabling/disabling a driver device requires admin, so the installer registers two
//! "run with highest privileges" scheduled tasks (one per direction). Triggering an
//! already-elevated, pre-authorized task does not raise a UAC prompt, so the non-elevated
//! host can flip the virtual display on and off seamlessly.

#[cfg(windows)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
#[cfg(windows)]
use tracing::{info, warn};

static TASKS: DriverTasks = DriverTasks::new();

/// Whether the installer-owned driver tasks can be used. Stable values the GUI
/// can explain; the extended display falls back to mirroring in both failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VddTaskHealth {
    Ok,
    /// Windows reports the scheduled task does not exist (0x80070002): the
    /// app was installed without the display driver, or the tasks were removed.
    TasksMissing,
    /// A driver task is still running from an earlier request. Disabling or
    /// enabling a display device can hang until Windows restarts.
    NotResponding,
}

/// After a task times out, requests fall back to mirroring at once for this
/// long instead of each one waiting out the same hung task again.
#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
const STUCK_COOLDOWN: Duration = Duration::from_secs(60);

/// Why a driver task is requested. The gate throttles the pipeline's own
/// attempts; cleanup (at startup, at quit and after a panic) always tries
/// when the display may be on, so a refused attempt never strands it.
#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Attempt,
    Cleanup,
}

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
#[derive(Debug, PartialEq, Eq)]
enum Gate {
    /// The device is already in the requested state; nothing to run.
    AlreadyDone,
    Refuse,
    Run,
}

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskOutcome {
    Done,
    TimedOut,
    Missing,
    /// With the device state the Enable task found, when it said.
    Failed(Option<crate::vdd_device::DeviceState>),
}

/// What a failed Enable can tell about the device: the state the task
/// found, or why there is none.
pub(crate) type EnableReport = Result<crate::vdd_device::DeviceState, &'static str>;

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
impl TaskOutcome {
    /// For an Enable task that ran and did not succeed.
    fn report(self) -> EnableReport {
        match self {
            Self::Failed(Some(state)) => Ok(state),
            Self::Failed(None) => Err("display task failed"),
            Self::TimedOut => Err("display task timed out"),
            Self::Missing => Err("display tasks missing"),
            Self::Done => Err("display task finished"),
        }
    }

    fn from_stderr(stderr: &str) -> Self {
        if stderr.contains("timed out") {
            Self::TimedOut
        } else if stderr.contains("0x80070002") {
            Self::Missing
        } else {
            Self::Failed(crate::vdd_device::from_task_stderr(stderr))
        }
    }
}

/// Remembers what the driver tasks last did. Every idle pipeline start used to
/// run the Disable task again (two to three seconds of PowerShell each time),
/// and a hung task made every later request wait out its timeout.
#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
struct TaskGate {
    known_enabled: Option<bool>,
    health: VddTaskHealth,
    stuck_at: Option<Instant>,
    /// An Enable task ran since the last completed Disable, so the display
    /// may be on, even when that Enable timed out and is still running.
    enable_ran: bool,
}

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
impl TaskGate {
    const fn new() -> Self {
        Self {
            known_enabled: None,
            health: VddTaskHealth::Ok,
            stuck_at: None,
            enable_ran: false,
        }
    }

    fn before(&self, enabled: bool, purpose: Purpose, now: Instant) -> Gate {
        if self.known_enabled == Some(enabled) {
            return Gate::AlreadyDone;
        }
        if purpose == Purpose::Cleanup {
            // Missing tasks only matter if no Enable could have run.
            return if self.health == VddTaskHealth::TasksMissing && !self.enable_ran {
                Gate::Refuse
            } else {
                Gate::Run
            };
        }
        match self.health {
            VddTaskHealth::TasksMissing => Gate::Refuse,
            VddTaskHealth::NotResponding
                if self
                    .stuck_at
                    .is_some_and(|at| now.duration_since(at) < STUCK_COOLDOWN) =>
            {
                Gate::Refuse
            }
            _ => Gate::Run,
        }
    }

    /// Forget that the tasks were missing, so the next attempt finds out
    /// whether Setup put them back. Still missing, that attempt fails fast.
    fn recheck(&mut self) {
        if self.health == VddTaskHealth::TasksMissing {
            self.health = VddTaskHealth::Ok;
        }
    }

    fn after(&mut self, enabled: bool, outcome: TaskOutcome, now: Instant) {
        self.known_enabled = None;
        if enabled && outcome != TaskOutcome::Missing {
            self.enable_ran = true;
        }
        if !enabled && outcome == TaskOutcome::Done {
            self.enable_ran = false;
        }
        match outcome {
            TaskOutcome::Done => {
                self.known_enabled = Some(enabled);
                self.health = VddTaskHealth::Ok;
                self.stuck_at = None;
            }
            TaskOutcome::TimedOut => {
                self.health = VddTaskHealth::NotResponding;
                self.stuck_at = Some(now);
            }
            TaskOutcome::Missing => self.health = VddTaskHealth::TasksMissing,
            TaskOutcome::Failed(_) => {}
        }
    }
}

/// Current health of the driver tasks, for the GUI.
pub fn task_health() -> VddTaskHealth {
    TASKS.health()
}

/// The user started or restarted the stream: missing driver tasks are looked
/// for again at the next attempt, since Setup may have repaired them.
pub fn recheck_tasks() {
    TASKS.gate.lock().recheck();
}

/// Runs one driver request at a time, from the gate check through the state
/// update. A Disable at shutdown must wait for an Enable that is still
/// running; checking the gate first let it read the old state, return
/// "already off", and leave the monitor on when the Enable finished.
struct DriverTasks {
    serial: parking_lot::Mutex<()>,
    gate: parking_lot::Mutex<TaskGate>,
    /// Disables running or waiting for their turn. A pipeline's own Disable
    /// does not queue behind one: the display ends up off either way, and
    /// a Mirror stream need not wait for it to show its first frame.
    disabling: std::sync::atomic::AtomicUsize,
}

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
impl DriverTasks {
    const fn new() -> Self {
        Self {
            serial: parking_lot::Mutex::new(()),
            gate: parking_lot::Mutex::new(TaskGate::new()),
            disabling: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn health(&self) -> VddTaskHealth {
        self.gate.lock().health
    }

    #[cfg(test)]
    fn set_enabled(
        &self,
        enabled: bool,
        purpose: Purpose,
        progress: impl FnMut(),
        run: impl FnOnce(bool, &mut dyn FnMut()) -> TaskOutcome,
    ) -> bool {
        self.set_enabled_on_turn(enabled, purpose, progress, || {}, run)
    }

    /// As `set_enabled`, calling `on_turn` once this request holds the turn:
    /// every request made after that queues behind it.
    fn set_enabled_on_turn(
        &self,
        enabled: bool,
        purpose: Purpose,
        mut progress: impl FnMut(),
        on_turn: impl FnOnce(),
        run: impl FnOnce(bool, &mut dyn FnMut()) -> TaskOutcome,
    ) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        struct Pending<'a>(Option<&'a std::sync::atomic::AtomicUsize>);
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                if let Some(count) = self.0 {
                    count.fetch_sub(1, SeqCst);
                }
            }
        }
        if !enabled {
            if purpose == Purpose::Attempt && self.disabling.load(SeqCst) > 0 {
                tracing::debug!("A Disable is already pending; not waiting for it");
                return true;
            }
            self.disabling.fetch_add(1, SeqCst);
        }
        let _pending = Pending((!enabled).then_some(&self.disabling));
        let _serial = loop {
            if let Some(guard) = self.serial.try_lock_for(Duration::from_millis(50)) {
                break guard;
            }
            progress();
        };
        on_turn();
        // The gate guard ends with this statement. Logging below reads the
        // health through the same mutex, which deadlocked when the guard
        // lived through the whole match.
        let decision = self.gate.lock().before(enabled, purpose, Instant::now());
        match decision {
            Gate::AlreadyDone => return true,
            Gate::Refuse => {
                tracing::debug!(enabled, health = ?self.health(), "Skipping VDD task");
                return false;
            }
            Gate::Run => {}
        }
        let outcome = run(enabled, &mut progress);
        self.gate.lock().after(enabled, outcome, Instant::now());
        outcome == TaskOutcome::Done
    }
}

/// Scheduled task names — must match the ones the installer registers.
pub const TASK_ENABLE: &str = "EternalMonitor VDD Enable";
pub const TASK_DISABLE: &str = "EternalMonitor VDD Disable";

/// Trigger a pre-registered scheduled task by name. Returns true on success. Missing tasks
/// (e.g. a build installed without the VDD feature) just log a warning and return false —
/// the caller falls back to the primary display.
#[cfg(windows)]
fn run_task(task: &str, progress: &mut dyn FnMut()) -> TaskOutcome {
    use std::os::windows::process::CommandExt;
    #[cfg(debug_assertions)]
    if let Some(outcome) = simulated_enable_failure(task) {
        warn!(task, ?outcome, "Simulating a failed VDD task");
        return outcome;
    }
    let started = Instant::now();
    // An Enable that timed out here can still be running in Task Scheduler;
    // each direction waits for the other first, so the last request wins.
    let other = if task == TASK_ENABLE {
        TASK_DISABLE
    } else {
        TASK_ENABLE
    };
    let script = format!(
        "& {{ {} }} -Task '{}' -After '{}'",
        include_str!("vdd_task.ps1"),
        task,
        other
    );
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(0x0800_0000)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            warn!(task, %error, "Failed to invoke the VDD task");
            return TaskOutcome::Failed(None);
        }
    };
    // vdd_task.ps1 waits up to 10 s for an earlier instance and 15 s for its own.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                progress();
                std::thread::sleep(Duration::from_millis(50));
            }
            result => {
                warn!(task, ?result, "VDD task runner failed or timed out");
                let _ = child.kill();
                let _ = child.wait();
                return TaskOutcome::TimedOut;
            }
        }
    }

    match child.wait_with_output() {
        Ok(out) if out.status.success() => {
            info!(
                task,
                took_ms = started.elapsed().as_millis() as u64,
                "Completed VDD scheduled task"
            );
            TaskOutcome::Done
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(
                task,
                code = ?out.status.code(),
                took_ms = started.elapsed().as_millis() as u64,
                stderr = %stderr.trim_end(),
                stdout = %String::from_utf8_lossy(&out.stdout).trim_end(),
                "VDD scheduled task failed"
            );
            TaskOutcome::from_stderr(&stderr)
        }
        Err(error) => {
            warn!(task, error = %error, "Failed to read the VDD task result");
            TaskOutcome::Failed(None)
        }
    }
}

/// Debug builds only, to check the failure paths on a PC whose tasks work:
/// `ETERNAL_SIMULATE_ENABLE_FAILURE=missing|timeout|failed` makes the Enable
/// task report that without running.
#[cfg(all(windows, debug_assertions))]
fn simulated_enable_failure(task: &str) -> Option<TaskOutcome> {
    if task != TASK_ENABLE {
        return None;
    }
    match std::env::var("ETERNAL_SIMULATE_ENABLE_FAILURE")
        .ok()?
        .as_str()
    {
        "missing" => Some(TaskOutcome::Missing),
        "timeout" => Some(TaskOutcome::TimedOut),
        "failed" => Some(TaskOutcome::Failed(None)),
        _ => None,
    }
}

/// Complete one installer-owned driver action while capture reports startup progress.
#[cfg_attr(not(windows), allow(dead_code))] // Only Windows capture asks.
pub(crate) fn set_enabled(enabled: bool, progress: impl FnMut()) -> bool {
    request(enabled, Purpose::Attempt, progress, &mut None)
}

/// Why an Enable the gate did not run failed.
#[cfg_attr(not(windows), allow(dead_code))] // Only Windows capture asks.
fn refused(health: VddTaskHealth) -> EnableReport {
    Err(match health {
        VddTaskHealth::TasksMissing => "display tasks missing",
        VddTaskHealth::NotResponding => "display task not responding",
        VddTaskHealth::Ok => "display task not run",
    })
}

/// Turn the virtual display on. On failure, what the Enable task found
/// wrong with the device, or why it could not say.
#[cfg_attr(not(windows), allow(dead_code))] // Only Windows capture asks.
pub(crate) fn enable(progress: impl FnMut()) -> Result<(), EnableReport> {
    let mut ran = None;
    if request(true, Purpose::Attempt, progress, &mut ran) {
        Ok(())
    } else {
        Err(ran.map_or_else(|| refused(task_health()), TaskOutcome::report))
    }
}

fn request(
    enabled: bool,
    purpose: Purpose,
    progress: impl FnMut(),
    ran: &mut Option<TaskOutcome>,
) -> bool {
    request_on_turn(enabled, purpose, progress, || {}, ran)
}

fn request_on_turn(
    enabled: bool,
    purpose: Purpose,
    progress: impl FnMut(),
    on_turn: impl FnOnce(),
    ran: &mut Option<TaskOutcome>,
) -> bool {
    #[cfg(windows)]
    {
        TASKS.set_enabled_on_turn(enabled, purpose, progress, on_turn, |enabled, progress| {
            let mut outcome = TaskOutcome::Failed(None);
            crate::startup::display::change(enabled, || {
                outcome = run_task(if enabled { TASK_ENABLE } else { TASK_DISABLE }, progress);
                outcome == TaskOutcome::Done
            });
            *ran = Some(outcome);
            outcome
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, purpose, progress, ran);
        on_turn();
        false
    }
}

/// Disable the virtual display device (best effort — failures are non-fatal).
/// For startup, quit and panics: a hung or refused earlier task does not stop
/// it, and it waits for an Enable still running, within the task's bounds.
/// True when the display is known to be off.
pub fn disable() -> bool {
    request(false, Purpose::Cleanup, || {}, &mut None)
}

/// `disable` at startup, without holding up the first frame: the Disable
/// runs on its own thread, and this returns once it holds the turn, so
/// every later request (an Enable, the quit's Disable) still runs after it.
/// A hung task no longer keeps a Mirror stream black for up to 30 s.
pub fn disable_in_background() {
    let (held, turn) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("vdd-startup-off".into())
        .spawn(move || {
            let on_turn = move || {
                let _ = held.send(());
            };
            request_on_turn(false, Purpose::Cleanup, || {}, on_turn, &mut None)
        });
    match spawned {
        Ok(_) => {
            let _ = turn.recv();
        }
        Err(_) => {
            disable();
        }
    }
}

/// Disable the virtual display after `host` ended abnormally, if it owned the
/// display and no other host of this install runs. For the failure reporter.
#[cfg(windows)]
pub(crate) fn disable_for_ended(
    host: crate::startup::ProcessId,
    reporters: &[crate::startup::ProcessId],
) -> Result<bool, String> {
    crate::startup::display::disable_for_ended(
        host,
        || crate::startup::display::other_host(host, reporters),
        || run_task(TASK_DISABLE, &mut || {}) == TaskOutcome::Done,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_already_off_is_not_disabled_again() {
        let mut gate = TaskGate::new();
        let now = Instant::now();
        // Unknown at launch: the first disable runs.
        assert_eq!(gate.before(false, Purpose::Attempt, now), Gate::Run);
        gate.after(false, TaskOutcome::Done, now);
        assert_eq!(gate.before(false, Purpose::Attempt, now), Gate::AlreadyDone);
        assert_eq!(gate.before(true, Purpose::Attempt, now), Gate::Run);
        gate.after(true, TaskOutcome::Done, now);
        assert_eq!(gate.before(true, Purpose::Attempt, now), Gate::AlreadyDone);
        assert_eq!(gate.before(false, Purpose::Attempt, now), Gate::Run);
    }

    #[test]
    fn a_hung_task_is_not_waited_out_again_until_the_cooldown_ends() {
        let mut gate = TaskGate::new();
        let now = Instant::now();
        gate.after(true, TaskOutcome::TimedOut, now);
        assert_eq!(gate.health, VddTaskHealth::NotResponding);
        assert_eq!(
            gate.before(true, Purpose::Attempt, now + Duration::from_secs(5)),
            Gate::Refuse
        );
        assert_eq!(
            gate.before(false, Purpose::Attempt, now + Duration::from_secs(5)),
            Gate::Refuse
        );
        assert_eq!(
            gate.before(true, Purpose::Attempt, now + STUCK_COOLDOWN),
            Gate::Run
        );
        gate.after(true, TaskOutcome::Done, now + STUCK_COOLDOWN);
        assert_eq!(gate.health, VddTaskHealth::Ok);
    }

    #[test]
    fn missing_tasks_stop_further_attempts_and_failures_forget_the_state() {
        let mut gate = TaskGate::new();
        let now = Instant::now();
        gate.after(false, TaskOutcome::Done, now);
        gate.after(true, TaskOutcome::Failed(None), now);
        // A failed task leaves the device state unknown, so disable runs again.
        assert_eq!(gate.before(false, Purpose::Attempt, now), Gate::Run);
        gate.after(false, TaskOutcome::Missing, now);
        assert_eq!(
            gate.before(true, Purpose::Attempt, now + STUCK_COOLDOWN * 10),
            Gate::Refuse
        );
    }

    #[test]
    fn quitting_inside_the_cooldown_after_an_enable_timed_out_still_disables() {
        let tasks = DriverTasks::new();
        // The Enable timed out here; Task Scheduler may still be running it.
        assert!(!tasks.set_enabled(
            true,
            Purpose::Attempt,
            || {},
            |_, _| { TaskOutcome::TimedOut }
        ));
        // The pipeline's own requests stay throttled during the cooldown.
        assert!(!tasks.set_enabled(
            false,
            Purpose::Attempt,
            || {},
            |_, _| { panic!("an attempt inside the cooldown must not run") }
        ));
        // The quit's Disable still runs.
        let mut ran = Vec::new();
        assert!(tasks.set_enabled(
            false,
            Purpose::Cleanup,
            || {},
            |enabled, _| {
                ran.push(enabled);
                TaskOutcome::Done
            }
        ));
        assert_eq!(ran, [false]);
        assert_eq!(tasks.gate.lock().known_enabled, Some(false));
    }

    #[test]
    fn cleanup_skips_only_a_display_this_host_cannot_have_turned_on() {
        let now = Instant::now();
        // Off for certain: nothing to do.
        let mut gate = TaskGate::new();
        gate.after(false, TaskOutcome::Done, now);
        assert_eq!(gate.before(false, Purpose::Cleanup, now), Gate::AlreadyDone);
        // The tasks were missing from the start: no Enable ever ran.
        let mut gate = TaskGate::new();
        gate.after(false, TaskOutcome::Missing, now);
        assert_eq!(gate.before(false, Purpose::Cleanup, now), Gate::Refuse);
        // An Enable ran before the tasks went missing: try anyway.
        let mut gate = TaskGate::new();
        gate.after(true, TaskOutcome::Done, now);
        gate.after(false, TaskOutcome::Missing, now);
        assert_eq!(gate.before(false, Purpose::Cleanup, now), Gate::Run);
        // A failed Disable leaves the state unknown, so quit tries again.
        let mut gate = TaskGate::new();
        gate.after(true, TaskOutcome::Done, now);
        gate.after(false, TaskOutcome::Failed(None), now);
        assert_eq!(gate.before(false, Purpose::Cleanup, now), Gate::Run);
    }

    #[test]
    fn try_again_after_setup_repairs_missing_tasks_runs_the_task() {
        let tasks = DriverTasks::new();
        let missing = |_: bool, _: &mut dyn FnMut()| TaskOutcome::Missing;
        assert!(!tasks.set_enabled(true, Purpose::Attempt, || {}, missing));
        assert_eq!(tasks.health(), VddTaskHealth::TasksMissing);
        // Restarts on their own still do not run PowerShell for nothing.
        assert!(!tasks.set_enabled(
            true,
            Purpose::Attempt,
            || {},
            |_, _| { panic!("missing tasks are not retried without the user") }
        ));
        // Try again before Setup ran: the task is looked for, and still missing.
        tasks.gate.lock().recheck();
        assert!(!tasks.set_enabled(true, Purpose::Attempt, || {}, missing));
        assert_eq!(tasks.health(), VddTaskHealth::TasksMissing);
        // Setup repaired the tasks; Try again turns the second screen on.
        tasks.gate.lock().recheck();
        let mut ran = 0;
        assert!(tasks.set_enabled(
            true,
            Purpose::Attempt,
            || {},
            |_, _| {
                ran += 1;
                TaskOutcome::Done
            }
        ));
        assert_eq!(ran, 1);
        assert_eq!(tasks.health(), VddTaskHealth::Ok);
    }

    #[test]
    fn try_again_and_start_in_the_window_look_for_repaired_tasks() {
        // The only test that touches the process-wide gate.
        let (supervisor_tx, _commands) = std::sync::mpsc::channel();
        let control = crate::control::GuiControl {
            shared: crate::control::SharedControl::new(9876, 15_000_000),
            supervisor_tx,
        };
        for request in [
            crate::control::GuiControl::request_restart,
            crate::control::GuiControl::request_start,
        ] {
            TASKS
                .gate
                .lock()
                .after(true, TaskOutcome::Missing, Instant::now());
            assert_eq!(task_health(), VddTaskHealth::TasksMissing);
            request(&control);
            assert_eq!(task_health(), VddTaskHealth::Ok);
        }
    }

    #[test]
    fn a_refused_task_with_debug_logging_returns_instead_of_deadlocking() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let tasks = DriverTasks::new();
            tasks
                .gate
                .lock()
                .after(true, TaskOutcome::Missing, Instant::now());
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(std::io::sink)
                .finish();
            let ran = tracing::subscriber::with_default(subscriber, || {
                tasks.set_enabled(
                    true,
                    Purpose::Attempt,
                    || {},
                    |_, _| panic!("a refused task must not run"),
                )
            });
            let _ = done_tx.send(ran);
        });
        let ran = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("set_enabled deadlocked on the gate mutex");
        assert!(!ran);
    }

    #[test]
    fn a_disable_waits_for_a_running_enable_instead_of_reading_stale_state() {
        use std::sync::mpsc::channel;
        use std::sync::{Arc, Mutex};
        let tasks = Arc::new(DriverTasks::new());
        let runs = Arc::new(Mutex::new(Vec::new()));
        let record = |runs: &Arc<Mutex<Vec<bool>>>, enabled| runs.lock().unwrap().push(enabled);
        // Startup leaves the device known to be off.
        assert!(tasks.set_enabled(
            false,
            Purpose::Attempt,
            || {},
            |e, _| {
                record(&runs, e);
                TaskOutcome::Done
            }
        ));

        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        let enable = {
            let (tasks, runs) = (Arc::clone(&tasks), Arc::clone(&runs));
            std::thread::spawn(move || {
                tasks.set_enabled(
                    true,
                    Purpose::Attempt,
                    || {},
                    |e, _| {
                        record(&runs, e);
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        TaskOutcome::Done
                    },
                )
            })
        };
        started_rx.recv().unwrap();

        // Shutdown asks for Disable while Enable is still running.
        let (waiting_tx, waiting_rx) = channel();
        let disable = {
            let (tasks, runs) = (Arc::clone(&tasks), Arc::clone(&runs));
            std::thread::spawn(move || {
                tasks.set_enabled(
                    false,
                    Purpose::Cleanup,
                    || {
                        let _ = waiting_tx.send(());
                    },
                    |e, _| {
                        record(&runs, e);
                        TaskOutcome::Done
                    },
                )
            })
        };
        assert!(
            waiting_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "Disable returned without waiting for the running Enable"
        );
        release_tx.send(()).unwrap();
        assert!(enable.join().unwrap());
        assert!(disable.join().unwrap());
        assert_eq!(*runs.lock().unwrap(), vec![false, true, false]);
        assert_eq!(tasks.gate.lock().known_enabled, Some(false));
    }

    #[test]
    fn enable_failures_without_a_device_state_say_why() {
        use crate::vdd_device::DeviceState;
        assert_eq!(
            TaskOutcome::Failed(Some(DeviceState::Problem(43))).report(),
            Ok(DeviceState::Problem(43))
        );
        assert_eq!(
            TaskOutcome::Failed(None).report(),
            Err("display task failed")
        );
        assert_eq!(
            TaskOutcome::TimedOut.report(),
            Err("display task timed out")
        );
        assert_eq!(TaskOutcome::Missing.report(), Err("display tasks missing"));
        // The gate refused to run the task at all.
        assert_eq!(
            refused(VddTaskHealth::TasksMissing),
            Err("display tasks missing")
        );
        assert_eq!(
            refused(VddTaskHealth::NotResponding),
            Err("display task not responding")
        );
        assert_eq!(refused(VddTaskHealth::Ok), Err("display task not run"));
    }

    /// Runs `set_enabled` on a thread; `None` if it has not returned in 2 s.
    fn returns_within(
        tasks: &std::sync::Arc<DriverTasks>,
        enabled: bool,
        purpose: Purpose,
        runs: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
    ) -> Option<bool> {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let tasks = std::sync::Arc::clone(tasks);
        std::thread::spawn(move || {
            let ran = tasks.set_enabled(
                enabled,
                purpose,
                || {},
                |e, _| {
                    runs.lock().unwrap().push(e);
                    TaskOutcome::Done
                },
            );
            let _ = done_tx.send(ran);
        });
        done_rx.recv_timeout(Duration::from_secs(2)).ok()
    }

    #[test]
    fn a_mirror_start_does_not_wait_behind_the_startup_disable() {
        use std::sync::mpsc::channel;
        use std::sync::{Arc, Mutex};
        let tasks = Arc::new(DriverTasks::new());
        let runs = Arc::new(Mutex::new(Vec::new()));
        // The startup Disable holds the turn: its task is hung.
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        let startup = {
            let tasks = Arc::clone(&tasks);
            std::thread::spawn(move || {
                tasks.set_enabled(
                    false,
                    Purpose::Cleanup,
                    || {},
                    |_, _| {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        TaskOutcome::Done
                    },
                )
            })
        };
        started_rx.recv().unwrap();
        // The Mirror pipeline's Disable returns at once and runs nothing.
        assert_eq!(
            returns_within(&tasks, false, Purpose::Attempt, Arc::clone(&runs)),
            Some(true),
            "the pipeline waited behind the startup Disable"
        );
        // An Enable still waits its turn, then runs after the Disable.
        let (enable_tx, enable_rx) = channel();
        {
            let (tasks, runs) = (Arc::clone(&tasks), Arc::clone(&runs));
            std::thread::spawn(move || {
                let _ = enable_tx.send(returns_within(&tasks, true, Purpose::Attempt, runs));
            });
        }
        assert!(
            enable_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the Enable did not wait"
        );
        release_tx.send(()).unwrap();
        assert!(startup.join().unwrap());
        assert_eq!(enable_rx.recv().unwrap(), Some(true));
        assert_eq!(*runs.lock().unwrap(), vec![true]);
        // With nothing pending, a pipeline Disable runs again.
        assert_eq!(
            returns_within(&tasks, false, Purpose::Attempt, Arc::clone(&runs)),
            Some(true)
        );
        assert_eq!(*runs.lock().unwrap(), vec![true, false]);
    }

    #[test]
    fn task_errors_are_classified_from_the_script_output() {
        assert_eq!(
            TaskOutcome::from_stderr("Previous VDD task timed out: EternalMonitor VDD Disable"),
            TaskOutcome::TimedOut
        );
        assert_eq!(
            TaskOutcome::from_stderr(
                "Das System kann die angegebene Datei nicht finden. (Ausnahme von HRESULT: 0x80070002)"
            ),
            TaskOutcome::Missing
        );
        assert_eq!(
            TaskOutcome::from_stderr("VDD task failed: x, result=1"),
            TaskOutcome::Failed(None)
        );
        // The Enable task says what it found wrong with the device.
        assert_eq!(
            TaskOutcome::from_stderr("VDD task failed: EternalMonitor VDD Enable, result=5"),
            TaskOutcome::Failed(Some(crate::vdd_device::DeviceState::NoMonitor))
        );
    }
}
