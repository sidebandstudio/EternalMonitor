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
/// stutter and an IDR. On a link just below the next rung, a step up is
/// undone by a step down within seconds, so a probe that fails reopens the
/// encoder twice. A probe undone within [`PROBE_HOLDS`] doubles the wait,
/// up to [`REOPEN_UP_COOLDOWN_MAX`]; one that holds that long resets it.
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
const JITTER_DOWN_US: u32 = 15_000;
/// Allow this much path variation above the sender peak in a clean report.
const JITTER_CLEAN_MARGIN_US: u32 = 5_000;
/// Real write waits must add up within one report to count as congestion.
const SOCKET_WAIT_MIN: Duration = Duration::from_millis(5);

/// Give frames in flight one more report interval to arrive.
const NO_PROGRESS_GRACE: Duration = Duration::from_millis(500);

/// Over USB: step down when the tunnel dropped frames in this many of the
/// last three reports.
const TUNNEL_WINDOW_MASK: u8 = 0b111;
const TUNNEL_DROPS_TO_STEP: u32 = 2;

/// Over WiFi, a report with no frames is a still screen's when the PC
/// sent at most this many since the previous report. The PC counts a
/// frame as it sends it and the iPad as it arrives, so one sent within a
/// round trip of the report shows up only in the next; a still screen
/// sends about one a second, so a lone frame on its way is that, not a
/// link losing frames.
const STILL_FRAMES_SENT: u32 = 1;

#[derive(Default)]
struct SendJitter {
    epoch: Option<u32>,
    seq: u32,
    previous_transit_us: Option<u64>,
    jitter_us: f64,
    peak_us: u32,
    frame_wait_us: u64,
    fragments_in_frame: u16,
    complete_since_report: u32,
    socket_wait: Duration,
}

impl SendJitter {
    fn record(
        &mut self,
        epoch: u32,
        seq: u32,
        capture_us: u64,
        send_us: u64,
        count: u16,
        waited: Duration,
    ) {
        if self.epoch != Some(epoch) || seq < self.seq {
            *self = Self {
                epoch: Some(epoch),
                ..Self::default()
            };
        }
        if self.seq != seq {
            self.seq = seq;
            self.frame_wait_us = 0;
            self.fragments_in_frame = 0;
        }

        self.socket_wait += waited;
        self.frame_wait_us += waited.as_micros() as u64;
        let transit_us = send_us.saturating_sub(capture_us + self.frame_wait_us);
        if let Some(previous) = self.previous_transit_us.replace(transit_us) {
            let delta_us = transit_us.abs_diff(previous) as f64;
            self.jitter_us += (delta_us - self.jitter_us) / 16.0;
        }
        self.peak_us = self.peak_us.max(self.jitter_us as u32);

        self.fragments_in_frame += 1;
        if self.fragments_in_frame == count {
            self.complete_since_report += 1;
        }
    }

    fn take_peak(&mut self) -> u32 {
        // The filter keeps its value at the start of the next interval.
        std::mem::replace(&mut self.peak_us, self.jitter_us as u32)
    }
}

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
    send_jitter: SendJitter,
    no_progress_since: Option<Instant>,
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
    /// The latest step up is a probe a step down undoes: no session on
    /// another link started since.
    probe_pending: bool,
    /// Whether the current session is over USB, once one started.
    session_usb: Option<bool>,
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
            send_jitter: SendJitter::default(),
            no_progress_since: None,
            prev_tunnel_drops: None,
            tunnel_drop_history: 0,
            encoder_reopens: false,
            reopen_up_cooldown: REOPEN_UP_COOLDOWN,
            probe_pending: false,
            session_usb: None,
        }
    }

    pub(super) fn with_stream_epoch(mut self, epoch: u32) -> Self {
        self.send_jitter.epoch = Some(epoch);
        self
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
        let undoes_probe = self.probe_pending
            && matches!(
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

    /// Feed each original video datagram as it leaves WiFi pacing. The
    /// iPad uses the same RFC 3550 filter and skips repairs. Remove socket
    /// waits from this fragment and the remaining fragments of its burst.
    pub(crate) fn note_udp_fragment_sent(
        &mut self,
        epoch: u32,
        seq: u32,
        capture_us: u64,
        send_us: u64,
        frag_count: u16,
        waited: Duration,
    ) {
        if self.enabled {
            self.send_jitter
                .record(epoch, seq, capture_us, send_us, frag_count, waited);
        }
    }

    pub(super) fn note_socket_wait(&mut self, waited: Duration) {
        self.send_jitter.socket_wait += waited;
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
    /// frames in at least two of the last three reports steps down, and one
    /// rung at a time. A single burst does not; a link that drops every
    /// other report does. Jitter does not either: the iPad measures it from
    /// the capture time, so over the cable it is the PC's encode time
    /// varying, which a lower bitrate does not fix.
    ///
    /// On WiFi, more than 15 ms of jitter beyond the sender peak is enough
    /// to step down. The sender peak is per report interval, not per packet.
    /// Reordering inflates reported jitter. Network jitter that coincides
    /// with sender jitter needs corroboration from loss, repairs or socket
    /// waits in the interval. A high report can count as clean if it is
    /// within 5 ms of the sender peak and there is no corroboration.
    ///
    /// `frames_sent` is the video frames the PC sent since the previous
    /// report. A report with no frames neither starts nor breaks the clean
    /// streak when the PC sent next to none either (a still screen sends
    /// about one a second, [`STILL_FRAMES_SENT`]). Over WiFi, when it sent
    /// more, they were lost, all of each frame, which the iPad does not
    /// count: the streak starts again. Over USB a frame is lost only where
    /// the tunnel drops it, so a report without frames is neutral unless the
    /// tunnel dropped some: a slow encode followed by a fast one can leave
    /// two frames queued behind a stalled writer when the iPad reports.
    pub fn on_report(
        &mut self,
        report: &ReceiverReport,
        tunnel_drops: Option<u64>,
        frames_sent: u32,
        now: Instant,
    ) -> AbrDecision {
        let unchanged = AbrDecision {
            target_bps: self.current_bps,
            changed: false,
        };
        if !self.enabled {
            return unchanged;
        }

        let wifi = tunnel_drops.is_none();
        // A report from another epoch cannot close this sender interval.
        if wifi
            && self
                .send_jitter
                .epoch
                .is_some_and(|epoch| report.stream_epoch != epoch)
        {
            return unchanged;
        }
        self.note_probe_held(now);

        // The iPad resets its counters on an epoch change or sequence wrap.
        let prev = self.prev_cumulative.filter(|prev| {
            prev.stream_epoch == report.stream_epoch
                && !(wifi && prev.highest_seq.saturating_sub(report.highest_seq) > u32::MAX / 2)
        });
        if prev.is_none() {
            self.clean_since = None;
        }
        let previous = prev.unwrap_or_default();
        let delta_received = report
            .frags_received
            .saturating_sub(previous.frags_received);
        let delta_lost = report.frags_lost.saturating_sub(previous.frags_lost);
        let delta_repaired = report
            .frags_repaired
            .saturating_sub(previous.frags_repaired);
        self.prev_cumulative = Some(*report);

        let stale = self
            .last_report_at
            .is_none_or(|last| now.duration_since(last) > REPORT_STALE_AFTER);
        if stale {
            self.clean_since = None;
        }
        self.last_report_at = Some(now);

        let send_jitter_us = self.send_jitter.take_peak();
        let sent = std::mem::take(&mut self.send_jitter.complete_since_report);
        let socket_waited =
            std::mem::take(&mut self.send_jitter.socket_wait) >= SOCKET_WAIT_MIN && wifi;
        let stalled = wifi
            && !stale
            && prev.is_some()
            && report.highest_seq == previous.highest_seq
            && delta_received == 0
            && sent > STILL_FRAMES_SENT;
        let blackout = if stalled {
            now.duration_since(*self.no_progress_since.get_or_insert(now)) >= NO_PROGRESS_GRACE
        } else {
            self.no_progress_since = None;
            false
        };

        let total = u64::from(delta_received) + u64::from(delta_lost) + u64::from(delta_repaired);
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

        let corroborated = loss >= LOSS_CLEAN || repairs >= LOSS_CLEAN || socket_waited;
        // Without new originals, the report repeats the iPad's old jitter.
        let jitter_reported = wifi && delta_received > 0 && report.jitter_us > JITTER_DOWN_US;
        let excess_us = report.jitter_us.saturating_sub(send_jitter_us);
        let jitter_high = jitter_reported && (excess_us > JITTER_DOWN_US || corroborated);
        let jitter_unexplained = jitter_reported && excess_us > JITTER_CLEAN_MARGIN_US;

        // ---- Downgrade path ----
        let down_rungs = if loss > LOSS_DOWN_TWO {
            2
        } else if loss > LOSS_DOWN_ONE
            || repairs > 0.05
            || congested_queue
            || jitter_high
            || blackout
        {
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
                        send_jitter_us,
                        socket_waited,
                        blackout,
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
        let none_lost = match tunnel_drops {
            Some(_) => !tunnel_lossy,
            None => frames_sent <= STILL_FRAMES_SENT,
        };
        if total == 0 && none_lost && !stalled {
            return unchanged;
        }
        if total > 0
            && loss < LOSS_CLEAN
            && repairs < LOSS_CLEAN
            && !congested_queue
            && !tunnel_lossy
            && !jitter_unexplained
            && !socket_waited
            && !stalled
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
                    self.probe_pending = true;
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

    /// A new client session started. Its reports count from its own start,
    /// and the old session's clean streak and tunnel drops say nothing about
    /// it: they start again, as at a new stream epoch. `tunnel_drops` is the
    /// USB tunnel's cumulative drops as it starts, or `None` on WiFi: its
    /// first report counts the drops since then, not since that report.
    ///
    /// The bitrate and the wait after the last change stay. On another link
    /// than the last session's, the probe backoff starts again too: what
    /// failed on WiFi says nothing about the cable, and a step down there
    /// does not undo a probe made on WiFi. On the same link it stays, or a
    /// link that keeps reconnecting would probe every 30 s again.
    pub fn new_session(&mut self, tunnel_drops: Option<u64>) {
        self.prev_cumulative = None;
        self.send_jitter = SendJitter {
            epoch: self.send_jitter.epoch,
            ..SendJitter::default()
        };
        self.no_progress_since = None;
        self.clean_since = None;
        self.prev_tunnel_drops = tunnel_drops;
        self.tunnel_drop_history = 0;
        let usb = tunnel_drops.is_some();
        if self.session_usb.replace(usb).is_some_and(|was| was != usb) {
            self.reopen_up_cooldown = REOPEN_UP_COOLDOWN;
            self.probe_pending = false;
        }
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
    use eternal_wire::reassembly::Reassembler;
    use eternal_wire::v2::media::MediaHeader;

    /// Frames the PC sent between two reports: a moving picture's, more
    /// than a still screen's.
    const SENT: u32 = 30;

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
        let d = abr.on_report(&report(850, 150, 0), None, SENT, t0);
        assert!(d.changed);
        assert_eq!(d.target_bps, 10_000_000);

        // Still lossy 1s later: inside the 3s down-cooldown, no change.
        let d = abr.on_report(
            &report(1700, 300, 0),
            None,
            SENT,
            t0 + Duration::from_secs(1),
        );
        assert!(!d.changed);

        // After the cooldown, mild loss steps one more rung: 10M -> 8M.
        let d = abr.on_report(
            &report(2540, 340, 0),
            None,
            SENT,
            t0 + Duration::from_secs(4),
        );
        assert!(d.changed);
        assert_eq!(d.target_bps, 8_000_000);
    }

    #[test]
    fn queue_depth_alone_steps_down() {
        let mut abr = AbrController::new(20_000_000, true);
        let d = abr.on_report(&report(1000, 0, 4), None, SENT, Instant::now());
        assert!(d.changed);
        assert_eq!(d.target_bps, 12_000_000);
    }

    #[test]
    fn clean_window_steps_up_after_cooldowns() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        // Step down first: 15M -> 12M.
        let d = abr.on_report(&report(950, 50, 0), None, SENT, t0);
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
            last = abr.on_report(&report(cumulative, 50, 0), None, SENT, at);
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
        abr.on_report(&report(950, 50, 0), None, SENT, t0); // down to 12M
                                                            // 10s of clean...
        let mut cumulative = 1000;
        for i in 1..=20 {
            cumulative += 500;
            abr.on_report(
                &report(cumulative, 50, 0),
                None,
                SENT,
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
                SENT,
                resume + Duration::from_secs(i),
            );
            assert!(!d.changed, "must not upgrade at +{i}s after an outage");
        }
    }

    #[test]
    fn repeated_stale_report_gaps_never_earn_clean_time() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        for tick in 0..=10 {
            let at = t0 + Duration::from_secs(u64::from(tick) * 5);
            let r = report(500 * (tick + 1), 0, 0);
            assert!(!abr.on_report(&r, None, SENT, at).changed);
        }
    }

    #[test]
    fn ceiling_clamps_immediately_and_disabled_never_changes() {
        let mut abr = AbrController::new(20_000_000, true);
        let d = abr.set_ceiling(9_000_000);
        assert!(d.changed);
        assert_eq!(d.target_bps, 8_000_000);

        let mut off = AbrController::new(20_000_000, false);
        let d = off.on_report(&report(500, 500, 5), None, SENT, Instant::now());
        assert!(!d.changed, "disabled controller must never adapt");
    }

    #[test]
    fn floor_is_four_megabits() {
        let mut abr = AbrController::new(20_000_000, true);
        let mut now = Instant::now();
        let mut cumulative = (0u32, 0u32);
        for _ in 0..12 {
            cumulative = (cumulative.0 + 800, cumulative.1 + 200);
            abr.on_report(&report(cumulative.0, cumulative.1, 0), None, SENT, now);
            now += Duration::from_secs(4);
        }
        assert_eq!(abr.current_bps(), 4_000_000, "must bottom out at the floor");
    }

    #[test]
    fn repairs_step_down_before_unrecovered_loss() {
        let now = Instant::now();
        let mut abr = AbrController::new(50_000_000, true);
        let mut r = ReceiverReport {
            frags_received: 1000,
            frags_repaired: 51,
            ..Default::default()
        };
        assert_eq!(abr.on_report(&r, None, SENT, now).target_bps, 12_000_000);
        r.frags_received += 1000;
        r.frags_repaired += 100;
        assert!(
            !abr.on_report(&r, None, SENT, now + Duration::from_secs(1))
                .changed
        );
        r.frags_received += 850;
        r.frags_lost += 150;
        assert_eq!(
            abr.on_report(&r, None, SENT, now + Duration::from_secs(3))
                .target_bps,
            8_000_000
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
            let d = abr.on_report(&r, None, SENT, now + Duration::from_millis(tick * 500));
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
                !abr.on_report(&r, None, SENT, now + Duration::from_millis(tick * 500))
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
            abr.on_report(&restarted, None, SENT, now + Duration::from_secs(21))
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
        assert!(!abr.on_report(&r, Some(0), SENT, now).changed);
        r.frags_received += 600;
        r.frags_lost += 400;
        assert!(
            !abr.on_report(&r, Some(0), SENT, now + Duration::from_secs(4))
                .changed
        );
        assert_eq!(abr.current_bps(), 15_000_000);
        // The same report over WiFi is heavy loss.
        let mut wifi = AbrController::new(20_000_000, true);
        assert_eq!(wifi.on_report(&r, None, SENT, now).target_bps, 10_000_000);
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
            let d = abr.on_report(
                &r,
                Some(drops),
                SENT,
                start + Duration::from_millis(500 * tick),
            );
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
            abr.on_report(&r, Some(drops), SENT, start + Duration::from_millis(ms))
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
        let d = abr.on_report(&report(950, 50, 0), None, SENT, t0 + Duration::from_secs(1));
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
        let d = abr.on_report(&report(950, 50, 0), None, SENT, t0);
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
        let lost = abr.prev_cumulative.map_or(0, |prev| prev.frags_lost);
        for second in seconds {
            *counter += 1_000;
            let d = abr.on_report(
                &report(*counter, lost, 0),
                None,
                SENT,
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
            assert!(abr.on_report(&report(980, 30, 0), None, SENT, t0).changed);
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
        let mut counter = 980;
        let mut lost = 30;
        // Steps down at 0 s, then probes up once the wait is over, and each
        // probe is lost 4 s later, on a link that cannot carry it.
        assert!(
            abr.on_report(&report(980, 30, 0), None, SENT, at(0))
                .changed
        );
        let mut downs = vec![0u64];
        let mut ups = Vec::new();
        let mut second = 1;
        while second < 600 {
            let since_down = second - downs.last().unwrap();
            let lossy = ups
                .last()
                .is_some_and(|&up: &u64| up > *downs.last().unwrap() && second - up == 4);
            counter += 1_000;
            if lossy {
                lost += 50;
            }
            let d = abr.on_report(&report(counter, lost, 0), None, SENT, at(second));
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
        assert!(
            abr.on_report(&report(980, 30, 0), None, SENT, at(0))
                .changed
        );
        let mut counter = 980;
        let ups = clean_reports(&mut abr, t0, &mut counter, 1..31);
        assert_eq!(ups, [(30, 15_000_000)]);
        assert!(
            abr.on_report(&report(counter + 900, 60, 0), None, SENT, at(32))
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
                .on_report(&report(counter, lost, 0), None, SENT, at(ms))
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
                SENT,
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
        // 11 Mbps: 10 Mbps holds, every probe to 12 Mbps is lost.
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

    /// Reports every 500 ms from `t0` over `ticks`, counters continuing
    /// from `r`, with `jitter_us`; `frames` gives each tick's frames the PC
    /// sent and whether any arrived. The link is USB when `usb`. The
    /// changes, as (ms, target).
    fn half_second_reports(
        abr: &mut AbrController,
        t0: Instant,
        r: &mut ReceiverReport,
        usb: bool,
        ticks: std::ops::Range<u64>,
        frames: impl Fn(u64) -> (u32, bool),
        jitter_us: impl Fn(u64) -> u32,
    ) -> Vec<(u64, u32)> {
        let mut changes = Vec::new();
        for tick in ticks {
            let (sent, arrived) = frames(tick);
            if arrived {
                r.frags_received += 500;
            }
            r.jitter_us = jitter_us(tick);
            let d = abr.on_report(
                r,
                usb.then_some(0),
                sent,
                t0 + Duration::from_millis(500 * tick),
            );
            if d.changed {
                changes.push((500 * tick, d.target_bps));
            }
        }
        changes
    }

    /// Jitter between 15 and 110 ms, as a QSV encode swinging from 10 to
    /// 400 ms gave a field session over the cable.
    fn encode_jitter(tick: u64) -> u32 {
        15_001 + (tick as u32 * 37_000) % 95_000
    }

    /// Over USB the iPad's jitter is the PC's encode time varying. With no
    /// dropped frames it neither steps down nor holds back a step up.
    #[test]
    fn usb_jitter_alone_neither_steps_down_nor_blocks_a_step_up() {
        let mut abr = AbrController::new(20_000_000, true);
        let mut r = ReceiverReport::default();
        let changes = half_second_reports(
            &mut abr,
            Instant::now(),
            &mut r,
            true,
            0..60,
            |_| (SENT, true),
            encode_jitter,
        );
        assert_eq!(changes, [(15_000, 20_000_000)]);
    }

    struct WifiFrame {
        capture_us: u64,
        encode_us: u64,
        path_us: u64,
        wait_us: u64,
        fragments: u16,
    }

    struct WifiPacket {
        header: MediaHeader,
        sent_us: u64,
        arrival_us: u64,
        wait: Duration,
    }

    fn wifi_packets(frames: impl Iterator<Item = WifiFrame>, until_us: u64) -> Vec<WifiPacket> {
        let mut packets = Vec::new();
        let mut last_send_us = 0;
        for (index, frame) in frames.enumerate() {
            let seq = index as u32 + 1;
            let first_send_us = (frame.capture_us + frame.encode_us).max(last_send_us + 100);
            if first_send_us > until_us {
                break;
            }
            for frag_index in 0..frame.fragments {
                last_send_us = first_send_us + frame.wait_us + u64::from(frag_index) * 50;
                let header = MediaHeader {
                    session_id: 1,
                    stream_epoch: 1,
                    frame_seq: seq,
                    frag_index,
                    frag_count: frame.fragments,
                    is_keyframe: seq == 1 || frame.fragments > 1,
                    is_retransmit: false,
                    capture_ts_us: frame.capture_us,
                    payload_len: 1,
                };
                let wait = Duration::from_micros(if frag_index == 0 { frame.wait_us } else { 0 });
                packets.push(WifiPacket {
                    header,
                    sent_us: last_send_us,
                    arrival_us: last_send_us + frame.path_us,
                    wait,
                });
            }
        }

        packets
    }

    /// Take sender and receiver snapshots at the same report time. A report
    /// can cut through a burst or leave a sent fragment in flight.
    fn wifi_jitter_reports(
        packets: &[WifiPacket],
        first_report_us: u64,
        ticks: u64,
        reported_loss: u32,
    ) -> Vec<(u64, u32)> {
        let mut abr = AbrController::new(20_000_000, true);
        let mut ipad = Reassembler::new();
        let t0 = Instant::now();
        let mut sends = packets.iter().peekable();
        let mut arrivals = packets.iter().peekable();
        let mut changes = Vec::new();
        for tick in 0..ticks {
            let report_us = first_report_us + 500_000 * tick;
            let mut frames_sent = 0;
            while sends
                .peek()
                .is_some_and(|packet| packet.sent_us <= report_us)
            {
                let &WifiPacket {
                    header,
                    sent_us,
                    wait,
                    ..
                } = sends.next().unwrap();
                abr.note_udp_fragment_sent(
                    1,
                    header.frame_seq,
                    header.capture_ts_us,
                    sent_us,
                    header.frag_count,
                    wait,
                );
                frames_sent += u32::from(header.frag_index + 1 == header.frag_count);
            }
            while arrivals
                .peek()
                .is_some_and(|packet| packet.arrival_us <= report_us)
            {
                let &WifiPacket {
                    header, arrival_us, ..
                } = arrivals.next().unwrap();
                ipad.add_media(header, &[0], t0 + Duration::from_micros(arrival_us));
            }
            let counters = ipad.counters();
            assert_eq!(counters.frags_lost, 0);
            // Vary the reported loss with the timing samples held fixed.
            let report = ReceiverReport {
                stream_epoch: counters.stream_epoch,
                highest_seq: counters.highest_seq,
                frags_received: counters.frags_received as u32 - reported_loss,
                frags_lost: reported_loss,
                jitter_us: counters.jitter_us,
                assembler_depth: counters.assembler_depth,
                ..Default::default()
            };
            let d = abr.on_report(
                &report,
                None,
                frames_sent,
                t0 + Duration::from_micros(report_us),
            );
            if d.changed {
                changes.push((500 * tick, d.target_bps));
            }
        }
        changes
    }

    #[test]
    fn clean_wifi_with_an_uneven_encoder_reaches_its_ceiling_at_every_report_phase() {
        for (fps, slow_us) in [(30, 65_000), (30, 67_500), (60, 65_000), (120, 65_000)] {
            for keyframes in [false, true] {
                let frames = (0..).map(|n| WifiFrame {
                    capture_us: n * 1_000_000 / fps,
                    encode_us: if n % 2 == 0 { slow_us } else { 36_000 },
                    path_us: 20_000,
                    wait_us: 0,
                    fragments: if keyframes && n % (2 * fps) == 0 {
                        24
                    } else {
                        1
                    },
                });
                let packets = wifi_packets(frames, 17_000_000);
                // Between these phase boundaries, all 32 reports see the
                // same set of sends and arrivals, even inside a keyframe.
                let mut phases = vec![0];
                phases.extend(
                    packets
                        .iter()
                        .flat_map(|packet| [packet.sent_us, packet.arrival_us])
                        .map(|time| time % 500_000),
                );
                phases.sort_unstable();
                phases.dedup();
                for phase in phases {
                    assert_eq!(
                        wifi_jitter_reports(&packets, 1_000_000 + phase, 32, 0),
                        [(15_000, 20_000_000)],
                        "{fps} fps, slow encode {slow_us} us, phase {phase} us, keyframes {keyframes}",
                    );
                }
            }
        }
    }

    #[test]
    fn clean_wifi_with_an_uneven_encoder_and_path_variation_reaches_its_ceiling() {
        for random_path in [false, true] {
            let mut seed = 42u64;
            let frames = (0..).map(|n| {
                let variation_us = if random_path {
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    1_000 + (seed >> 32) % 4_001
                } else {
                    5_000 - n % 2 * 4_000
                };
                WifiFrame {
                    capture_us: n * 1_000_000 / 30,
                    encode_us: if n % 2 == 0 { 65_000 } else { 36_000 },
                    path_us: 20_000 + variation_us,
                    wait_us: 0,
                    fragments: 1,
                }
            });
            let packets = wifi_packets(frames, 17_000_000);
            for phase_us in [0, 123_000, 499_000] {
                assert_eq!(
                    wifi_jitter_reports(&packets, 1_000_000 + phase_us, 32, 0),
                    [(15_000, 20_000_000)],
                    "random path: {random_path}, phase: {phase_us} us",
                );
            }
        }
    }

    #[test]
    fn wifi_clean_jitter_margin_is_inclusive() {
        let t0 = Instant::now();
        for (excess_us, clean) in [(5_000, true), (5_001, false)] {
            let mut abr = AbrController::new(20_000_000, true);
            abr.on_report(&report(1, 0, 0), None, 1, t0);
            abr.note_udp_fragment_sent(0, 2, 0, 0, 1, Duration::ZERO);
            abr.note_udp_fragment_sent(0, 3, 0, 240_000, 1, Duration::ZERO);
            assert_eq!(abr.send_jitter.peak_us, 15_000);
            let r = ReceiverReport {
                jitter_us: 15_000 + excess_us,
                ..report(3, 0, 0)
            };
            assert!(!abr.on_report(&r, None, 2, t0 + NO_PROGRESS_GRACE).changed);
            assert_eq!(abr.clean_since, clean.then_some(t0), "{excess_us} us");
        }
    }

    #[test]
    fn network_jitter_on_wifi_steps_down_one_rung_per_cooldown() {
        let frames = (0..).map(|n| WifiFrame {
            capture_us: n * 42_000,
            encode_us: 36_000,
            path_us: 10_000 + n % 2 * 30_000,
            wait_us: 0,
            fragments: 1,
        });
        assert_eq!(
            wifi_jitter_reports(&wifi_packets(frames, 10_500_000), 1_000_000, 20, 0),
            [
                (0, 12_000_000),
                (3_000, 10_000_000),
                (6_000, 8_000_000),
                (9_000, 6_000_000)
            ]
        );
    }

    fn uneven_wifi_frames(wait_us: impl Fn(u64) -> u64) -> impl Iterator<Item = WifiFrame> {
        (0..).map(move |n| WifiFrame {
            capture_us: n * 70_000,
            encode_us: [
                36_000, 65_000, 36_000, 65_000, 65_000, 36_000, 65_000, 36_000,
            ][(n % 8) as usize],
            path_us: 10_000 + n % 2 * 30_000,
            wait_us: wait_us(n),
            fragments: 1,
        })
    }

    #[test]
    fn uneven_encoding_and_path_delay_step_down_only_with_corroboration() {
        let packets = wifi_packets(uneven_wifi_frames(|_| 0), 21_500_000);
        assert!(wifi_jitter_reports(&packets, 1_000_000, 42, 0).is_empty());
        assert_eq!(
            wifi_jitter_reports(
                &wifi_packets(
                    uneven_wifi_frames(|n| if n == 0 { 5_000 } else { 0 }),
                    21_500_000
                ),
                1_000_000,
                42,
                0
            ),
            [(0, 12_000_000)]
        );
        // Exactly 200 fragments arrive by this report. Report one of them
        // as lost to exercise the 0.5% threshold with the same jitter.
        assert_eq!(
            wifi_jitter_reports(&packets, 14_020_000, 1, 1),
            [(0, 12_000_000)]
        );
    }

    #[test]
    fn isolated_one_millisecond_waits_do_not_corroborate_jitter() {
        let frames = uneven_wifi_frames(|n| {
            if n * 70_000 % 3_000_000 < 70_000 {
                1_000
            } else {
                0
            }
        });
        assert!(
            wifi_jitter_reports(&wifi_packets(frames, 21_500_000), 1_000_000, 42, 0).is_empty()
        );
    }

    #[test]
    fn fragment_loss_or_repair_corroborates_jitter_at_half_a_percent() {
        for repairs in [false, true] {
            for (total, events, step_down) in [(240, 1, false), (200, 1, true), (240, 2, true)] {
                let mut abr = AbrController::new(20_000_000, true);
                abr.note_udp_fragment_sent(0, 1, 0, 0, 1, Duration::ZERO);
                abr.note_udp_fragment_sent(0, 2, 0, 16, 1, Duration::ZERO);
                let r = ReceiverReport {
                    jitter_us: 15_001,
                    frags_received: if repairs { total } else { total - events },
                    frags_lost: if repairs { 0 } else { events },
                    frags_repaired: if repairs { events } else { 0 },
                    ..Default::default()
                };
                assert_eq!(
                    abr.on_report(&r, None, SENT, Instant::now()).changed,
                    step_down,
                    "repairs={repairs}, events={events}, total={total}",
                );
            }
        }
    }

    #[test]
    fn each_report_takes_the_sender_peak_and_preserves_the_filter() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        abr.note_udp_fragment_sent(1, 1, 0, 36_000, 1, Duration::ZERO);
        abr.note_udp_fragment_sent(1, 2, 0, 68_000, 2, Duration::ZERO);
        abr.note_udp_fragment_sent(1, 2, 0, 68_000, 2, Duration::ZERO);
        assert_eq!(abr.send_jitter.peak_us, 2_000);
        assert_eq!(abr.send_jitter.jitter_us, 1_875.0);
        let r = ReceiverReport {
            stream_epoch: 1,
            frags_received: 3,
            jitter_us: 17_000,
            ..Default::default()
        };
        assert!(!abr.on_report(&r, None, 2, t0).changed);
        assert_eq!(abr.send_jitter.peak_us, 1_875);

        abr.note_udp_fragment_sent(2, 1, 0, 36_000, 1, Duration::ZERO);
        assert_eq!(abr.send_jitter.peak_us, 0);
        abr.note_udp_fragment_sent(2, 2, 0, 68_000, 1, Duration::ZERO);
        assert_eq!(abr.send_jitter.peak_us, 2_000);
        abr.new_session(None);
        assert_eq!(abr.send_jitter.peak_us, 0);
        assert!(abr.send_jitter.previous_transit_us.is_none());
    }

    #[test]
    fn wifi_reports_from_the_previous_epoch_leave_sender_samples_and_clean_streak_alone() {
        let mut abr = AbrController::new(20_000_000, true).with_stream_epoch(2);
        abr.new_session(None);
        let t0 = Instant::now();
        let old = ReceiverReport {
            stream_epoch: 1,
            frags_received: 100,
            jitter_us: 28_000,
            ..Default::default()
        };
        assert!(!abr.on_report(&old, None, 0, t0).changed);
        assert!(abr.prev_cumulative.is_none());
        assert!(abr.clean_since.is_none());

        abr.note_udp_fragment_sent(2, 1, 0, 36_000, 1, Duration::ZERO);
        assert!(!abr.on_report(&old, None, 1, t0 + NO_PROGRESS_GRACE).changed);
        assert_eq!(abr.send_jitter.complete_since_report, 1);

        let current = ReceiverReport {
            stream_epoch: 2,
            frags_received: 1,
            ..Default::default()
        };
        let clean_start = t0 + Duration::from_secs(1);
        assert!(!abr.on_report(&current, None, 1, clean_start).changed);
        assert_eq!(abr.clean_since, Some(clean_start));

        abr.note_udp_fragment_sent(2, 2, 0, 68_000, 1, Duration::ZERO);
        abr.note_udp_fragment_sent(2, 3, 0, 68_000, 1, Duration::ZERO);
        let delayed = clean_start + NO_PROGRESS_GRACE;
        assert!(!abr.on_report(&old, None, 2, delayed).changed);
        assert_eq!(abr.send_jitter.peak_us, 2_000);
        assert_eq!(abr.send_jitter.complete_since_report, 2);
        assert_eq!(abr.prev_cumulative, Some(current));
        assert_eq!(abr.last_report_at, Some(clean_start));
        assert_eq!(abr.clean_since, Some(clean_start));
    }

    #[test]
    fn send_jitter_and_report_counts_reset_at_sequence_wrap_in_the_same_epoch() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        abr.note_udp_fragment_sent(1, u32::MAX - 1, 0, 36_000, 1, Duration::ZERO);
        abr.note_udp_fragment_sent(1, u32::MAX, 0, 68_000, 1, SOCKET_WAIT_MIN);
        assert!(abr.send_jitter.peak_us > 1_000);
        let mut r = ReceiverReport {
            stream_epoch: 1,
            highest_seq: u32::MAX,
            frags_received: 2,
            ..Default::default()
        };
        abr.on_report(&r, None, 2, t0);
        abr.note_udp_fragment_sent(1, 0, 0, 65_000, 1, Duration::ZERO);
        assert_eq!(abr.send_jitter.peak_us, 0);
        assert_eq!(abr.send_jitter.complete_since_report, 1);
        r.highest_seq = 0;
        r.frags_received = 1;
        r.jitter_us = 16_000;
        assert_eq!(
            abr.on_report(&r, None, 1, t0 + NO_PROGRESS_GRACE)
                .target_bps,
            12_000_000
        );
    }

    #[test]
    fn alternating_socket_waits_are_congestion_and_do_not_inflate_sender_jitter() {
        let mut abr = AbrController::new(20_000_000, true);
        for seq in 1..=120 {
            let wait = Duration::from_millis(30 * u64::from(seq % 2));
            abr.note_udp_fragment_sent(1, seq, 0, 36_000 + wait.as_micros() as u64, 1, wait);
        }
        assert_eq!(abr.send_jitter.peak_us, 0);
        let r = ReceiverReport {
            stream_epoch: 1,
            highest_seq: 120,
            frags_received: 119,
            jitter_us: 29_985,
            ..Default::default()
        };
        assert_eq!(
            abr.on_report(&r, None, SENT, Instant::now()).target_bps,
            12_000_000
        );
    }

    #[test]
    fn wifi_jitter_does_not_step_down_again_when_no_new_fragments_arrive() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        abr.note_udp_fragment_sent(1, 1, 0, 36_000, 1, Duration::ZERO);
        let r = ReceiverReport {
            stream_epoch: 1,
            highest_seq: 1,
            frags_received: 1,
            jitter_us: 30_000,
            ..Default::default()
        };
        assert_eq!(abr.on_report(&r, None, 1, t0).target_bps, 12_000_000);
        assert!(!abr.on_report(&r, None, 0, t0 + DOWN_COOLDOWN).changed);
    }

    #[test]
    fn blackout_steps_down_after_grace_only_when_wifi_frames_were_sent() {
        let t0 = Instant::now();
        for (sent, frames_sent, drops) in [
            (SENT, SENT, None),
            (0, 0, None),
            (0, SENT, None),
            (SENT, SENT, Some(0)),
        ] {
            let mut abr = AbrController::new(20_000_000, true);
            abr.note_udp_fragment_sent(1, 1, 0, 36_000, 1, Duration::ZERO);
            let r = ReceiverReport {
                stream_epoch: 1,
                highest_seq: 1,
                frags_received: 1,
                jitter_us: 30_000,
                ..Default::default()
            };
            assert_eq!(abr.on_report(&r, drops, 1, t0).changed, drops.is_none());
            let mut seq = 1;
            let mut changes = Vec::new();
            // The report outage must discard the earlier no-progress
            // interval. The first fresh report starts a new grace period.
            for tick in [1, 2, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
                for _ in 0..sent {
                    seq += 1;
                    abr.note_udp_fragment_sent(1, seq, 0, 36_000, 1, Duration::ZERO);
                }
                let at = t0 + Duration::from_millis(500 * tick);
                let d = abr.on_report(&r, drops, frames_sent, at);
                if d.changed {
                    changes.push((tick, d.target_bps));
                }
            }
            if sent > 0 && drops.is_none() {
                assert_eq!(changes, [(9, 10_000_000), (15, 8_000_000)]);
            } else {
                assert!(changes.is_empty());
            }
        }
    }

    /// A still screen sends about a frame a second, so every other report
    /// has none, and the PC sent none either. Those neither break the clean
    /// streak, which kept a field session at 4 Mbps for hours, nor start it.
    #[test]
    fn reports_without_frames_neither_break_nor_start_the_clean_streak() {
        // Loss steps 15 -> 12 Mbps at 0 ms, then frames on odd ticks only:
        // clean from 500 ms, so up at 15.5 s and again 15 s later.
        let t0 = Instant::now();
        let mut wifi = AbrController::new(20_000_000, true);
        let mut r = report(950, 50, 0);
        assert!(wifi.on_report(&r, None, SENT, t0).changed);
        let odd = |tick: u64| (u32::from(tick % 2 == 1), tick % 2 == 1);
        let changes = half_second_reports(&mut wifi, t0, &mut r, false, 1..80, odd, |_| 0);
        assert_eq!(changes, [(15_500, 15_000_000), (30_500, 20_000_000)]);

        let mut usb = AbrController::new(20_000_000, true);
        let mut r = ReceiverReport::default();
        let even = |tick: u64| (u32::from(tick.is_multiple_of(2)), tick.is_multiple_of(2));
        let changes = half_second_reports(&mut usb, t0, &mut r, true, 0..60, even, |_| 0);
        assert_eq!(changes, [(15_000, 20_000_000)]);

        // 20 s with no frames at all, then frames: up 15 s after the first.
        let mut idle = AbrController::new(20_000_000, true);
        let mut r = ReceiverReport::default();
        let after_20_s = |tick: u64| if tick >= 40 { (SENT, true) } else { (0, false) };
        let changes = half_second_reports(&mut idle, t0, &mut r, false, 0..80, after_20_s, |_| 0);
        assert_eq!(changes, [(35_000, 20_000_000)]);
    }

    /// A frame sent just before a report reaches the iPad after it: the
    /// next report counts it. The one without it is still a still screen's.
    #[test]
    fn a_frame_on_its_way_does_not_break_the_clean_streak() {
        // Sent on even ticks, arrived by odd ones: clean from 500 ms.
        let in_flight = |tick: u64| (u32::from(tick.is_multiple_of(2)), tick % 2 == 1);
        for usb in [false, true] {
            let mut abr = AbrController::new(20_000_000, true);
            let mut r = ReceiverReport::default();
            let changes = half_second_reports(
                &mut abr,
                Instant::now(),
                &mut r,
                usb,
                0..60,
                in_flight,
                |_| 0,
            );
            assert_eq!(changes, [(15_500, 20_000_000)], "usb: {usb}");
        }
    }

    #[test]
    fn wifi_empty_report_with_one_frame_in_flight_and_a_socket_wait_is_neutral() {
        let t0 = Instant::now();
        let mut abr = AbrController::new(20_000_000, true);
        let mut r = ReceiverReport::default();
        for tick in 0..=30 {
            let waited = if tick == 14 {
                SOCKET_WAIT_MIN
            } else {
                Duration::ZERO
            };
            let seq = tick + 1;
            abr.note_udp_fragment_sent(0, seq, 0, 36_000 + waited.as_micros() as u64, 1, waited);
            if waited.is_zero() {
                r.frags_received = seq;
                r.highest_seq = seq;
            }
            let d = abr.on_report(
                &r,
                None,
                1,
                t0 + Duration::from_millis(500 * u64::from(tick)),
            );
            assert_eq!(d.changed, tick == 30, "tick {tick}");
        }
        assert_eq!(abr.current_bps(), 20_000_000);
    }

    /// Over WiFi the PC kept sending for 3 s and nothing arrived, while the
    /// iPad's reports still did. It counts no loss for frames it saw none
    /// of, so reports without frames are the blackout's: the streak starts
    /// again. (Over USB the tunnel counts what it drops.)
    #[test]
    fn a_blackout_while_frames_are_sent_breaks_the_clean_streak() {
        // Clean from 0, nothing arrives from 10 s to 13 s: up 15 s after.
        let blackout = |tick: u64| (SENT, !(20..26).contains(&tick));
        let mut abr = AbrController::new(20_000_000, true);
        let mut r = ReceiverReport::default();
        let changes = half_second_reports(
            &mut abr,
            Instant::now(),
            &mut r,
            false,
            0..60,
            blackout,
            |_| 0,
        );
        assert_eq!(changes, [(28_000, 20_000_000)]);
    }

    /// Over USB a frame is lost only where the tunnel drops it. A 400 ms
    /// encode then a 10 ms one can leave two frames queued behind a stalled
    /// writer when the iPad reports: with no tunnel drops that report is
    /// neutral, and the step up comes at 15 s, not 20.5 s. Over WiFi the
    /// same two frames went missing: the streak starts again.
    #[test]
    fn usb_reports_without_frames_are_neutral_while_the_tunnel_drops_none() {
        // A frame each tick; at 5 s two sent and none arrived yet.
        let queued = |tick: u64| if tick == 10 { (2, false) } else { (1, true) };
        for (usb, up_at) in [(true, 15_000), (false, 20_500)] {
            let mut abr = AbrController::new(20_000_000, true);
            let mut r = ReceiverReport::default();
            let changes =
                half_second_reports(&mut abr, Instant::now(), &mut r, usb, 0..60, queued, |_| 0);
            assert_eq!(changes, [(up_at, 20_000_000)], "usb: {usb}");
        }
    }

    /// A new session's reports count from its start. A quick reconnect in
    /// the same stream epoch reported lower counters: the deltas came to
    /// nothing, that empty report kept the old session's clean streak, and
    /// the new session stepped up on its second report. The streak starts
    /// again with the new session; the bitrate stays.
    #[test]
    fn a_new_session_starts_the_clean_streak_again_and_keeps_the_bitrate() {
        for usb in [false, true] {
            let t0 = Instant::now();
            let mut abr = AbrController::new(20_000_000, true);
            assert!(abr.on_send_refused(t0).changed);
            // Clean from 500 ms to 14 s.
            let mut r = ReceiverReport::default();
            let all = |_| (SENT, true);
            let old = half_second_reports(&mut abr, t0, &mut r, usb, 1..29, all, |_| 0);
            assert!(old.is_empty());
            abr.new_session(usb.then_some(0));
            assert_eq!(abr.current_bps(), 12_000_000, "usb: {usb}");
            // The new session's first report has none of its frames yet.
            let mut r = ReceiverReport::default();
            let first_empty = |tick| if tick == 29 { (1, false) } else { (SENT, true) };
            let new = half_second_reports(&mut abr, t0, &mut r, usb, 29..80, first_empty, |_| 0);
            assert_eq!(new, [(30_000, 15_000_000)], "usb: {usb}");
        }
    }

    /// Over USB a report without frames, but with a frame the tunnel
    /// dropped, breaks the streak: up 15 s after it, not 15 s after the
    /// first report.
    #[test]
    fn a_tunnel_drop_in_a_report_without_frames_breaks_the_clean_streak() {
        let t0 = Instant::now();
        let mut abr = AbrController::new(20_000_000, true);
        let mut r = ReceiverReport::default();
        let mut changes = Vec::new();
        for tick in 0..60 {
            // At 5 s the one frame sent was the tunnel's one drop.
            let dropped = tick == 10;
            if !dropped {
                r.frags_received += 500;
            }
            let drops = u64::from(tick >= 10);
            let at = t0 + Duration::from_millis(500 * tick);
            let d = abr.on_report(&r, Some(drops), u32::from(dropped), at);
            if d.changed {
                changes.push((500 * tick, d.target_bps));
            }
        }
        assert_eq!(changes, [(20_500, 20_000_000)]);
    }

    /// Feeds one report a second over USB from `from` to `to` (in s from
    /// `t0`), counters continuing from `counter`, the tunnel's cumulative
    /// drops by second from `drops`. The changes, as (second, target).
    fn usb_reports(
        abr: &mut AbrController,
        t0: Instant,
        counter: &mut u32,
        seconds: std::ops::Range<u64>,
        drops: impl Fn(u64) -> u64,
    ) -> Vec<(u64, u32)> {
        let mut changes = Vec::new();
        for second in seconds {
            *counter += 1_000;
            let at = t0 + Duration::from_secs(second);
            let d = abr.on_report(&report(*counter, 0, 0), Some(drops(second)), SENT, at);
            if d.changed {
                changes.push((second, d.target_bps));
            }
        }
        changes
    }

    /// A probe failed on WiFi doubles the wait to 60 s. A session on the
    /// cable starts it again at 30 s after the last change; one on WiFi
    /// again keeps it.
    #[test]
    fn a_session_on_another_link_starts_the_probe_backoff_again() {
        for (usb, up_at) in [(true, 62), (false, 92)] {
            let mut abr = AbrController::new(50_000_000, true);
            abr.encoder_reopens(true);
            abr.new_session(None);
            let t0 = Instant::now();
            let at = |s: u64| t0 + Duration::from_secs(s);
            // Down at 0 s, a probe up at 30 s, undone at 32 s.
            assert!(
                abr.on_report(&report(980, 30, 0), None, SENT, at(0))
                    .changed
            );
            let mut counter = 980;
            let ups = clean_reports(&mut abr, t0, &mut counter, 1..31);
            assert_eq!(ups, [(30, 15_000_000)]);
            counter += 900;
            assert!(
                abr.on_report(&report(counter, 60, 0), None, SENT, at(32))
                    .changed
            );
            abr.new_session(usb.then_some(0));
            let ups = if usb {
                usb_reports(&mut abr, t0, &mut counter, 33..100, |_| 0)
            } else {
                clean_reports(&mut abr, t0, &mut counter, 33..100)
            };
            assert_eq!(ups[0], (up_at, 15_000_000), "usb: {usb}");
        }
    }

    /// A probe made on WiFi, then a session on the cable that steps down
    /// within 30 s of it: that step down did not undo the probe, so the
    /// next waits 30 s, not 60.
    #[test]
    fn a_step_down_on_another_link_does_not_undo_the_probe() {
        let mut abr = AbrController::new(50_000_000, true);
        abr.encoder_reopens(true);
        abr.new_session(None);
        let t0 = Instant::now();
        assert!(abr.on_report(&report(980, 30, 0), None, SENT, t0).changed);
        let mut counter = 980;
        let ups = clean_reports(&mut abr, t0, &mut counter, 1..31);
        assert_eq!(ups, [(30, 15_000_000)]);
        abr.new_session(Some(0));
        // The tunnel drops frames at 32 s and 33 s: down at 33 s.
        let drops = |second: u64| match second {
            ..32 => 0,
            32 => 5,
            _ => 10,
        };
        let changes = usb_reports(&mut abr, t0, &mut counter, 31..90, drops);
        assert_eq!(changes, [(33, 12_000_000), (63, 15_000_000)]);
    }

    /// The tunnel drops frames between the session starting and the iPad's
    /// first report, and again before the next: two of its first two
    /// reports dropped frames, so it steps down. Counted from the first
    /// report instead, only the second did.
    #[test]
    fn a_new_usb_session_counts_tunnel_drops_from_its_start() {
        let mut abr = AbrController::new(20_000_000, true);
        let t0 = Instant::now();
        abr.new_session(Some(100));
        let mut counter = 0;
        let drops = |second: u64| if second == 0 { 105 } else { 110 };
        let changes = usb_reports(&mut abr, t0, &mut counter, 0..3, drops);
        assert_eq!(changes, [(1, 12_000_000)]);
    }
}
