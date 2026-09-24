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
#[cfg(windows)]
use std::time::{Duration, Instant};
#[cfg(windows)]
use tracing::{info, warn};

#[cfg(windows)]
static TASK_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Scheduled task names — must match the ones the installer registers.
pub const TASK_ENABLE: &str = "EternalMonitor VDD Enable";
pub const TASK_DISABLE: &str = "EternalMonitor VDD Disable";

/// Trigger a pre-registered scheduled task by name. Returns true on success. Missing tasks
/// (e.g. a build installed without the VDD feature) just log a warning and return false —
/// the caller falls back to the primary display.
#[cfg(windows)]
fn run_task(task: &str, mut progress: impl FnMut()) -> bool {
    use std::os::windows::process::CommandExt;
    let _guard = loop {
        if let Some(guard) = TASK_LOCK.try_lock_for(Duration::from_millis(50)) {
            break guard;
        }
        progress();
    };
    let script = format!("& {{ {} }} -Task '{}'", include_str!("vdd_task.ps1"), task);
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
            return false;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(20);
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
                return false;
            }
        }
    }

    match child.wait_with_output() {
        Ok(out) if out.status.success() => {
            info!(task, "Completed VDD scheduled task");
            true
        }
        Ok(out) => {
            warn!(
                task,
                code = ?out.status.code(),
                stderr = %String::from_utf8_lossy(&out.stderr),
                stdout = %String::from_utf8_lossy(&out.stdout),
                "VDD scheduled task failed"
            );
            false
        }
        Err(error) => {
            warn!(task, error = %error, "Failed to read the VDD task result");
            false
        }
    }
}

/// Complete one installer-owned driver action while capture reports startup progress.
pub(crate) fn set_enabled(enabled: bool, progress: impl FnMut()) -> bool {
    #[cfg(windows)]
    {
        run_task(if enabled { TASK_ENABLE } else { TASK_DISABLE }, progress)
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, progress);
        false
    }
}

/// Disable the virtual display device (best effort — failures are non-fatal).
pub fn disable() {
    set_enabled(false, || {});
}
