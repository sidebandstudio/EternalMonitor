//! The transport loop's heartbeat, and the watch that notices when it stops.
//!
//! The loop wakes at least every 250 ms (its liveness tick). A loop stuck in
//! one call answers nobody: its iPad loses the heartbeats and gives up, the
//! session is never ended for silence, and new HELLOs wait unread. That
//! happened in the field for close to a minute while Windows switched the
//! virtual display, and once, perhaps, for 18 minutes, with nothing in the
//! log to say where. The loop now marks each step it takes with a [`Phase`],
//! and the supervisor's thread logs one line when the mark goes stale, and
//! one when it moves again.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering::Relaxed};
use std::time::Duration;

use tracing::{info, warn};

/// Where the transport loop was when it last marked its heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// Waiting for its next timer, datagram, frame or audio packet.
    Waiting = 0,
    /// A heartbeat or liveness tick.
    Tick,
    /// Handling a control datagram from an iPad, or a USB tunnel event.
    Control,
    /// Injecting the iPad's touch, pen, mouse or keyboard input into Windows.
    Input,
    /// Sending what the session decided: replies, goodbyes, summaries.
    Actions,
    /// The second screen's next step.
    SecondScreen,
    /// Sending a video frame, with its pacing waits and the repairs served
    /// during them.
    Video,
    /// Sending PC audio.
    Audio,
    /// Sending the pointer to an iPad that draws it.
    Pointer,
}

impl Phase {
    const ALL: [Phase; 9] = [
        Phase::Waiting,
        Phase::Tick,
        Phase::Control,
        Phase::Input,
        Phase::Actions,
        Phase::SecondScreen,
        Phase::Video,
        Phase::Audio,
        Phase::Pointer,
    ];

    /// Stable log value.
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Waiting => "waiting",
            Phase::Tick => "tick",
            Phase::Control => "control",
            Phase::Input => "input",
            Phase::Actions => "actions",
            Phase::SecondScreen => "second_screen",
            Phase::Video => "video",
            Phase::Audio => "audio",
            Phase::Pointer => "pointer",
        }
    }
}

/// The last mark: when (ms on the process clock, 0 while no transport runs)
/// and in which phase.
#[derive(Debug, Default)]
pub struct Beat {
    at_ms: AtomicU64,
    phase: AtomicU8,
}

impl Beat {
    pub fn mark(&self, phase: Phase) {
        self.phase.store(phase as u8, Relaxed);
        self.at_ms
            .store((crate::clock::host_now_us() / 1000).max(1), Relaxed);
    }

    /// The transport loop ended.
    pub fn stop(&self) {
        self.at_ms.store(0, Relaxed);
    }

    /// The last mark, unless no transport runs.
    pub fn read(&self) -> Option<(u64, Phase)> {
        let at = self.at_ms.load(Relaxed);
        let phase = Phase::ALL
            .get(usize::from(self.phase.load(Relaxed)))
            .copied()
            .unwrap_or(Phase::Waiting);
        (at != 0).then_some((at, phase))
    }
}

/// A mark this old means the loop is stuck. An iPad gives up 3 s after the
/// last heartbeat it got, plus 5 s.
pub const STALL_AFTER: Duration = Duration::from_secs(2);

/// A gap this long between two checks means the watch itself did not run:
/// the PC slept, or the whole process was paused. The loop did not run
/// either, through no fault of its own, so the gap is not counted.
const PAUSED_AFTER: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The loop has not marked for `silent_ms`, stuck in `phase`.
    Stalled { phase: Phase, silent_ms: u64 },
    /// The loop marked again, or ended, after it was stuck for `stalled_ms`.
    Moving { phase: Phase, stalled_ms: u64 },
}

/// Judges the heartbeat at each check. Pure: the caller passes the clock.
#[derive(Debug, Default)]
pub struct Watch {
    checked_ms: u64,
    /// Counting starts again here after a pause.
    resumed_ms: u64,
    /// The stale mark the last stall was reported for.
    stalled: Option<(u64, Phase)>,
}

impl Watch {
    pub fn check(&mut self, beat: Option<(u64, Phase)>, now_ms: u64) -> Option<Verdict> {
        if self.checked_ms != 0
            && now_ms.saturating_sub(self.checked_ms) >= PAUSED_AFTER.as_millis() as u64
        {
            self.resumed_ms = now_ms;
        }
        self.checked_ms = now_ms;
        if let Some((at, phase)) = self.stalled {
            if beat.is_some_and(|(now_at, _)| now_at == at) {
                return None;
            }
            self.stalled = None;
            let moved = beat.map_or(now_ms, |(now_at, _)| now_at);
            return Some(Verdict::Moving {
                phase,
                stalled_ms: moved.saturating_sub(at),
            });
        }
        let (at, phase) = beat?;
        let silent_since = at.max(self.resumed_ms);
        if now_ms.saturating_sub(silent_since) < STALL_AFTER.as_millis() as u64 {
            return None;
        }
        self.stalled = Some((at, phase));
        Some(Verdict::Stalled {
            phase,
            silent_ms: now_ms.saturating_sub(at),
        })
    }

    /// [`Self::check`], logged.
    pub fn check_and_log(&mut self, beat: &Beat, now_ms: u64) {
        match self.check(beat.read(), now_ms) {
            Some(Verdict::Stalled { phase, silent_ms }) => warn!(
                stall = "transport_loop",
                phase = phase.name(),
                silent_ms,
                "The stream's transport loop stopped: iPads get no heartbeats or answers until it moves"
            ),
            Some(Verdict::Moving { phase, stalled_ms }) => info!(
                phase = phase.name(),
                stalled_ms,
                "The stream's transport loop moved again"
            ),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_that_keeps_marking_is_never_stalled() {
        let mut watch = Watch::default();
        for step in 0..100u64 {
            let now = 10_000 + step * 250;
            assert_eq!(watch.check(Some((now - 200, Phase::Waiting)), now), None);
        }
        // Nothing runs, nothing to judge.
        assert_eq!(watch.check(None, 40_000), None);
    }

    #[test]
    fn a_stuck_loop_is_reported_once_with_its_phase_then_when_it_moves() {
        let mut watch = Watch::default();
        let stuck = Some((10_000, Phase::Input));
        let mut verdicts = Vec::new();
        for now in (10_000..=20_000).step_by(250) {
            verdicts.extend(watch.check(stuck, now));
        }
        assert_eq!(
            verdicts,
            [Verdict::Stalled {
                phase: Phase::Input,
                silent_ms: 2_000
            }]
        );
        assert_eq!(
            watch.check(Some((20_100, Phase::Actions)), 20_250),
            Some(Verdict::Moving {
                phase: Phase::Input,
                stalled_ms: 10_100
            })
        );
        assert_eq!(watch.check(Some((20_300, Phase::Waiting)), 20_500), None);
    }

    #[test]
    fn a_stuck_loop_that_ends_is_reported_as_moving() {
        let mut watch = Watch::default();
        for now in (5_000..=8_000).step_by(250) {
            watch.check(Some((5_000, Phase::SecondScreen)), now);
        }
        assert_eq!(
            watch.check(None, 8_250),
            Some(Verdict::Moving {
                phase: Phase::SecondScreen,
                stalled_ms: 3_250
            })
        );
    }

    /// The PC slept: the loop's mark is minutes old when the watch runs
    /// again, but the loop was never given a chance to run.
    #[test]
    fn a_sleeping_pc_is_not_a_stall() {
        let mut watch = Watch::default();
        assert_eq!(watch.check(Some((9_900, Phase::Waiting)), 10_000), None);
        let woke = 10_000 + 600_000;
        assert_eq!(watch.check(Some((9_900, Phase::Waiting)), woke), None);
        // The loop marks again within its tick after the wake.
        assert_eq!(
            watch.check(Some((woke + 100, Phase::Tick)), woke + 250),
            None
        );
        // A loop that stays stuck after the wake is still caught.
        let mut stuck = Watch::default();
        stuck.check(Some((9_900, Phase::Video)), 10_000);
        let mut verdicts = Vec::new();
        for now in (woke..woke + 3_000).step_by(250) {
            verdicts.extend(stuck.check(Some((9_900, Phase::Video)), now));
        }
        assert_eq!(
            verdicts,
            [Verdict::Stalled {
                phase: Phase::Video,
                silent_ms: woke + 2_000 - 9_900
            }]
        );
    }

    #[test]
    fn the_heartbeat_reads_back_what_was_marked() {
        crate::clock::init();
        let beat = Beat::default();
        assert_eq!(beat.read(), None);
        beat.mark(Phase::Audio);
        let (at, phase) = beat.read().unwrap();
        assert!(at > 0);
        assert_eq!(phase, Phase::Audio);
        for phase in Phase::ALL {
            beat.mark(phase);
            assert_eq!(beat.read().unwrap().1, phase);
        }
        beat.stop();
        assert_eq!(beat.read(), None);
    }
}
