//! Adaptive bitrate controller: a pure rung-ladder policy evaluated on each
//! receiver report. The transport applies the returned target through the
//! encoder-reconfigure path; this module never touches sockets or encoders.

use std::time::{Duration, Instant};

use eternal_wire::v2::control::ReceiverReport;
use tracing::info;

/// Candidate bitrates, low to high. The active ladder is this intersected
/// with the user's ceiling (the GUI "Max bitrate" slider).
const LADDER_BPS: [u32; 11] = [
    4_000_000, 6_000_000, 8_000_000, 10_000_000, 12_000_000, 15_000_000, 20_000_000, 25_000_000,
    30_000_000, 40_000_000, 50_000_000,
];

const DOWN_COOLDOWN: Duration = Duration::from_secs(3);
const UP_COOLDOWN: Duration = Duration::from_secs(15);
/// The wait after any change before a step up, while the encoder takes a
/// new bitrate only by reopening (AMF, libx265): each step there costs a
/// stutter and an IDR. In 24 h of field logs from 59 Windows PCs, 75-80% of
/// hardware encoder opens followed an ABR step, and of 1,504 steps up, 919
/// were undone by a step down: half of those within 4 s and 788 within
/// 30 s, so each probe that failed reopened twice. One PC opened AMF 897
/// times in 7 h. A probe undone within [`PROBE_HOLDS`] doubles the wait, up
/// to [`REOPEN_UP_COOLDOWN_MAX`]; one that holds that long resets it.
const REOPEN_UP_COOLDOWN: Duration = Duration::from_secs(30);
const REOPEN_UP_COOLDOWN_MAX: Duration = Duration::from_secs(120);
/// A step up held this long, with no step down, worked.
const PROBE_HOLDS: Duration = Duration::from_secs(30);
/// This long continuously clean before stepping up.
const CLEAN_WINDOW: Duration = Duration::from_secs(15);
/// Freeze the controller when reports stop (liveness machinery owns that).
const REPORT_STALE_AFTER: Duration = Duration::from_secs(2);

/// Fragment-loss thresholds per report interval.
const LOSS_DOWN_ONE: f64 = 0.02;
const LOSS_DOWN_TWO: f64 = 0.10;
const LOSS_CLEAN: f64 = 0.005;

/// Over USB: step down when the tunnel dropped frames in this many of the
/// last three reports.
const TUNNEL_WINDOW_MASK: u8 = 0b111;
const TUNNEL_DROPS_TO_STEP: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbrDecision {
    pub target_bps: u32,
    pub changed: bool,
}

pub struct AbrController {
    enabled: bool,
    ceiling_bps: u32,
    current_bps: u32,
    last_change: Option<(Instant, bool)>, // (when, was_downgrade)
    clean_since: Option<Instant>,
    last_report_at: Option<Instant>,
    prev_cumulative: Option<ReceiverReport>,
    /// The USB tunnel's cumulative dropped frames at the previous report.
    prev_tunnel_drops: Option<u64>,
    /// Whether the tunnel dropped frames in each of the latest reports,
    /// newest in bit 0.
    tunnel_drop_history: u8,
    /// The encoder takes a new bitrate only by reopening (see
    /// [`Self::encoder_reopens`]).
    encoder_reopens: bool,
    /// The wait before a step up while it does.
    reopen_up_cooldown: Duration,
}

impl AbrController {
    /// Starts at min(ceiling, 15 Mbps) per the plan.
    pub fn new(ceiling_bps: u32, enabled: bool) -> Self {
        let start = clamp_to_ladder(
            if enabled {
                ceiling_bps.min(15_000_000)
            } else {
                ceiling_bps
            },
            ceiling_bps,
        );
        Self {
            enabled,
            ceiling_bps,
            current_bps: start,
            last_change: None,
            clean_since: None,
            last_report_at: None,
            prev_cumulative: None,
            prev_tunnel_drops: None,
            tunnel_drop_history: 0,
            encoder_reopens: false,
            reopen_up_cooldown: REOPEN_UP_COOLDOWN,
        }
    }

    /// Whether the open encoder session takes a new bitrate only by
    /// reopening it, as the transport reads it before each report. Then a
    /// step up waits [`REOPEN_UP_COOLDOWN`] or more after any change instead
    /// of [`UP_COOLDOWN`]. Steps down are not held: they relieve a link that
    /// is losing frames.
    pub fn encoder_reopens(&mut self, reopens: bool) {
        self.encoder_reopens = reopens;
    }

    /// The wait after a change before a step up.
    fn up_cooldown(&self) -> Duration {
        if self.encoder_reopens {
            self.reopen_up_cooldown
        } else {
            UP_COOLDOWN
        }
    }

    /// A step down at `now`. One soon after a step up undoes that probe:
    /// the next probe waits twice as long, while the encoder reopens.
    fn note_down(&mut self, now: Instant) {
        let undoes_probe = matches!(
            self.last_change,
            Some((when, false)) if now.duration_since(when) < PROBE_HOLDS
        );
        if undoes_probe && self.encoder_reopens {
            self.reopen_up_cooldown = (self.reopen_up_cooldown * 2).min(REOPEN_UP_COOLDOWN_MAX);
        }
        self.last_change = Some((now, true));
    }

    /// A step up that has held for [`PROBE_HOLDS`] worked: the next waits
    /// the shortest time again.
    fn note_probe_held(&mut self, now: Instant) {
        if matches!(
            self.last_change,
            Some((when, false)) if now.duration_since(when) >= PROBE_HOLDS
        ) {
            self.reopen_up_cooldown = REOPEN_UP_COOLDOWN;
        }
    }

    pub fn current_bps(&self) -> u32 {
        self.current_bps
    }

    /// The user moved the ceiling: re-clamp immediately.
    pub fn set_ceiling(&mut self, ceiling_bps: u32) -> AbrDecision {
        self.ceiling_bps = ceiling_bps;
        let clamped = clamp_to_ladder(
            if self.enabled {
                self.current_bps
            } else {
                ceiling_bps
            },
            ceiling_bps,
        );
        let changed = clamped != self.current_bps;
        self.current_bps = clamped;
        AbrDecision {
            target_bps: self.current_bps,
            changed,
        }
    }

    /// Evaluate one receiver report (cumulative counters — this diffs them).
    /// `tunnel_drops` is the USB tunnel's cumulative count of frames dropped
    /// because it fell behind, or `None` on WiFi.
    ///
    /// The cable loses nothing, so over USB the iPad's loss, repair and
    /// partial-frame figures say nothing about the link: a partial frame is
    /// one still arriving. 0.3.1 read them as network loss and cut a slow
    /// PC's bitrate two rungs at once. Over USB only a queue that dropped
    /// frames in at least two of the last three reports, or rising jitter,
    /// steps down, and one rung at a time. A single burst does not; a link
    /// that drops every other report does.
    pub fn on_report(
        &mut self,
        report: &ReceiverReport,
        tunnel_drops: Option<u64>,
        now: Instant,
    ) -> AbrDecision {
        let unchanged = AbrDecision {
            target_bps: self.current_bps,
            changed: false,
        };
        if !self.enabled {
            return unchanged;
        }
        self.note_probe_held(now);

        // Interval deltas from cumulative counters.
        let (delta_received, delta_lost, delta_repaired) = match self.prev_cumulative {
            Some(prev) if prev.stream_epoch == report.stream_epoch => (
                report.frags_received.saturating_sub(prev.frags_received),
                report.frags_lost.saturating_sub(prev.frags_lost),
                report.frags_repaired.saturating_sub(prev.frags_repaired),
            ),
            _ => {
                self.clean_since = None;
                (
                    report.frags_received,
                    report.frags_lost,
                    report.frags_repaired,
                )
            }
        };
        self.prev_cumulative = Some(*report);

        // A stale gap between reports invalidates the clean streak.
        if let Some(last) = self.last_report_at {
            if now.duration_since(last) > REPORT_STALE_AFTER {
                self.clean_since = None;
            }
        }
        self.last_report_at = Some(now);

        let total = u64::from(delta_received) + u64::from(delta_lost) + u64::from(delta_repaired);
        let jitter_high = report.jitter_us > 15_000;
        let (loss, repairs, congested_queue, tunnel_lossy) = match tunnel_drops {
            Some(drops) => {
                let dropped = self.prev_tunnel_drops.is_some_and(|prev| drops > prev);
                self.prev_tunnel_drops = Some(drops);
                self.tunnel_drop_history =
                    (self.tunnel_drop_history << 1 | u8::from(dropped)) & TUNNEL_WINDOW_MASK;
                let sustained = self.tunnel_drop_history.count_ones() >= TUNNEL_DROPS_TO_STEP;
                (0.0, 0.0, sustained, dropped)
            }
            None => {
                self.prev_tunnel_drops = None;
                self.tunnel_drop_history = 0;
                let loss = if total == 0 {
                    0.0
                } else {
                    f64::from(delta_lost) / total as f64
                };
                let repairs = f64::from(delta_repaired) / f64::from(delta_received.max(1));
                (loss, repairs, report.assembler_depth >= 3, false)
            }
        };

        // ---- Downgrade path ----
        let down_rungs = if loss > LOSS_DOWN_TWO {
            2
        } else if loss > LOSS_DOWN_ONE || repairs > 0.05 || congested_queue || jitter_high {
            1
        } else {
            0
        };
        if down_rungs > 0 {
            self.clean_since = None;
            let in_cooldown = matches!(
                self.last_change,
                Some((when, true)) if now.duration_since(when) < DOWN_COOLDOWN
            );
            if !in_cooldown {
                let target = step(self.current_bps, -(down_rungs), self.ceiling_bps);
                if target != self.current_bps {
                    info!(
                        from = self.current_bps,
                        to = target,
                        loss_pct = format!("{:.1}", loss * 100.0),
                        repair_pct = format!("{:.1}", repairs * 100.0),
                        jitter_us = report.jitter_us,
                        "ABR stepping bitrate down"
                    );
                    self.current_bps = target;
                    self.note_down(now);
                    return AbrDecision {
                        target_bps: target,
                        changed: true,
                    };
                }
            }
            return unchanged;
        }

        // ---- Upgrade path ----
        if total > 0
            && loss < LOSS_CLEAN
            && repairs < LOSS_CLEAN
            && !congested_queue
            && !tunnel_lossy
            && !jitter_high
        {
            let clean_since = *self.clean_since.get_or_insert(now);
            let up_cooldown_over = match self.last_change {
                Some((when, _)) => now.duration_since(when) >= self.up_cooldown(),
                None => true,
            };
            if now.duration_since(clean_since) >= CLEAN_WINDOW && up_cooldown_over {
                let target = step(self.current_bps, 1, self.ceiling_bps);
                if target != self.current_bps {
                    info!(
                        from = self.current_bps,
                        to = target,
                        "ABR stepping bitrate up"
                    );
                    self.current_bps = target;
                    self.last_change = Some((now, false));
                    self.clean_since = Some(now);
                    return AbrDecision {
                        target_bps: target,
                        changed: true,
                    };
                }
            }
        } else {
            self.clean_since = None;
        }

        unchanged
    }

    /// The system had no buffer space for a datagram: one rung down at
    /// once, without waiting for the iPad to report the loss. Like any
    /// downgrade it waits out the cooldown after the last one, and starts
    /// it, so refusals and loss reports together step at most once per
    /// [`DOWN_COOLDOWN`].
    pub fn on_send_refused(&mut self, now: Instant) -> AbrDecision {
        let unchanged = AbrDecision {
            target_bps: self.current_bps,
            changed: false,
        };
        if !self.enabled {
            return unchanged;
        }
        self.clean_since = None;
        if matches!(
            self.last_change,
            Some((when, true)) if now.duration_since(when) < DOWN_COOLDOWN
        ) {
            return unchanged;
        }
        let target = step(self.current_bps, -1, self.ceiling_bps);
        if target == self.current_bps {
            return unchanged;
        }
        info!(
            from = self.current_bps,
            to = target,
            reason = "no_buffer_space",
            "ABR stepping bitrate down"
        );
        self.current_bps = target;
        self.note_down(now);
        AbrDecision {
            target_bps: target,
            changed: true,
        }
    }
}

/// Nearest ladder rung at or below `bps`, capped by `ceiling`.
fn clamp_to_ladder(bps: u32, ceiling: u32) -> u32 {
    let cap = bps.min(ceiling);
    LADDER_BPS
        .iter()
        .rev()
        .copied()
        .find(|&rung| rung <= cap)
        .unwrap_or(LADDER_BPS[0])
}

/// Move `rungs` steps up (+) or down (−) the ladder from `from`, capped.
fn step(from: u32, rungs: i32, ceiling: u32) -> u32 {
    let current_index = LADDER_BPS
        .iter()
        .position(|&rung| rung >= from)
        .unwrap_or(LADDER_BPS.len() - 1);
    let target_index =
        (current_index as i32 + rungs).clamp(0, LADDER_BPS.len() as i32 - 1) as usize;
    LADDER_BPS[target_index].min(clamp_to_ladder(u32::MAX, ceiling))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(received: u32, lost: u32, depth: u8) -> ReceiverReport {
        ReceiverReport {
            frags_received: received,
            frags_lost: lost,
            assembler_depth: depth,
            ..Default::default()
        }
    }

    #[test]
    fn starts_at_min_of_ceiling_and_fifteen() {
        assert_eq!(
            AbrController::new(20_000_000, true).current_bps(),
            15_000_000
        );
        assert_eq!(AbrController::new(8_000_000, true).current_bps(), 8_000_000);
        assert_eq!(AbrController::new(5_000_000, true).current_bps(), 4_000_000);
    }

    #[test]
    fn heavy_loss_steps_down_two_rungs_with_cooldown() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        // 15% loss -> two rungs: 15M -> 10M.
        let d = abr.on_report(&report(850, 150, 0), None, t0);
        assert!(d.changed);
        assert_eq!(d.target_bps, 10_000_000);

        // Still lossy 1s later: inside the 3s down-cooldown, no change.
        let d = abr.on_report(&report(1700, 300, 0), None, t0 + Duration::from_secs(1));
        assert!(!d.changed);

        // After the cooldown, mild loss steps one more rung: 10M -> 8M.
        let d = abr.on_report(&report(2540, 340, 0), None, t0 + Duration::from_secs(4));
        assert!(d.changed);
        assert_eq!(d.target_bps, 8_000_000);
    }

    #[test]
    fn queue_depth_alone_steps_down() {
        let mut abr = AbrController::new(20_000_000, true);
        let d = abr.on_report(&report(1000, 0, 4), None, Instant::now());
        assert!(d.changed);
        assert_eq!(d.target_bps, 12_000_000);
    }

    #[test]
    fn clean_window_steps_up_after_cooldowns() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        // Step down first: 15M -> 12M.
        let d = abr.on_report(&report(950, 50, 0), None, t0);
        assert_eq!(d.target_bps, 12_000_000);

        // Clean reports every 500ms; must NOT step up before 15s clean.
        let mut cumulative = 1000;
        let mut last = AbrDecision {
            target_bps: 12_000_000,
            changed: false,
        };
        for i in 1..=40 {
            cumulative += 500;
            let at = t0 + Duration::from_millis(500 * i);
            last = abr.on_report(&report(cumulative, 50, 0), None, at);
            if last.changed {
                assert!(
                    at.duration_since(t0) >= Duration::from_secs(15),
                    "stepped up too early at {:?}",
                    at.duration_since(t0)
                );
                break;
            }
        }
        assert!(last.changed, "a sustained clean window must step up");
        assert_eq!(last.target_bps, 15_000_000);
    }

    #[test]
    fn report_gap_resets_clean_streak() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        abr.on_report(&report(950, 50, 0), None, t0); // down to 12M
                                                      // 10s of clean...
        let mut cumulative = 1000;
        for i in 1..=20 {
            cumulative += 500;
            abr.on_report(
                &report(cumulative, 50, 0),
                None,
                t0 + Duration::from_millis(500 * i),
            );
        }
        // ...then a 3s report outage, then clean again: the streak restarts,
        // so no upgrade until ~15s after the outage.
        let resume = t0 + Duration::from_secs(13);
        for i in 0..=8 {
            cumulative += 500;
            let d = abr.on_report(
                &report(cumulative, 50, 0),
                None,
                resume + Duration::from_secs(i),
            );
            assert!(!d.changed, "must not upgrade at +{i}s after an outage");
        }
    }

    #[test]
    fn ceiling_clamps_immediately_and_disabled_never_changes() {
        let mut abr = AbrController::new(20_000_000, true);
        let d = abr.set_ceiling(9_000_000);
        assert!(d.changed);
        assert_eq!(d.target_bps, 8_000_000);

        let mut off = AbrController::new(20_000_000, false);
        let d = off.on_report(&report(500, 500, 5), None, Instant::now());
        assert!(!d.changed, "disabled controller must never adapt");
    }

    #[test]
    fn floor_is_four_megabits() {
        let mut abr = AbrController::new(20_000_000, true);
        let mut now = Instant::now();
        let mut cumulative = (0u32, 0u32);
        for _ in 0..12 {
            cumulative = (cumulative.0 + 800, cumulative.1 + 200);
            abr.on_report(&report(cumulative.0, cumulative.1, 0), None, now);
            now += Duration::from_secs(4);
        }
        assert_eq!(abr.current_bps(), 4_000_000, "must bottom out at the floor");
    }

    #[test]
    fn repairs_and_jitter_step_down_before_unrecovered_loss() {
        let now = Instant::now();
        let mut abr = AbrController::new(50_000_000, true);
        let mut r = ReceiverReport {
            frags_received: 1000,
            frags_repaired: 51,
            ..Default::default()
        };
        assert_eq!(abr.on_report(&r, None, now).target_bps, 12_000_000);
        r.frags_received += 1000;
        r.frags_repaired += 100;
        assert!(
            !abr.on_report(&r, None, now + Duration::from_secs(1))
                .changed
        );
        r.frags_received += 1000;
        r.jitter_us = 15_001;
        assert_eq!(
            abr.on_report(&r, None, now + Duration::from_secs(3))
                .target_bps,
            10_000_000
        );
        r.frags_received += 850;
        r.frags_lost += 150;
        assert_eq!(
            abr.on_report(&r, None, now + Duration::from_secs(6))
                .target_bps,
            6_000_000
        );
    }

    #[test]
    fn extended_ladder_reaches_fifty_and_obeys_disabled_ceiling() {
        let mut abr = AbrController::new(50_000_000, true);
        let now = Instant::now();
        let mut r = ReceiverReport::default();
        let mut rungs = Vec::new();
        for tick in 0..=150 {
            r.frags_received += 1000;
            let d = abr.on_report(&r, None, now + Duration::from_millis(tick * 500));
            if d.changed {
                rungs.push(d.target_bps);
            }
        }
        assert_eq!(
            rungs,
            [20_000_000, 25_000_000, 30_000_000, 40_000_000, 50_000_000]
        );
        let mut fixed = AbrController::new(40_000_000, false);
        assert_eq!(fixed.current_bps(), 40_000_000);
        assert_eq!(fixed.set_ceiling(50_000_000).target_bps, 50_000_000);
    }

    #[test]
    fn repairs_prevent_clean_upgrade_and_epoch_reset_uses_new_counters() {
        let mut abr = AbrController::new(50_000_000, true);
        let now = Instant::now();
        let mut r = ReceiverReport {
            stream_epoch: 1,
            ..Default::default()
        };
        for tick in 0..=40 {
            r.frags_received += 1000;
            r.frags_repaired += 10;
            assert!(
                !abr.on_report(&r, None, now + Duration::from_millis(tick * 500))
                    .changed
            );
        }
        let restarted = ReceiverReport {
            stream_epoch: 2,
            frags_received: 800,
            frags_lost: 200,
            ..Default::default()
        };
        assert_eq!(
            abr.on_report(&restarted, None, now + Duration::from_secs(21))
                .target_bps,
            10_000_000
        );
    }

    /// Over USB a partial frame is one still arriving and nothing is lost on
    /// the cable. The iPad's loss figures must not cut the bitrate.
    #[test]
    fn usb_ignores_fragment_loss_and_partial_frames() {
        let mut abr = AbrController::new(20_000_000, true);
        let now = Instant::now();
        let mut r = ReceiverReport {
            frags_received: 600,
            frags_lost: 400,
            frags_repaired: 100,
            assembler_depth: 5,
            ..Default::default()
        };
        assert!(!abr.on_report(&r, Some(0), now).changed);
        r.frags_received += 600;
        r.frags_lost += 400;
        assert!(
            !abr.on_report(&r, Some(0), now + Duration::from_secs(4))
                .changed
        );
        assert_eq!(abr.current_bps(), 15_000_000);
        // The same report over WiFi is heavy loss.
        let mut wifi = AbrController::new(20_000_000, true);
        assert_eq!(wifi.on_report(&r, None, now).target_bps, 10_000_000);
    }

    /// A tunnel that drops frames in every other report is overloaded
    /// too. Requiring two reports in a row never stepped down: 1,200 frames
    /// dropped over 120 s left the stream at 15 Mbps.
    #[test]
    fn usb_steps_down_when_drops_alternate_with_clean_reports() {
        let mut abr = AbrController::new(20_000_000, true);
        let start = Instant::now();
        let mut r = ReceiverReport::default();
        let mut drops = 0;
        let mut rungs = Vec::new();
        for tick in 0..240u64 {
            r.frags_received += 500;
            if tick % 2 == 1 {
                drops += 10;
            }
            let d = abr.on_report(&r, Some(drops), start + Duration::from_millis(500 * tick));
            if d.changed {
                rungs.push((500 * tick, d.target_bps));
            }
        }
        assert_eq!(drops, 1_200);
        assert_eq!(
            rungs.first(),
            Some(&(1_500, 12_000_000)),
            "steps down within two seconds, one rung"
        );
        assert!(
            rungs.windows(2).all(|pair| pair[0].1 > pair[1].1),
            "never steps up while dropping: {rungs:?}"
        );
        assert_eq!(abr.current_bps(), 4_000_000);
    }

    /// A tunnel that keeps dropping frames is too slow for the stream: one
    /// rung down, but not for a single burst or bursts far apart.
    #[test]
    fn usb_steps_down_one_rung_only_when_the_tunnel_keeps_dropping() {
        let mut abr = AbrController::new(20_000_000, true);
        let start = Instant::now();
        let mut r = ReceiverReport::default();
        let mut decide = |drops: u64, ms: u64| {
            r.frags_received += 500;
            abr.on_report(&r, Some(drops), start + Duration::from_millis(ms))
        };
        assert!(!decide(0, 0).changed);
        assert!(!decide(3, 500).changed, "one burst is not a slow link");
        assert!(!decide(3, 1000).changed);
        assert!(!decide(3, 1500).changed);
        assert!(
            !decide(5, 2000).changed,
            "bursts three reports apart neither"
        );
        let down = decide(9, 2500);
        assert!(down.changed, "two of the last three reports dropped");
        assert_eq!(down.target_bps, 12_000_000, "one rung at a time");
        assert!(!decide(12, 3000).changed, "cooldown");
        // Clean reports without drops let it recover after the clean window.
        let mut ms = 3500;
        let mut stepped_up = false;
        while ms < 40_000 && !stepped_up {
            stepped_up = decide(12, ms).changed;
            ms += 500;
        }
        assert!(stepped_up);
        assert_eq!(abr.current_bps(), 15_000_000);
    }

    /// A refused send steps one rung down at once, and the loss reports of
    /// the datagrams it lost wait out the cooldown instead of stepping again.
    #[test]
    fn a_refused_send_steps_down_at_once_and_reports_wait_the_cooldown() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        let d = abr.on_send_refused(t0);
        assert_eq!((d.target_bps, d.changed), (12_000_000, true));
        let d = abr.on_report(&report(950, 50, 0), None, t0 + Duration::from_secs(1));
        assert!(!d.changed, "the same loss, inside the cooldown");
        assert_eq!(abr.current_bps(), 12_000_000);

        let d = abr.on_send_refused(t0 + Duration::from_secs(2));
        assert!(!d.changed, "a refusal waits out the cooldown too");
        assert_eq!(abr.current_bps(), 12_000_000);

        let mut floor = AbrController::new(4_000_000, true);
        assert!(!floor.on_send_refused(t0).changed);
        let mut disabled = AbrController::new(20_000_000, false);
        assert!(!disabled.on_send_refused(t0).changed);
        assert_eq!(disabled.current_bps(), 20_000_000);
    }

    /// Episodes of refused sends a little over a second apart each ask for
    /// a step, but steps come at most once per cooldown: not 15 Mbps down to
    /// the floor in five seconds.
    #[test]
    fn refusal_episodes_step_at_most_once_per_cooldown() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        let mut steps = Vec::new();
        for n in 0..5u64 {
            let at = Duration::from_millis(1_100 * n);
            if abr.on_send_refused(t0 + at).changed {
                steps.push((at.as_millis(), abr.current_bps()));
            }
        }
        assert_eq!(steps, [(0, 12_000_000), (3_300, 10_000_000)]);
    }

    /// A receiver report that just stepped down starts the cooldown: a
    /// refusal right after it doesn't step again for the same congestion.
    #[test]
    fn a_refusal_right_after_a_report_downgrade_waits_the_cooldown() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        let d = abr.on_report(&report(950, 50, 0), None, t0);
        assert_eq!((d.target_bps, d.changed), (12_000_000, true));
        assert!(!abr.on_send_refused(t0 + Duration::from_millis(500)).changed);
        assert!(
            !abr.on_send_refused(t0 + Duration::from_millis(2_900))
                .changed
        );
        let d = abr.on_send_refused(t0 + Duration::from_millis(3_100));
        assert_eq!((d.target_bps, d.changed), (10_000_000, true));
    }

    /// Feeds one clean report a second from `from` to `to` (in s from
    /// `t0`), with counters continuing from `counter`. The changes, as
    /// (second, target).
    fn clean_reports(
        abr: &mut AbrController,
        t0: Instant,
        counter: &mut u32,
        seconds: std::ops::Range<u64>,
    ) -> Vec<(u64, u32)> {
        let mut changes = Vec::new();
        for second in seconds {
            *counter += 1_000;
            let d = abr.on_report(
                &report(*counter, 0, 0),
                None,
                t0 + Duration::from_secs(second),
            );
            if d.changed {
                changes.push((second, d.target_bps));
            }
        }
        changes
    }

    #[test]
    fn a_step_up_waits_30_s_after_a_change_while_the_encoder_reopens() {
        // In place: 15 s clean from the first clean report, at 1 s; then a
        // rung per 15 s. Reopening: 30 s after the change, then a rung per
        // 30 s.
        for (reopens, first_up, wait) in [(false, 16, 15), (true, 30, 30)] {
            let mut abr = AbrController::new(50_000_000, true);
            abr.encoder_reopens(reopens);
            let t0 = Instant::now();
            // Loss steps 15 -> 12 Mbps at 0 s; clean from then on.
            assert!(abr.on_report(&report(980, 30, 0), None, t0).changed);
            let mut counter = 980;
            let changes = clean_reports(&mut abr, t0, &mut counter, 1..80);
            assert_eq!(changes[0], (first_up, 15_000_000), "reopens={reopens}");
            // Then one rung per wait, the clean window being shorter.
            let gaps: Vec<u64> = changes
                .windows(2)
                .map(|pair| pair[1].0 - pair[0].0)
                .collect();
            assert!(
                gaps.iter().all(|&gap| gap == wait),
                "reopens={reopens}: {changes:?}"
            );
        }
    }

    #[test]
    fn a_probe_undone_soon_doubles_the_wait_and_one_that_holds_resets_it() {
        let mut abr = AbrController::new(50_000_000, true);
        abr.encoder_reopens(true);
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut counter = 0;
        // Steps down at 0 s, then probes up once the wait is over, and each
        // probe is lost 4 s later, as half the failed probes in the field.
        assert!(abr.on_report(&report(980, 30, 0), None, at(0)).changed);
        let mut downs = vec![0u64];
        let mut ups = Vec::new();
        let mut second = 1;
        while second < 600 {
            let since_down = second - downs.last().unwrap();
            let lossy = ups
                .last()
                .is_some_and(|&up: &u64| up > *downs.last().unwrap() && second - up == 4);
            counter += 1_000;
            let lost = if lossy { 50 } else { 0 };
            let d = abr.on_report(&report(counter, lost, 0), None, at(second));
            if d.changed {
                if lossy {
                    downs.push(second);
                } else {
                    ups.push(second);
                }
            }
            let _ = since_down;
            second += 1;
        }
        let waits: Vec<u64> = ups
            .iter()
            .map(|up| up - downs.iter().rev().find(|down| *down < up).unwrap())
            .collect();
        assert_eq!(waits, [30, 60, 120, 120, 120, 120], "{ups:?} {downs:?}");
        // A probe that holds 30 s: the next one waits 30 s again.
        let mut abr = AbrController::new(50_000_000, true);
        abr.encoder_reopens(true);
        assert!(abr.on_report(&report(980, 30, 0), None, at(0)).changed);
        let mut counter = 980;
        let ups = clean_reports(&mut abr, t0, &mut counter, 1..31);
        assert_eq!(ups, [(30, 15_000_000)]);
        assert!(
            abr.on_report(&report(counter + 900, 60, 0), None, at(32))
                .changed
        );
        counter += 900;
        // Undone at 32 s: the wait doubles to 60 s.
        assert_eq!(clean_reports(&mut abr, t0, &mut counter, 33..95)[0].0, 92);
        // That probe holds; after 30 s the wait is 30 s again.
        let ups = clean_reports(&mut abr, t0, &mut counter, 95..160);
        assert_eq!(ups, [(122, 20_000_000), (152, 25_000_000)]);
    }

    #[test]
    fn steps_down_and_in_place_encoders_are_not_held() {
        // A reopening encoder steps down as before: at once, then 3 s apart.
        let mut abr = AbrController::new(50_000_000, true);
        abr.encoder_reopens(true);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let (mut counter, mut lost) = (0, 0);
        let mut downs = Vec::new();
        for ms in (0..10_000).step_by(1_000) {
            counter += 1_000;
            lost += 50;
            if abr
                .on_report(&report(counter, lost, 0), None, at(ms))
                .changed
            {
                downs.push((ms, abr.current_bps()));
            }
        }
        assert_eq!(
            downs,
            [
                (0, 12_000_000),
                (3_000, 10_000_000),
                (6_000, 8_000_000),
                (9_000, 6_000_000)
            ]
        );
        // A refused send too.
        let d = abr.on_send_refused(at(12_000));
        assert_eq!((d.target_bps, d.changed), (4_000_000, true));
        // An encoder that takes the bitrate in place keeps the 15 s wait:
        // 15 s clean from the first clean report.
        abr.encoder_reopens(false);
        let ups = clean_reports(&mut abr, at(12_000), &mut counter, 1..17);
        assert_eq!(ups, [(16, 6_000_000)]);
    }

    /// A link that carries `capacity` bps: each second, the fragments over
    /// it are lost. `capacity_at` gives it by second. The steps, by second.
    fn on_a_link(reopens: bool, seconds: u64, capacity_at: impl Fn(u64) -> u32) -> Vec<(u64, u32)> {
        let mut abr = AbrController::new(50_000_000, true);
        abr.encoder_reopens(reopens);
        let t0 = Instant::now();
        let (mut received, mut lost) = (0u32, 0u32);
        let mut steps = Vec::new();
        for second in 0..seconds {
            let rate = abr.current_bps();
            let capacity = capacity_at(second);
            // A fragment a kilobit: rate in kbit per second.
            let sent = rate / 1_000;
            let over = rate.saturating_sub(capacity) / 1_000;
            received += sent - over;
            lost += over;
            let d = abr.on_report(
                &report(received, lost, 0),
                None,
                t0 + Duration::from_secs(second),
            );
            if d.changed {
                steps.push((second, d.target_bps));
            }
        }
        steps
    }

    #[test]
    fn a_link_just_below_the_next_rung_probes_less_often_while_the_encoder_reopens() {
        // 11 Mbps: 10 Mbps holds, every probe to 12 Mbps is lost, as on the
        // field PC that opened AMF 897 times in 7 h.
        let minutes = 30;
        let steady = on_a_link(false, minutes * 60, |_| 11_000_000);
        let reopening = on_a_link(true, minutes * 60, |_| 11_000_000);
        let per_minute = |steps: &[(u64, u32)]| steps.len() as f64 / minutes as f64;
        eprintln!(
            "steps a minute at 11 Mbps: {:.2} in place, {:.2} reopening",
            per_minute(&steady),
            per_minute(&reopening)
        );
        // In place: a probe and its undoing every 15 s plus a few.
        assert!(per_minute(&steady) >= 6.0, "{steady:?}");
        // Reopening: about one probe and its undoing every 2 minutes once
        // the wait reaches its cap.
        assert!(per_minute(&reopening) <= 1.2, "{reopening:?}");
        // Both settle on 10 Mbps between probes and never stay above the
        // link.
        for steps in [&steady, &reopening] {
            assert!(steps.iter().all(|&(_, bps)| bps <= 12_000_000), "{steps:?}");
        }
        // Capacity that comes back: the reopening encoder still climbs, a
        // rung per 30 s.
        let recovered = on_a_link(
            true,
            400,
            |second| if second < 60 { 7_000_000 } else { 60_000_000 },
        );
        let after: Vec<_> = recovered
            .iter()
            .filter(|(second, _)| *second >= 60)
            .collect();
        assert_eq!(
            after.last().map(|(_, bps)| *bps),
            Some(50_000_000),
            "{recovered:?}"
        );
    }
}
