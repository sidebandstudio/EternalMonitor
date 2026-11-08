//! Portable policy for capture interruptions Windows causes on purpose: the
//! lock screen, UAC prompts, display mode changes, exclusive full-screen apps.
//! None of these is a pipeline fault, so they must neither spin (the 0.3.1
//! ACCESS_LOST loop re-created the duplication every millisecond for hours)
//! nor count toward the supervisor's restart budget.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::control::SharedControl;

/// Why the desktop cannot be captured right now. The values are stable so the
/// GUI can tell the user what to do; each one is a Windows state, not a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBlock {
    /// A secure screen is up (lock screen, UAC prompt, Ctrl+Alt+Del). Capture
    /// resumes by itself when the user returns to the desktop.
    SecureDesktop,
    /// The display mode changed or a full-screen app took exclusive control
    /// of the display, and duplication has not come back yet.
    DisplayChanging,
    /// No display is on: a laptop's lid is closed or its screen is off.
    /// Capture resumes by itself when a display comes on.
    NoDisplay,
    /// Windows refuses to duplicate this display from the GPU EternalMonitor
    /// runs on. Seen on hybrid laptops when the app runs on the discrete GPU
    /// while the screen is wired to the integrated one.
    UnsupportedOnThisGpu,
    /// Too many apps are capturing the screen at once (Windows allows a few).
    TooManyCaptureApps,
    /// The Windows session is disconnected or remote.
    SessionDisconnected,
    /// A graphics driver call did not return in time (see `crate::stall`).
    /// Capture waits for it and resumes by itself when it does.
    DriverNotResponding,
}

impl CaptureBlock {
    /// How an iPad that asks after the storm brake is told this block.
    pub fn failed_reason(self) -> eternal_wire::v2::control::FailedReason {
        use eternal_wire::v2::control::FailedReason;
        match self {
            Self::SecureDesktop => FailedReason::SecureDesktop,
            Self::DisplayChanging => FailedReason::DisplayChanging,
            Self::NoDisplay => FailedReason::NoDisplay,
            Self::UnsupportedOnThisGpu => FailedReason::UnsupportedOnThisGpu,
            Self::TooManyCaptureApps => FailedReason::TooManyCaptureApps,
            Self::SessionDisconnected => FailedReason::SessionDisconnected,
            Self::DriverNotResponding => FailedReason::DriverNotResponding,
        }
    }

    /// Maps a DXGI/Win32 HRESULT from DuplicateOutput or AcquireNextFrame.
    /// `None` means a real failure that the supervisor should handle.
    pub fn from_hresult(code: i32) -> Option<Self> {
        match code as u32 {
            0x8007_0005 => Some(Self::SecureDesktop), // E_ACCESSDENIED
            0x887A_0026 => Some(Self::DisplayChanging), // DXGI_ERROR_ACCESS_LOST
            // DXGI_ERROR_NOT_FOUND: an adapter or output is missing, while
            // the layout changes or a laptop's lid is closed. It ended
            // capture like a crash, so the restart brake stopped it until
            // the user clicked Start. [`Self::opening`] tells a closed lid.
            0x887A_0002 => Some(Self::DisplayChanging),
            0x887A_0004 => Some(Self::UnsupportedOnThisGpu), // DXGI_ERROR_UNSUPPORTED
            0x887A_0022 => Some(Self::TooManyCaptureApps),   // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
            0x887A_0028 => Some(Self::SessionDisconnected),  // DXGI_ERROR_SESSION_DISCONNECTED
            _ => None,
        }
    }

    /// [`Self::from_hresult`] for a failure to open the output to capture.
    /// DXGI_ERROR_NOT_FOUND means no display is on only when enumeration
    /// found an adapter but no output at all; anywhere else the layout is
    /// changing.
    pub fn opening(step: OpenStep, code: i32) -> Option<Self> {
        match (step, code as u32) {
            (OpenStep::OnlyOutput, 0x887A_0002) => Some(Self::NoDisplay),
            _ => Self::from_hresult(code),
        }
    }
}

/// The call that opens the output to capture, for [`CaptureBlock::opening`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenStep {
    /// `EnumAdapters1`: no adapter is listed, as during a driver update.
    Adapter,
    /// `EnumOutputs` for an output enumeration listed: it went away since,
    /// as when a display is unplugged or Win+P switches displays.
    ListedOutput,
    /// `EnumOutputs(0)` after enumeration listed no output at all: a
    /// laptop's lid is closed or every display is off.
    OnlyOutput,
}

/// Errors a GPU reset, a display change or a shortage of memory raises while
/// it lasts: DXGI_ERROR_DEVICE_REMOVED, _DEVICE_HUNG, _DEVICE_RESET,
/// _ACCESS_LOST and E_OUTOFMEMORY. Freeing the memory changes nothing in a
/// saved record's fingerprint, so saving it would keep a working PC on the
/// mirror.
const PASSING_ERRORS: [u32; 5] = [
    0x887A_0005,
    0x887A_0006,
    0x887A_0007,
    0x887A_0026,
    0x8007_000E,
];

/// [`passing_error`] for a DXGI/Win32 HRESULT.
pub fn passing_code(code: i32) -> bool {
    match CaptureBlock::from_hresult(code) {
        Some(block) => block != CaptureBlock::UnsupportedOnThisGpu,
        None => PASSING_ERRORS.contains(&(code as u32)),
    }
}

/// Whether a capture error comes from a state of Windows that passes (a
/// secure screen, a display or mode change, a GPU reset, another capturing
/// app, a disconnected session) rather than from the display itself. Such a
/// second screen failure is not remembered for later app runs. A display
/// Windows will not duplicate from this GPU is the PC's. A driver call that
/// did not return in time passes too, like a hung device.
pub fn passing_error(error: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    if let Some(unavailable) = error.downcast_ref::<CaptureUnavailable>() {
        return unavailable.block != CaptureBlock::UnsupportedOnThisGpu;
    }
    if error.downcast_ref::<Reopen>().is_some() {
        return true;
    }
    if error.downcast_ref::<crate::stall::Stall>().is_some() {
        return true;
    }
    #[cfg(windows)]
    if let Some(error) = error.downcast_ref::<windows::core::Error>() {
        return passing_code(error.code().0);
    }
    false
}

/// After a graphics card switch made from the Stream page notice, the first
/// capture attempt of the relaunched app says whether it helped: one log
/// line, and the record the GUI saves. The process that saved the change
/// still runs on the old card and says nothing.
pub fn note_gpu_outcome(shared: &SharedControl, outcome: crate::gpu_preference::Outcome) {
    let mut record = shared.gpu_switch.lock();
    // Read under the record's lock: `save_gpu_switch` sets it there before
    // a record saved here is shared.
    if shared.gpu_changed_here.load(Ordering::SeqCst) {
        return;
    }
    let Some(record) = record.as_mut() else {
        return;
    };
    if !crate::gpu_preference::settle(record, outcome) {
        return;
    }
    let app_gpu = crate::stats::PIPELINE_STATS.lock().gpu_name.clone();
    match outcome {
        crate::gpu_preference::Outcome::Worked => info!(
            gpu = %record.gpu,
            preference = ?record.preference,
            app_gpu,
            "Graphics card switch outcome: capture works"
        ),
        crate::gpu_preference::Outcome::StillUnsupported => warn!(
            gpu = %record.gpu,
            preference = ?record.preference,
            app_gpu,
            "Graphics card switch outcome: Windows still won't share the screen; another app probably chooses the card"
        ),
    }
}

/// A graphics card change saved by this process: `save` works on a copy of
/// the switch record (writing settings.json and Windows' value). Only when
/// it worked is the record it keeps shared, under the record's lock and
/// after marking this process as the one that changed the card, so capture
/// here never sees it unmarked and never judges it. A failed save shares
/// nothing.
pub fn save_gpu_switch(
    shared: &SharedControl,
    save: impl FnOnce(&mut Option<crate::gpu_preference::SwitchRecord>) -> Result<(), String>,
) -> Result<(), String> {
    let mut record = shared.gpu_switch.lock().clone();
    save(&mut record)?;
    let mut shared_record = shared.gpu_switch.lock();
    shared.gpu_changed_here.store(true, Ordering::SeqCst);
    *shared_record = record;
    Ok(())
}

/// The capture loop could not get at the desktop for a reason outside the
/// app's control. The stage retries instead of failing the pipeline.
#[derive(Debug)]
pub struct CaptureUnavailable {
    pub block: CaptureBlock,
    pub detail: String,
}

impl std::fmt::Display for CaptureUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "capture unavailable ({:?}): {}", self.block, self.detail)
    }
}

impl std::error::Error for CaptureUnavailable {}

/// The capture loop ended to open its display afresh: after a lost
/// duplication the target is now another output or adapter, or a re-created
/// duplication showed no new image for too long. The stage opens it again at
/// once, not at the blocked pace.
#[derive(Debug)]
pub struct Reopen {
    pub detail: String,
}

impl std::fmt::Display for Reopen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "capture reopens: {}", self.detail)
    }
}

impl std::error::Error for Reopen {}

/// A [`Reopen`] within this long of the last one waits at the blocked pace:
/// a layout that keeps changing must not reopen the display in a loop.
const REOPEN_SPACING: Duration = Duration::from_secs(1);

/// Delays between duplication re-creations after ACCESS_LOST. The first retry
/// is immediate, as before; a duplication that is lost again right after it
/// was re-created waits longer each time instead of spinning.
const LOSS_DELAYS_MS: [u64; 6] = [0, 100, 250, 500, 1000, 2000];
/// After this long without a working duplication the loop hands the problem
/// to the stage, which rebuilds the DXGI factory and device (an output can
/// move to another adapter on hybrid laptops and docks).
pub const LOSS_GIVE_UP: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub enum LossStep {
    Retry(Duration),
    GiveUp,
}

/// A duplication that came back keeps its place on the delay ladder until it
/// has worked this long. A UAC prompt, the lock screen or a full-screen game
/// can take the desktop again right after it returns; starting the ladder
/// over at each return re-created the duplication at full speed in a loop.
/// It was 5 s, and on a PC whose desktop was taken again every 1 to 4 s the
/// ladder never came down from 2 s: each loss cost 2 s without a picture,
/// and the iPad got 3 to 17 fps for an hour. A duplication that worked for
/// a quarter of a second is re-created at once, so a steady stream of quick
/// losses costs tens of milliseconds each, and re-creations still stay
/// under four a second.
pub const LOSS_LADDER_RESET: Duration = Duration::from_millis(250);

/// A loss that is back within this long is routine (the lock screen, a UAC
/// prompt, a mode change) and is logged at info; a longer one is a warning.
pub const LOSS_QUIET: Duration = Duration::from_secs(1);

/// One loss episode lasts from the first ACCESS_LOST until a frame or an idle
/// timeout proves the new duplication works.
#[derive(Default)]
pub struct LossRecovery {
    since: Option<Instant>,
    /// Losses this episode.
    losses: u32,
    /// Steps up the delay ladder, kept across an episode's end until the
    /// duplication has worked for [`LOSS_LADDER_RESET`].
    step: usize,
    /// When the last episode ended, while `step` is kept.
    back_at: Option<Instant>,
    /// This episode outlasted [`LOSS_QUIET`] and said so.
    overdue: bool,
}

impl LossRecovery {
    pub fn in_episode(&self) -> bool {
        self.since.is_some()
    }

    /// True once per episode, when it has lasted [`LOSS_QUIET`].
    pub fn overdue(&mut self, now: Instant) -> bool {
        let Some(since) = self.since else {
            return false;
        };
        if self.overdue || now.duration_since(since) < LOSS_QUIET {
            return false;
        }
        self.overdue = true;
        true
    }

    pub fn lost(&mut self, now: Instant) -> LossStep {
        if let Some(back_at) = self.back_at.take() {
            if now.duration_since(back_at) >= LOSS_LADDER_RESET {
                self.step = 0;
            }
        }
        let since = *self.since.get_or_insert(now);
        self.losses += 1;
        if now.duration_since(since) >= LOSS_GIVE_UP {
            return LossStep::GiveUp;
        }
        let index = self.step.min(LOSS_DELAYS_MS.len() - 1);
        self.step += 1;
        LossStep::Retry(Duration::from_millis(LOSS_DELAYS_MS[index]))
    }

    /// Called for each frame or idle timeout. Ends the episode, returning how
    /// many losses it had and how long it took; once the duplication has
    /// worked for [`LOSS_LADDER_RESET`], the next loss is retried at once
    /// again.
    pub fn healthy(&mut self, now: Instant) -> Option<(u32, Duration)> {
        let Some(since) = self.since.take() else {
            if self
                .back_at
                .is_some_and(|back_at| now.duration_since(back_at) >= LOSS_LADDER_RESET)
            {
                self.back_at = None;
                self.step = 0;
            }
            return None;
        };
        self.back_at = Some(now);
        self.overdue = false;
        let losses = std::mem::take(&mut self.losses);
        Some((losses, now.duration_since(since)))
    }
}

/// While capture is blocked the stage retries with a fresh factory at this
/// pace. The blocked states clear when the user acts, so the first retries
/// are quick; a display Windows will not duplicate settles at the cap.
const BLOCKED_RETRY_MS: [u64; 4] = [500, 1000, 2000, 5000];

/// Retry cadence and logging for the capture stage while Windows blocks it.
#[derive(Default)]
pub struct BlockedRetry {
    current: Option<CaptureBlock>,
    attempts: u32,
}

impl BlockedRetry {
    /// Records a blocked attempt. Returns the wait before the next one and
    /// whether this is a new state worth a log line.
    pub fn blocked(&mut self, block: CaptureBlock) -> (Duration, bool) {
        let changed = self.current != Some(block);
        if changed {
            self.current = Some(block);
            self.attempts = 0;
        }
        let index = (self.attempts as usize).min(BLOCKED_RETRY_MS.len() - 1);
        self.attempts += 1;
        (Duration::from_millis(BLOCKED_RETRY_MS[index]), changed)
    }

    /// Capture worked since the last blocked attempt. The next block is
    /// retried at the quick pace again but is not logged again when it is
    /// the same state: the pace climbed over a whole app run, and a PC
    /// locked for the fourth time waited up to 5 s after it was unlocked.
    pub fn resumed(&mut self) {
        self.attempts = 0;
    }
}

/// Runs `capture` until it ends for a reason other than a blocked desktop.
/// While blocked, the stage keeps its heartbeats: restarting the pipeline
/// cannot unlock the PC, and a restart would drop the iPad's session.
/// `wait` sleeps in short slices and returns false once this generation
/// should stop.
///
/// Every attempt numbers its frames from where the last one stopped. Frame
/// numbers become the stream's sequence, and a retry keeps the transport's
/// stream epoch: an attempt that started again at 1 sent sequence 1 after
/// 200, and the iPad dropped every frame until the count passed 200 again.
pub fn run_until_unblocked(
    shared: &SharedControl,
    capture: impl FnMut(&mut u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
    wait: impl FnMut(Duration) -> bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_until_unblocked_on(shared, capture, wait, Instant::now)
}

/// [`run_until_unblocked`] on the clock `clock`, which times the
/// [`REOPEN_SPACING`].
pub(crate) fn run_until_unblocked_on(
    shared: &SharedControl,
    mut capture: impl FnMut(&mut u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
    mut wait: impl FnMut(Duration) -> bool,
    mut clock: impl FnMut() -> Instant,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut retry = BlockedRetry::default();
    let mut stalled = 0;
    let mut frame_number = 0;
    let mut last_reopen: Option<Instant> = None;
    loop {
        let error = match capture(&mut frame_number) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        let error: Box<dyn std::error::Error + Send + Sync> = match error.downcast::<Reopen>() {
            Ok(reopen) => {
                let now = clock();
                if last_reopen.is_none_or(|at| now.duration_since(at) >= REOPEN_SPACING) {
                    last_reopen = Some(now);
                    info!(detail = %reopen.detail, "Opening the display to capture again");
                    continue;
                }
                Box::new(CaptureUnavailable {
                    block: CaptureBlock::DisplayChanging,
                    detail: reopen.detail,
                })
            }
            Err(error) => error,
        };
        // A driver call that did not return in time, or was refused while an
        // earlier one is stuck. Restarting the pipeline cannot free the
        // driver, and refusals would spend the restart budget until the
        // supervisor gave up for good, with nothing to start capture again
        // once the call returns. Retry here at the blocked pace instead.
        if error.downcast_ref::<crate::stall::Stall>().is_some() {
            let delay = Duration::from_millis(
                BLOCKED_RETRY_MS[(stalled as usize).min(BLOCKED_RETRY_MS.len() - 1)],
            );
            if stalled == 0 {
                warn!(
                    error = %error,
                    "A graphics driver call is stuck — retrying capture until it returns"
                );
            }
            // Tell the GUI why; the first working capture clears it.
            *shared.capture_block.lock() = Some(CaptureBlock::DriverNotResponding);
            stalled += 1;
            if !wait(delay) {
                return Ok(());
            }
            continue;
        }
        stalled = 0;
        #[cfg(windows)]
        let error = access_lost_is_blocked(error);
        let Some(unavailable) = error.downcast_ref::<CaptureUnavailable>() else {
            return Err(error);
        };
        // The first working frame clears the block (see
        // [`note_capture_resumed`]).
        if shared
            .capture_block
            .lock()
            .replace(unavailable.block)
            .is_none()
        {
            retry.resumed();
        }
        let (delay, changed) = retry.blocked(unavailable.block);
        if changed {
            warn!(
                block = ?unavailable.block,
                detail = %unavailable.detail,
                "Windows is not letting EternalMonitor capture the screen — retrying until it does"
            );
        }
        if !wait(delay) {
            return Ok(());
        }
    }
}

/// DXGI_ERROR_ACCESS_LOST that left the capture loop instead of reaching its
/// handler: a display change the stage waits out, never a pipeline failure.
/// The loop hands ACCESS_LOST from every call it knows to its handler; this
/// is for any call it does not.
#[cfg(windows)]
fn access_lost_is_blocked(
    error: Box<dyn std::error::Error + Send + Sync>,
) -> Box<dyn std::error::Error + Send + Sync> {
    match error.downcast_ref::<windows::core::Error>() {
        Some(lost) if lost.code().0 as u32 == 0x887A_0026 => Box::new(CaptureUnavailable {
            block: CaptureBlock::DisplayChanging,
            detail: format!("desktop duplication lost outside its handler: {lost}"),
        }),
        _ => error,
    }
}

/// The heartbeat's notice while capture is blocked. A secure screen says the
/// PC is locked, and no display attached says so, so the iPad doesn't show a
/// frozen picture without a word; it clears with the block when capture
/// works again. The other blocks keep the held picture as before.
pub fn heartbeat_notice(block: Option<CaptureBlock>) -> u8 {
    use eternal_wire::v2::control::{NOTICE_NONE, NOTICE_NO_DISPLAY, NOTICE_PC_LOCKED};
    match block {
        Some(CaptureBlock::SecureDesktop) => NOTICE_PC_LOCKED,
        Some(CaptureBlock::NoDisplay) => NOTICE_NO_DISPLAY,
        _ => NOTICE_NONE,
    }
}

/// Clears the blocked state once frames flow again.
pub fn note_capture_resumed(shared: &SharedControl) {
    if let Some(block) = shared.capture_block.lock().take() {
        info!(block = ?block, "Screen capture resumed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn the_process_that_saved_a_switch_never_judges_it() {
        use crate::gpu_preference::{Outcome, Preference, SwitchRecord};
        let shared = SharedControl::new(9876, 15_000_000);
        let waiting = SwitchRecord::new(None, None, Preference::HighPerformance, "NVIDIA");
        *shared.gpu_switch.lock() = Some(waiting.clone());
        // The old window saved it and still runs on the old card (its
        // restart failed, or it is about to leave): no "another app" verdict.
        shared.gpu_changed_here.store(true, Ordering::SeqCst);
        note_gpu_outcome(&shared, Outcome::StillUnsupported);
        assert_eq!(*shared.gpu_switch.lock(), Some(waiting.clone()));
        // The restarted copy judges it.
        let restarted = SharedControl::new(9876, 15_000_000);
        *restarted.gpu_switch.lock() = Some(waiting);
        note_gpu_outcome(&restarted, Outcome::StillUnsupported);
        assert!(restarted.gpu_switch.lock().as_ref().unwrap().failed);
    }

    /// EternalMonitor's value, with capture on the old card retrying while
    /// it is written.
    struct RetriedDuringWrite<'a> {
        value: Option<String>,
        shared: &'a SharedControl,
        refused: bool,
    }

    impl crate::gpu_preference::PreferenceStore for RetriedDuringWrite<'_> {
        fn read(&self) -> Result<Option<String>, String> {
            Ok(self.value.clone())
        }
        fn write(&mut self, value: Option<&str>) -> Result<(), String> {
            note_gpu_outcome(
                self.shared,
                crate::gpu_preference::Outcome::StillUnsupported,
            );
            if self.refused {
                return Err("access denied".into());
            }
            self.value = value.map(Into::into);
            Ok(())
        }
    }

    #[test]
    fn capture_retrying_during_a_switch_saved_here_never_judges_it() {
        use crate::gpu_preference::{switch, Outcome, Preference, SwitchRecord};
        let shared = SharedControl::new(9876, 15_000_000);
        let file = RefCell::new(None);
        let mut store = RetriedDuringWrite {
            value: None,
            shared: &shared,
            refused: false,
        };
        // A retry at every step: after settings.json has the record, during
        // the write (the store above), and once the record is shared.
        save_gpu_switch(&shared, |record| {
            switch(
                &mut store,
                record,
                Preference::HighPerformance,
                "NVIDIA",
                |saved| {
                    *file.borrow_mut() = saved.clone();
                    note_gpu_outcome(&shared, Outcome::StillUnsupported);
                    Ok(())
                },
            )
        })
        .unwrap();
        note_gpu_outcome(&shared, Outcome::StillUnsupported);
        let record: SwitchRecord = shared.gpu_switch.lock().clone().unwrap();
        assert!(record.checking && !record.failed, "{record:?}");
        assert_eq!(*file.borrow(), Some(record));
        assert!(shared.gpu_changed_here.load(Ordering::SeqCst));
    }

    #[test]
    fn a_refused_switch_shares_nothing_and_capture_still_judges_an_earlier_one() {
        use crate::gpu_preference::{switch, Outcome, Preference, SwitchRecord};
        // A restarted copy still waiting to judge an earlier switch.
        let shared = SharedControl::new(9876, 15_000_000);
        let earlier = SwitchRecord::new(None, None, Preference::PowerSaving, "AMD");
        *shared.gpu_switch.lock() = Some(earlier.clone());
        let mut store = RetriedDuringWrite {
            value: Some("GpuPreference=1;".into()),
            shared: &shared,
            refused: true,
        };
        let result = save_gpu_switch(&shared, |record| {
            switch(
                &mut store,
                record,
                Preference::HighPerformance,
                "NVIDIA",
                |_| Ok(()),
            )
        });
        assert_eq!(result, Err("access denied".into()));
        assert!(!shared.gpu_changed_here.load(Ordering::SeqCst));
        // This process still runs on the card the value names: the retry
        // during the write judged the earlier switch, and that verdict stays.
        let record = shared.gpu_switch.lock().clone().unwrap();
        assert!(record.failed && !record.checking, "{record:?}");
        assert_eq!(record.gpu, earlier.gpu);
        note_gpu_outcome(&shared, Outcome::StillUnsupported);
        assert!(shared.gpu_switch.lock().as_ref().unwrap().failed);
    }

    #[test]
    fn only_a_secure_screen_or_no_display_has_a_notice_for_the_ipad() {
        use eternal_wire::v2::control::{NOTICE_NONE, NOTICE_NO_DISPLAY, NOTICE_PC_LOCKED};
        assert_eq!(
            heartbeat_notice(Some(CaptureBlock::SecureDesktop)),
            NOTICE_PC_LOCKED
        );
        // A closed lid or screens that are off: the field's PCs showed a
        // frozen picture without a word until a display came back.
        assert_eq!(
            heartbeat_notice(Some(CaptureBlock::NoDisplay)),
            NOTICE_NO_DISPLAY
        );
        assert_eq!(heartbeat_notice(None), NOTICE_NONE);
        for block in [
            CaptureBlock::DisplayChanging,
            CaptureBlock::UnsupportedOnThisGpu,
            CaptureBlock::TooManyCaptureApps,
            CaptureBlock::SessionDisconnected,
            CaptureBlock::DriverNotResponding,
        ] {
            assert_eq!(heartbeat_notice(Some(block)), NOTICE_NONE, "{block:?}");
        }
    }

    #[test]
    fn a_stuck_driver_call_is_waited_out_in_the_stage_not_restarted() {
        use crate::stall::{no_hooks, run_in, Call, Lanes, Stall, Wait};
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
        let shared = SharedControl::new(9876, 15_000_000);
        let lanes: &'static Lanes = Box::leak(Box::new(Lanes::new()));
        let stepping = || {
            let start = Instant::now();
            let reads = AtomicU64::new(0);
            move || start + Duration::from_millis(100 * reads.fetch_add(1, SeqCst))
        };
        let (release, released) = std::sync::mpsc::channel::<()>();
        let mut stuck = Some(move || {
            let _ = released.recv();
        });
        let mut release = Some(release);
        let mut outcomes = Vec::new();
        let mut waited = Duration::ZERO;
        // Each attempt makes the device call through the real runner: the
        // first hangs and times out, later ones are refused until it returns.
        let result = run_until_unblocked(
            &shared,
            |_| {
                let call = stuck.take();
                let result = run_in(
                    lanes,
                    Call::D3d11CreateDevice,
                    Wait {
                        limits: Call::D3d11CreateDevice.limits(),
                        slice: Duration::from_millis(1),
                        now: stepping(),
                    },
                    move || {
                        if let Some(call) = call {
                            call();
                        }
                    },
                    || {},
                    no_hooks(),
                );
                outcomes.push(result);
                result.map_err(|stall: Stall| stall.into())
            },
            |delay| {
                assert_eq!(
                    *shared.capture_block.lock(),
                    Some(CaptureBlock::DriverNotResponding)
                );
                waited += delay;
                // Over a minute in, the driver returns the stuck call.
                if waited >= Duration::from_secs(65) {
                    if let Some(release) = release.take() {
                        release.send(()).unwrap();
                        let deadline = Instant::now() + Duration::from_secs(60);
                        while lanes.abandoned(Call::D3d11CreateDevice) != 0 {
                            assert!(Instant::now() < deadline, "the lane did not free");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                }
                true
            },
        );
        // Capture resumed by itself, and nothing reached the supervisor.
        assert!(result.is_ok(), "{result:?}");
        assert!(matches!(outcomes[0], Err(Stall::TimedOut { .. })));
        let refused = outcomes[1..outcomes.len() - 1]
            .iter()
            .all(|outcome| matches!(outcome, Err(Stall::Refused { .. })));
        assert!(refused, "{outcomes:?}");
        assert_eq!(outcomes.last(), Some(&Ok(())));
        // At the blocked pace, not a spin: 5 s apart once settled.
        assert!(outcomes.len() < 25, "{} attempts", outcomes.len());
        // The GUI said why throughout; frames flowing again clear it.
        assert_eq!(
            *shared.capture_block.lock(),
            Some(CaptureBlock::DriverNotResponding)
        );
        note_capture_resumed(&shared);
        assert!(shared.capture_block.lock().is_none());
        // A real failure still goes straight to the supervisor's budget.
        let mut attempts = 0;
        let failed = run_until_unblocked(
            &shared,
            |_| {
                attempts += 1;
                Err("DuplicateOutput failed".into())
            },
            |_| panic!("a real failure was retried"),
        );
        assert!(failed.is_err());
        assert_eq!(attempts, 1);
    }

    #[test]
    fn repeated_losses_back_off_instead_of_spinning() {
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        let delays: Vec<_> = (0..8)
            .map(|i| recovery.lost(start + Duration::from_millis(i)))
            .collect();
        let ms = |ms| LossStep::Retry(Duration::from_millis(ms));
        assert_eq!(
            delays,
            vec![
                ms(0),
                ms(100),
                ms(250),
                ms(500),
                ms(1000),
                ms(2000),
                ms(2000),
                ms(2000)
            ]
        );
        // The 0.3.1 loop made ~150 attempts per second. Over the whole give-up
        // window the ladder now makes at most a handful.
        let total: u64 = LOSS_DELAYS_MS.iter().sum();
        let attempts =
            LOSS_DELAYS_MS.len() as u64 + (LOSS_GIVE_UP.as_millis() as u64 - total) / 2000;
        assert!(attempts < 12, "{attempts} attempts");
    }

    #[test]
    fn a_working_frame_ends_the_episode_and_a_healthy_quarter_second_resets_the_ladder() {
        let ms = Duration::from_millis;
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        recovery.lost(start);
        recovery.lost(start);
        assert!(recovery.in_episode());
        let ended = recovery.healthy(start + ms(300));
        assert_eq!(ended, Some((2, ms(300))));
        assert!(!recovery.in_episode());
        // Frames keep coming for a quarter of a second: the next loss is
        // retried at once.
        let mut now = start + ms(300);
        while now < start + ms(300) + LOSS_LADDER_RESET {
            now += ms(16);
            assert_eq!(recovery.healthy(now), None);
        }
        assert_eq!(recovery.lost(now), LossStep::Retry(Duration::ZERO));
        // Also when no frame was looked at in between (nobody watching).
        let back = now + ms(10);
        assert!(recovery.healthy(back).is_some());
        let later = back + LOSS_LADDER_RESET;
        assert_eq!(recovery.lost(later), LossStep::Retry(Duration::ZERO));
    }

    /// A UAC prompt or a full-screen game takes the desktop again right after
    /// each return. The first retry is as quick as before; the returns that
    /// follow within a quarter of a second keep climbing the ladder instead of
    /// starting over at 0 ms each time.
    #[test]
    fn a_duplication_lost_again_soon_after_it_returned_keeps_its_backoff() {
        let ms = Duration::from_millis;
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        let mut now = start;
        let mut delays = Vec::new();
        for _ in 0..10 {
            let LossStep::Retry(delay) = recovery.lost(now) else {
                panic!("gave up")
            };
            delays.push(delay.as_millis() as u64);
            // The new duplication works for 200 ms, then is lost again.
            now += delay + ms(1);
            assert!(
                recovery.healthy(now).is_some(),
                "each loss is its own episode"
            );
            now += ms(200);
            assert_eq!(recovery.healthy(now), None);
        }
        assert_eq!(
            delays,
            [0, 100, 250, 500, 1000, 2000, 2000, 2000, 2000, 2000]
        );
        // The last return was 200 ms ago. 240 healthy ms are not enough;
        // 250 are.
        let back = now - ms(200);
        assert_eq!(recovery.healthy(back + ms(240)), None);
        assert_eq!(recovery.lost(back + ms(240)), LossStep::Retry(ms(2000)));
        recovery.healthy(back + ms(2_300));
        assert_eq!(recovery.healthy(back + ms(2_550)), None);
        assert_eq!(
            recovery.lost(back + ms(2_550)),
            LossStep::Retry(Duration::ZERO)
        );
        // Each return still ends its episode: the give-up window measures
        // one outage, not the whole loop.
        assert!(now.duration_since(start) > LOSS_GIVE_UP);
    }

    /// The field's looping PC (0.3.4): the desktop was taken again 1 to 4 s
    /// after each return, for about an hour. With the ladder held for 5 s
    /// every retry waited 2 s, and the iPad got 3 to 17 fps. A duplication
    /// that worked for a quarter of a second is re-created at once.
    #[test]
    fn a_desktop_taken_every_few_seconds_is_retried_at_once() {
        let ms = Duration::from_millis;
        // How long each re-created duplication worked, from that PC's log,
        // and two quicker losses.
        let lives = [3006, 1789, 650, 1227, 2008, 300, 2260, 1005].map(ms);
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        let mut now = start;
        let mut without_picture = Duration::ZERO;
        let mut losses = 0;
        while now < start + Duration::from_secs(60) {
            let LossStep::Retry(delay) = recovery.lost(now) else {
                panic!("gave up")
            };
            losses += 1;
            assert_eq!(delay, Duration::ZERO, "loss {losses} waited");
            without_picture += delay;
            now += delay;
            assert!(recovery.healthy(now).is_some());
            now += lives[losses % lives.len()];
        }
        assert!(
            without_picture < ms(100),
            "{without_picture:?} without a picture in a minute"
        );
        // Still never a spin: at most four re-creations a second.
        assert!(losses <= 240, "{losses} re-creations in a minute");
    }

    /// ACCESS_LOST from a call the capture loop does not hand to its handler
    /// (on 0.3.4, ReleaseFrame after an acquire: "Capture loop exited with
    /// error" seven times in an hour, each a pipeline restart). The stage
    /// waits it out like any display change, however often it comes.
    #[cfg(windows)]
    #[test]
    fn access_lost_that_leaves_the_capture_loop_never_fails_the_stage() {
        let shared = SharedControl::new(0, 1);
        let mut attempts = 0;
        let mut blocks = Vec::new();
        let result = run_until_unblocked(
            &shared,
            |_| {
                attempts += 1;
                if attempts <= 7 {
                    Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                        0x887A_0026_u32 as i32,
                    ))
                    .into())
                } else {
                    Ok(())
                }
            },
            |_| {
                blocks.push(*shared.capture_block.lock());
                true
            },
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(attempts, 8);
        assert_eq!(blocks, [Some(CaptureBlock::DisplayChanging); 7]);
        // Other Windows errors still reach the supervisor.
        let failed = run_until_unblocked(
            &shared,
            |_| {
                Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                    0x887A_0005_u32 as i32,
                ))
                .into())
            },
            |_| panic!("a removed device was retried in the stage"),
        );
        assert!(failed.is_err());
    }

    #[test]
    fn only_a_loss_not_back_within_a_second_is_a_warning() {
        let ms = Duration::from_millis;
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        assert!(!recovery.overdue(start), "no episode yet");
        recovery.lost(start);
        assert!(!recovery.overdue(start + ms(999)));
        recovery.lost(start + ms(500));
        assert!(recovery.overdue(start + LOSS_QUIET));
        assert!(!recovery.overdue(start + ms(5_000)), "said once");
        recovery.healthy(start + ms(5_000));
        // The next episode is judged on its own.
        let next = start + ms(9_000);
        recovery.lost(next);
        assert!(recovery.healthy(next + ms(300)).is_some());
        assert!(!recovery.overdue(next + ms(2_000)));
        recovery.lost(next + ms(3_000));
        assert!(!recovery.overdue(next + ms(3_999)));
        assert!(recovery.overdue(next + ms(4_000)));
    }

    /// Review r1 of #141: "No display is on" told a desktop PC to open the
    /// laptop lid while a display was unplugged or a driver updated. Only
    /// an enumeration that found no output at all is a display that's off.
    #[test]
    fn only_no_listed_output_at_all_says_no_display_is_on() {
        let not_found = 0x887A_0002_u32 as i32;
        assert_eq!(
            CaptureBlock::opening(OpenStep::OnlyOutput, not_found),
            Some(CaptureBlock::NoDisplay)
        );
        // A listed output gone by the time it opens: unplugged, Win+P.
        assert_eq!(
            CaptureBlock::opening(OpenStep::ListedOutput, not_found),
            Some(CaptureBlock::DisplayChanging)
        );
        // No adapter listed, as during a driver update.
        assert_eq!(
            CaptureBlock::opening(OpenStep::Adapter, not_found),
            Some(CaptureBlock::DisplayChanging)
        );
        assert_eq!(
            CaptureBlock::from_hresult(not_found),
            Some(CaptureBlock::DisplayChanging)
        );
        // Other codes map as anywhere else.
        let access_denied = 0x8007_0005_u32 as i32;
        for step in [
            OpenStep::Adapter,
            OpenStep::ListedOutput,
            OpenStep::OnlyOutput,
        ] {
            assert_eq!(
                CaptureBlock::opening(step, access_denied),
                Some(CaptureBlock::SecureDesktop)
            );
            assert_eq!(CaptureBlock::opening(step, 0x8007_000E_u32 as i32), None);
        }
    }

    /// Lid closed for two minutes: every attempt finds no display output
    /// (DXGI_ERROR_NOT_FOUND), then the lid opens.
    #[test]
    fn no_display_output_is_waited_out_without_the_restart_brake() {
        use crate::supervisor::{Effect, Machine, Stage, StageOutcome, SupervisorState};
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        use tracing_subscriber::prelude::*;

        struct Warnings(Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Warnings {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() <= tracing::Level::WARN {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
        }

        let not_found = 0x887A_0002_u32 as i32;
        assert_eq!(
            CaptureBlock::opening(OpenStep::OnlyOutput, not_found),
            Some(CaptureBlock::NoDisplay)
        );
        // Not remembered against the second screen either.
        assert!(passing_code(not_found));
        let lid_closed_for = Duration::from_secs(120);

        let shared = SharedControl::new(0, 1);
        let waited = RefCell::new(Duration::ZERO);
        let waits = RefCell::new(Vec::new());
        let warnings = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Warnings(warnings.clone()));
        crate::logging::prepare_log_capture();
        let result = tracing::subscriber::with_default(subscriber, || {
            run_until_unblocked(
                &shared,
                |_| {
                    if *waited.borrow() < lid_closed_for {
                        let block = CaptureBlock::opening(OpenStep::OnlyOutput, not_found).unwrap();
                        return Err(Box::new(CaptureUnavailable {
                            block,
                            detail: "EnumOutputs: 0x887a0002".into(),
                        }));
                    }
                    note_capture_resumed(&shared);
                    Ok(())
                },
                |delay| {
                    assert_eq!(*shared.capture_block.lock(), Some(CaptureBlock::NoDisplay));
                    waits.borrow_mut().push(delay);
                    *waited.borrow_mut() += delay;
                    true
                },
            )
        });
        // Capture resumed by itself; nothing reached the supervisor.
        assert!(result.is_ok(), "{result:?}");
        assert!(shared.capture_block.lock().is_none());
        // A bounded pace, never a tight loop.
        let waits = waits.into_inner();
        assert!(waits
            .iter()
            .all(|&d| d >= Duration::from_millis(500) && d <= Duration::from_secs(5)));
        assert!(waits.len() < 30, "{} attempts", waits.len());
        // One warning for the whole wait.
        assert_eq!(warnings.load(Ordering::SeqCst), 1);

        // Before, each attempt ended the stage: the brake stopped capture
        // half a minute into the two, until the user clicked Start.
        let mut machine = Machine::new(1);
        let start = Instant::now();
        let mut now = start;
        let mut generation = 1;
        let failure = StageOutcome::Failed("EnumOutputs: 0x887a0002".into());
        while !matches!(machine.state, SupervisorState::Failed { .. }) {
            assert!(now.duration_since(start) < lid_closed_for);
            assert_eq!(
                machine.on_stage_exit(generation, Stage::Capture, &failure, now),
                Effect::StopPipeline
            );
            if let SupervisorState::BackingOff { until, .. } = machine.state {
                now = until;
                assert_eq!(machine.on_tick(now), Effect::Start);
                generation += 1;
                machine.note_started(generation);
            }
        }
        assert!(now.duration_since(start) < Duration::from_secs(60));
    }

    #[test]
    fn a_long_episode_gives_up_so_the_stage_can_rebuild_the_device() {
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        recovery.lost(start);
        assert_eq!(recovery.lost(start + LOSS_GIVE_UP), LossStep::GiveUp);
    }

    #[test]
    fn known_hresults_map_to_stable_blocks() {
        let code = |hex: u32| hex as i32;
        assert_eq!(
            CaptureBlock::from_hresult(code(0x8007_0005)),
            Some(CaptureBlock::SecureDesktop)
        );
        assert_eq!(
            CaptureBlock::from_hresult(code(0x887A_0004)),
            Some(CaptureBlock::UnsupportedOnThisGpu)
        );
        assert_eq!(
            CaptureBlock::from_hresult(code(0x887A_0026)),
            Some(CaptureBlock::DisplayChanging)
        );
        assert_eq!(CaptureBlock::from_hresult(code(0x8007_000E)), None);
    }

    #[test]
    fn only_a_display_this_gpu_cannot_duplicate_is_the_pcs_failure() {
        let unavailable = |block| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(CaptureUnavailable {
                block,
                detail: String::new(),
            })
        };
        for block in [
            CaptureBlock::SecureDesktop,
            CaptureBlock::DisplayChanging,
            CaptureBlock::NoDisplay,
            CaptureBlock::TooManyCaptureApps,
            CaptureBlock::SessionDisconnected,
        ] {
            assert!(passing_error(unavailable(block).as_ref()), "{block:?}");
        }
        assert!(!passing_error(
            unavailable(CaptureBlock::UnsupportedOnThisGpu).as_ref()
        ));
        let other: Box<dyn std::error::Error + Send + Sync> = "DuplicateOutput failed".into();
        assert!(!passing_error(other.as_ref()));
        let call = crate::stall::Call::DuplicateOutput;
        for stall in [
            crate::stall::Stall::TimedOut {
                call,
                after: Duration::from_secs(3),
            },
            crate::stall::Stall::Refused { call },
        ] {
            let error: Box<dyn std::error::Error + Send + Sync> = Box::new(stall);
            assert!(passing_error(error.as_ref()), "{stall:?}");
        }
        let code = |hresult: u32| hresult as i32;
        for passing in [
            0x8007_0005, // E_ACCESSDENIED
            0x887A_0022, // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
            0x887A_0028, // DXGI_ERROR_SESSION_DISCONNECTED
            0x887A_0005, // DXGI_ERROR_DEVICE_REMOVED
            0x887A_0006, // DXGI_ERROR_DEVICE_HUNG
            0x887A_0007, // DXGI_ERROR_DEVICE_RESET
            0x887A_0026, // DXGI_ERROR_ACCESS_LOST
            0x8007_000E, // E_OUTOFMEMORY, until another app frees memory
        ] {
            assert!(passing_code(code(passing)), "{passing:#x}");
        }
        for persistent in [0x887A_0004, 0x8007_0057, 0x8000_4005] {
            assert!(!passing_code(code(persistent)), "{persistent:#x}");
        }
    }

    fn blocked(block: CaptureBlock) -> Box<dyn std::error::Error + Send + Sync> {
        Box::new(CaptureUnavailable {
            block,
            detail: "test".into(),
        })
    }

    #[test]
    fn a_blocked_desktop_is_retried_without_failing_the_stage() {
        let shared = SharedControl::new(0, 1);
        let attempts = RefCell::new(0);
        let waits = RefCell::new(Vec::new());
        let result = run_until_unblocked(
            &shared,
            |_| {
                *attempts.borrow_mut() += 1;
                match *attempts.borrow() {
                    1..=3 => Err(blocked(CaptureBlock::SecureDesktop)),
                    _ => {
                        // Frames flow again.
                        note_capture_resumed(&shared);
                        Ok(())
                    }
                }
            },
            |delay| {
                waits.borrow_mut().push(delay.as_millis());
                // The state is visible to the GUI while waiting.
                assert_eq!(
                    *shared.capture_block.lock(),
                    Some(CaptureBlock::SecureDesktop)
                );
                true
            },
        );
        assert!(result.is_ok());
        assert_eq!(*waits.borrow(), vec![500, 1000, 2000]);
        assert_eq!(*shared.capture_block.lock(), None);
    }

    /// Frames the iPad shows, in order, when one frame per sequence number
    /// arrives in a single stream epoch.
    fn shown(sequence: &[u64], repair: bool) -> Vec<u64> {
        use eternal_wire::reassembly::{AddOutcome, Reassembler};
        use eternal_wire::v2::media::MediaHeader;
        let mut ipad = Reassembler::new();
        ipad.configure_repair(
            repair,
            Duration::from_micros(16_667),
            Duration::from_millis(10),
        );
        let now = Instant::now();
        let mut shown = Vec::new();
        for &seq in sequence {
            let header = MediaHeader {
                session_id: 1,
                stream_epoch: 7,
                frame_seq: seq as u32,
                frag_index: 0,
                frag_count: 1,
                is_keyframe: seq == 1,
                is_retransmit: false,
                capture_ts_us: seq * 16_667,
                payload_len: 4,
            };
            if let AddOutcome::Completed(_) = ipad.add_media(header, &[0; 4], now) {
                shown.push(seq);
            }
            shown.extend(ipad.take_ready().iter().map(|frame| u64::from(frame.seq)));
        }
        shown
    }

    #[test]
    fn a_retry_continues_the_frame_numbers_the_ipad_is_showing() {
        let shared = SharedControl::new(0, 1);
        let attempts = RefCell::new(0);
        let sent = RefCell::new(Vec::new());
        let result = run_until_unblocked(
            &shared,
            |frame_number| {
                *attempts.borrow_mut() += 1;
                // 200 frames, then Windows withholds the desktop past the
                // loop's give-up window. The retry captures one frame.
                let first = *attempts.borrow() == 1;
                let frames = if first { 200 } else { 1 };
                for _ in 0..frames {
                    *frame_number += 1;
                    sent.borrow_mut().push(*frame_number);
                }
                if first {
                    Err(blocked(CaptureBlock::DisplayChanging))
                } else {
                    Ok(())
                }
            },
            |_| true,
        );
        assert!(result.is_ok());
        let sent = sent.into_inner();
        assert_eq!(sent.last(), Some(&201));

        let mut restarted: Vec<u64> = (1..=200).collect();
        restarted.push(1);
        for repair in [false, true] {
            // The previous loops counted from zero again: the iPad dropped
            // the recovered frame and kept the frozen picture.
            assert_eq!(shown(&restarted, repair).last(), Some(&200), "{repair}");
            assert_eq!(shown(&sent, repair).last(), Some(&201), "{repair}");
        }
    }

    #[test]
    fn real_errors_still_reach_the_supervisor_and_stop_ends_the_wait() {
        let shared = SharedControl::new(0, 1);
        let result = run_until_unblocked(&shared, |_| Err("device removed".into()), |_| true);
        assert_eq!(result.unwrap_err().to_string(), "device removed");

        let result = run_until_unblocked(
            &shared,
            |_| Err(blocked(CaptureBlock::UnsupportedOnThisGpu)),
            |_| false,
        );
        assert!(result.is_ok(), "a stopped generation exits cleanly");
    }

    /// The lock screen, four times in one app run. Each unlock is retried
    /// as quickly as the first, not 5 s later.
    #[test]
    fn a_block_after_capture_worked_again_is_retried_at_the_quick_pace() {
        let shared = SharedControl::new(0, 1);
        let attempts = RefCell::new(0);
        let waits = RefCell::new(Vec::new());
        let result = run_until_unblocked(
            &shared,
            |_| {
                *attempts.borrow_mut() += 1;
                let attempt = *attempts.borrow();
                if attempt > 12 {
                    return Ok(());
                }
                // Three blocked attempts, then the user unlocks and capture
                // works until the next lock.
                if attempt % 3 == 0 {
                    note_capture_resumed(&shared);
                }
                Err(blocked(CaptureBlock::SecureDesktop))
            },
            |delay| {
                waits.borrow_mut().push(delay.as_millis());
                true
            },
        );
        assert!(result.is_ok());
        assert_eq!(
            *waits.borrow(),
            [500, 1000, 500, 1000, 2000, 500, 1000, 2000, 500, 1000, 2000, 500]
        );
    }

    /// Review r1 of #205: a display that moved was reopened after the
    /// blocked pace's 500 ms. It is reopened at once; a second move right
    /// after waits at that pace instead of reopening in a loop.
    #[test]
    fn a_moved_display_is_reopened_at_once_but_not_in_a_loop() {
        use std::cell::Cell;
        let ms = Duration::from_millis;
        let shared = SharedControl::new(0, 1);
        // A paused clock: each attempt and each wait moves it.
        let start = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        // Before each attempt, how far the clock moves; every attempt but
        // the last asks to reopen.
        let steps = [ms(0), ms(999), ms(0), ms(1_000), ms(0)];
        let mut attempt = 0;
        let mut waits = Vec::new();
        let result = run_until_unblocked_on(
            &shared,
            |_| {
                elapsed.set(elapsed.get() + steps[attempt]);
                attempt += 1;
                if attempt < steps.len() {
                    Err(Reopen {
                        detail: "the display layout changed".into(),
                    }
                    .into())
                } else {
                    Ok(())
                }
            },
            |delay| {
                waits.push(delay);
                elapsed.set(elapsed.get() + delay);
                true
            },
            || start + elapsed.get(),
        );
        assert!(result.is_ok());
        assert_eq!(attempt, steps.len());
        // At 0 ms: at once. At 999 ms, under a second after it: the blocked
        // pace, 500 ms. At 1,499 ms, a second and more after the last one
        // opened at once: at once. At 2,499 ms, exactly a second after
        // that: at once.
        assert_eq!(waits, [ms(500)]);
        assert_eq!(elapsed.get(), ms(2_499));
        assert_eq!(
            *shared.capture_block.lock(),
            Some(CaptureBlock::DisplayChanging)
        );
        // Not a failure of the second screen.
        let reopen: Box<dyn std::error::Error + Send + Sync> = Box::new(Reopen {
            detail: String::new(),
        });
        assert!(passing_error(reopen.as_ref()));
    }

    #[test]
    fn retry_pace_resets_when_the_reason_changes() {
        let mut retry = BlockedRetry::default();
        let ms = |d: Duration| d.as_millis();
        let (d, changed) = retry.blocked(CaptureBlock::UnsupportedOnThisGpu);
        assert!(changed);
        assert_eq!(ms(d), 500);
        for _ in 0..5 {
            assert!(!retry.blocked(CaptureBlock::UnsupportedOnThisGpu).1);
        }
        assert_eq!(
            ms(retry.blocked(CaptureBlock::UnsupportedOnThisGpu).0),
            5000
        );
        let (d, changed) = retry.blocked(CaptureBlock::SecureDesktop);
        assert!(changed);
        assert_eq!(ms(d), 500);
    }
}
