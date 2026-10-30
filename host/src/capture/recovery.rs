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
    /// Maps a DXGI/Win32 HRESULT from DuplicateOutput or AcquireNextFrame.
    /// `None` means a real failure that the supervisor should handle.
    pub fn from_hresult(code: i32) -> Option<Self> {
        match code as u32 {
            0x8007_0005 => Some(Self::SecureDesktop), // E_ACCESSDENIED
            0x887A_0026 => Some(Self::DisplayChanging), // DXGI_ERROR_ACCESS_LOST
            // DXGI_ERROR_NOT_FOUND: no display output, while a laptop's lid
            // is closed or its screen is off. It ended capture like a crash,
            // so the restart brake stopped it until the user clicked Start.
            0x887A_0002 => Some(Self::DisplayChanging),
            0x887A_0004 => Some(Self::UnsupportedOnThisGpu), // DXGI_ERROR_UNSUPPORTED
            0x887A_0022 => Some(Self::TooManyCaptureApps),   // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
            0x887A_0028 => Some(Self::SessionDisconnected),  // DXGI_ERROR_SESSION_DISCONNECTED
            _ => None,
        }
    }
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
pub const LOSS_LADDER_RESET: Duration = Duration::from_secs(5);

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
}

impl LossRecovery {
    pub fn in_episode(&self) -> bool {
        self.since.is_some()
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
    mut capture: impl FnMut(&mut u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
    mut wait: impl FnMut(Duration) -> bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut retry = BlockedRetry::default();
    let mut stalled = 0;
    let mut frame_number = 0;
    loop {
        let error = match capture(&mut frame_number) {
            Ok(()) => return Ok(()),
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
        let Some(unavailable) = error.downcast_ref::<CaptureUnavailable>() else {
            return Err(error);
        };
        *shared.capture_block.lock() = Some(unavailable.block);
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
    fn a_working_frame_ends_the_episode_and_five_healthy_seconds_reset_the_ladder() {
        let ms = Duration::from_millis;
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        recovery.lost(start);
        recovery.lost(start);
        assert!(recovery.in_episode());
        let ended = recovery.healthy(start + ms(300));
        assert_eq!(ended, Some((2, ms(300))));
        assert!(!recovery.in_episode());
        // Frames keep coming for 5 s: the next loss is retried at once.
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
    /// follow within 5 s keep climbing the ladder instead of starting over
    /// at 0 ms each time.
    #[test]
    fn a_duplication_lost_again_soon_after_it_returned_keeps_its_backoff() {
        let ms = Duration::from_millis;
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        let mut now = start;
        let mut delays = Vec::new();
        for _ in 0..7 {
            let LossStep::Retry(delay) = recovery.lost(now) else {
                panic!("gave up")
            };
            delays.push(delay.as_millis() as u64);
            // The new duplication works for a second, then is lost again.
            now += delay + ms(1);
            assert!(
                recovery.healthy(now).is_some(),
                "each loss is its own episode"
            );
            now += ms(1_000);
            assert_eq!(recovery.healthy(now), None);
        }
        assert_eq!(delays, [0, 100, 250, 500, 1000, 2000, 2000]);
        // The last return was a second ago. Four healthy seconds are not
        // enough; five are.
        let back = now - ms(1_000);
        assert_eq!(recovery.healthy(back + ms(4_000)), None);
        assert_eq!(recovery.lost(back + ms(4_000)), LossStep::Retry(ms(2000)));
        recovery.healthy(back + ms(6_000));
        assert_eq!(recovery.healthy(back + ms(11_000)), None);
        assert_eq!(
            recovery.lost(back + ms(11_000)),
            LossStep::Retry(Duration::ZERO)
        );
        // Each return still ends its episode: the give-up window measures
        // one outage, not the whole loop.
        assert!(now.duration_since(start) > LOSS_GIVE_UP);
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
            CaptureBlock::from_hresult(not_found),
            Some(CaptureBlock::DisplayChanging)
        );
        // Not remembered against the second screen either.
        assert!(passing_code(not_found));
        let lid_closed_for = Duration::from_secs(120);

        let shared = SharedControl::new(0, 1);
        let waited = RefCell::new(Duration::ZERO);
        let waits = RefCell::new(Vec::new());
        let warnings = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Warnings(warnings.clone()));
        let result = tracing::subscriber::with_default(subscriber, || {
            run_until_unblocked(
                &shared,
                |_| {
                    if *waited.borrow() < lid_closed_for {
                        let block = CaptureBlock::from_hresult(not_found).unwrap();
                        return Err(Box::new(CaptureUnavailable {
                            block,
                            detail: "EnumOutputs: 0x887a0002".into(),
                        }));
                    }
                    note_capture_resumed(&shared);
                    Ok(())
                },
                |delay| {
                    assert_eq!(
                        *shared.capture_block.lock(),
                        Some(CaptureBlock::DisplayChanging)
                    );
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
