//! Portable policy for capture interruptions Windows causes on purpose: the
//! lock screen, UAC prompts, display mode changes, exclusive full-screen apps.
//! None of these is a pipeline fault, so they must neither spin (the 0.3.1
//! ACCESS_LOST loop re-created the duplication every millisecond for hours)
//! nor count toward the supervisor's restart budget.

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
}

impl CaptureBlock {
    /// Maps a DXGI/Win32 HRESULT from DuplicateOutput or AcquireNextFrame.
    /// `None` means a real failure that the supervisor should handle.
    pub fn from_hresult(code: i32) -> Option<Self> {
        match code as u32 {
            0x8007_0005 => Some(Self::SecureDesktop), // E_ACCESSDENIED
            0x887A_0026 => Some(Self::DisplayChanging), // DXGI_ERROR_ACCESS_LOST
            0x887A_0004 => Some(Self::UnsupportedOnThisGpu), // DXGI_ERROR_UNSUPPORTED
            0x887A_0022 => Some(Self::TooManyCaptureApps), // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
            0x887A_0028 => Some(Self::SessionDisconnected), // DXGI_ERROR_SESSION_DISCONNECTED
            _ => None,
        }
    }
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

/// One loss episode lasts from the first ACCESS_LOST until a frame or an idle
/// timeout proves the new duplication works.
#[derive(Default)]
pub struct LossRecovery {
    since: Option<Instant>,
    losses: u32,
}

impl LossRecovery {
    pub fn in_episode(&self) -> bool {
        self.since.is_some()
    }

    pub fn lost(&mut self, now: Instant) -> LossStep {
        let since = *self.since.get_or_insert(now);
        self.losses += 1;
        if now.duration_since(since) >= LOSS_GIVE_UP {
            return LossStep::GiveUp;
        }
        let index = (self.losses as usize - 1).min(LOSS_DELAYS_MS.len() - 1);
        LossStep::Retry(Duration::from_millis(LOSS_DELAYS_MS[index]))
    }

    /// Ends the episode; returns how many losses it had and how long it took.
    pub fn healthy(&mut self, now: Instant) -> Option<(u32, Duration)> {
        let since = self.since.take()?;
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
    let mut frame_number = 0;
    loop {
        let error = match capture(&mut frame_number) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
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
    fn a_working_frame_ends_the_episode_and_resets_the_ladder() {
        let mut recovery = LossRecovery::default();
        let start = Instant::now();
        recovery.lost(start);
        recovery.lost(start);
        assert!(recovery.in_episode());
        let ended = recovery.healthy(start + Duration::from_millis(300));
        assert_eq!(ended, Some((2, Duration::from_millis(300))));
        assert!(!recovery.in_episode());
        assert_eq!(recovery.healthy(start), None);
        assert_eq!(recovery.lost(start), LossStep::Retry(Duration::ZERO));
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
