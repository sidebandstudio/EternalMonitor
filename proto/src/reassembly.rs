//! Fragment reassembly that mirrors the iPad's `FrameAssembler.swift`
//! semantics exactly — epoch precedence, duplicate/stale rejection, the
//! seq-backward-jump restart heuristic, negotiated ordered repair windows, and bounded partial-frame storage.
//!
//! The host never reassembles in production; this exists so the fake receiver
//! in the end-to-end tests (and any future host-side receiver) exercises the
//! same rules the real client applies, and so those rules are unit-testable
//! on every platform. If behavior here diverges from the Swift assembler,
//! the Swift side is the specification.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::v2::control::Nack;
use crate::v2::media::{MediaHeader, MAX_FRAG_COUNT};

pub const STREAM_RESTART_GAP: u32 = 256;
pub const STALE_FRAME_TIMEOUT: Duration = Duration::from_millis(100);
pub const EPOCH_RESYNC_THRESHOLD: u32 = 512;
/// Also bounds frames held behind an incomplete one in repair mode; the
/// repair deadline limits the wait (see `FrameAssembler.maxPendingFrames`).
pub const MAX_PENDING_FRAMES: usize = 8;
/// How far a stall can move one frame's repair deadline (see
/// `FrameAssembler.maxStallAllowanceUs`).
pub const MAX_STALL_ALLOWANCE: Duration = Duration::from_millis(100);
pub const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    StaleEpoch,
    DuplicateCompleted,
    StaleSeq,
    ZeroCount,
    IndexOutOfRange,
    CountMismatch,
    DuplicateFragment,
    CountExceedsCap,
    Capacity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    Stored,
    Completed(Vec<u8>),
    Dropped(DropReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledFrame {
    pub seq: u32,
    pub capture_ts_us: u64,
    pub is_keyframe: bool,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
struct PendingFrame {
    header: MediaHeader,
    fragments: HashMap<u16, Vec<u8>>,
    bytes: usize,
    created_at: Instant,
    contiguous: u16,
    highest_index: u16,
    repaired: u64,
    gap_at: Option<Instant>,
    deadline: Option<Instant>,
    retry_at: Option<Instant>,
    requested_at: Option<Instant>,
    completed_at: Option<Instant>,
    stall_used: Duration,
    stall_counted_until: Option<Instant>,
    /// Indices already requested; each is requested when first seen missing
    /// and at most once more by the frame's single retry.
    requested: HashSet<u16>,
    retried: bool,
    keyframe_requested: bool,
}

impl PendingFrame {
    fn complete(&self) -> bool {
        self.fragments.len() == usize::from(self.header.frag_count)
    }
    fn missing_fragments(&self) -> u64 {
        u64::from(self.header.frag_count).saturating_sub(self.fragments.len() as u64)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblyCounters {
    pub frames_complete: u64,
    pub frames_dropped: u64,
    pub frags_received: u64,
    pub frags_lost: u64,
    pub frags_repaired: u64,
    pub nacks_sent: u64,
    pub highest_seq: u32,
    pub stream_epoch: u32,
    pub assembler_depth: u8,
    pub jitter_us: u32,
}

#[derive(Debug)]
pub struct Reassembler {
    pending: BTreeMap<u32, PendingFrame>,
    pending_bytes: usize,
    latest_completed_seq: u32,
    retired_seq: u32,
    current_epoch: Option<u32>,
    stale_epoch_streak: u32,
    counters: ReassemblyCounters,
    repair_enabled: bool,
    frame_period: Duration,
    rtt: Duration,
    ready: VecDeque<AssembledFrame>,
    nacks: VecDeque<Nack>,
    keyframe_needed: bool,
    clock_origin: Option<Instant>,
    previous_transit: Option<f64>,
    jitter: f64,
    last_arrival: Option<Instant>,
    last_repair: Option<Instant>,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
            pending_bytes: 0,
            latest_completed_seq: 0,
            retired_seq: 0,
            current_epoch: None,
            stale_epoch_streak: 0,
            counters: ReassemblyCounters::default(),
            repair_enabled: false,
            frame_period: Duration::from_micros(16_667),
            rtt: Duration::from_millis(10),
            ready: VecDeque::new(),
            nacks: VecDeque::new(),
            keyframe_needed: false,
            clock_origin: None,
            previous_transit: None,
            jitter: 0.0,
            last_arrival: None,
            last_repair: None,
        }
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn counters(&self) -> ReassemblyCounters {
        self.counters
    }
    pub fn pending_frames(&self) -> usize {
        self.pending.len()
    }
    pub fn latest_completed_seq(&self) -> u32 {
        self.latest_completed_seq
    }

    pub fn configure_repair(&mut self, enabled: bool, frame_period: Duration, rtt: Duration) {
        self.repair_enabled = enabled;
        self.frame_period = frame_period;
        self.rtt = rtt.min(Duration::from_secs(1));
    }

    /// Repair-mode callers drain deliveries and requests after each add/tick,
    /// like the synchronous callbacks in the Swift receiver.
    pub fn take_ready(&mut self) -> Vec<AssembledFrame> {
        self.ready.drain(..).collect()
    }
    pub fn take_nacks(&mut self) -> Vec<Nack> {
        self.nacks.drain(..).collect()
    }
    pub fn take_keyframe_request(&mut self) -> Option<(u32, u32)> {
        std::mem::take(&mut self.keyframe_needed)
            .then_some((self.current_epoch.unwrap_or(0), self.latest_completed_seq))
    }

    /// Compatibility helper for receivers that have not negotiated repair.
    pub fn add_fragment(
        &mut self,
        seq: u32,
        index: u16,
        count: u16,
        epoch: u32,
        payload: &[u8],
        now: Instant,
    ) -> AddOutcome {
        self.add_media(
            MediaHeader {
                session_id: 1,
                stream_epoch: epoch,
                frame_seq: seq,
                frag_index: index,
                frag_count: count,
                is_keyframe: false,
                is_retransmit: false,
                capture_ts_us: 0,
                payload_len: payload.len() as u16,
            },
            payload,
            now,
        )
    }

    pub fn add_media(&mut self, header: MediaHeader, payload: &[u8], now: Instant) -> AddOutcome {
        let (seq, index, count, epoch) = (
            header.frame_seq,
            header.frag_index,
            header.frag_count,
            header.stream_epoch,
        );
        match self.current_epoch {
            Some(current) if epoch > current => {
                self.reset_internal();
                self.current_epoch = Some(epoch);
            }
            Some(current) if epoch < current => {
                self.stale_epoch_streak += 1;
                if self.stale_epoch_streak < EPOCH_RESYNC_THRESHOLD {
                    return AddOutcome::Dropped(DropReason::StaleEpoch);
                }
                self.reset_internal();
                self.current_epoch = Some(epoch);
            }
            Some(_) => self.stale_epoch_streak = 0,
            None => self.current_epoch = Some(epoch),
        }
        // Any current-stream datagram shows media flowing; any replacement
        // shows repairs flowing, including one that lost the race to its original.
        if self.repair_enabled {
            self.resume_after_stall(now);
        }
        self.last_arrival = Some(now);
        if header.is_retransmit {
            self.last_repair = Some(now);
        }
        let retired = self.latest_completed_seq.max(self.retired_seq);
        if retired > 0 {
            if seq == retired {
                return AddOutcome::Dropped(DropReason::DuplicateCompleted);
            }
            if seq < retired {
                if retired - seq > STREAM_RESTART_GAP {
                    self.reset_internal();
                } else {
                    return AddOutcome::Dropped(DropReason::StaleSeq);
                }
            }
        }
        if count == 0 {
            return AddOutcome::Dropped(DropReason::ZeroCount);
        }
        if count > MAX_FRAG_COUNT {
            return AddOutcome::Dropped(DropReason::CountExceedsCap);
        }
        if index >= count {
            return AddOutcome::Dropped(DropReason::IndexOutOfRange);
        }
        self.tick(now);
        if self.repair_enabled && self.retired_seq > 0 && seq <= self.retired_seq {
            return AddOutcome::Dropped(DropReason::StaleSeq);
        }
        if let Some(frame) = self.pending.get(&seq) {
            if frame.header.frag_count != count {
                return AddOutcome::Dropped(DropReason::CountMismatch);
            }
            if frame.fragments.contains_key(&index) {
                return AddOutcome::Dropped(DropReason::DuplicateFragment);
            }
        }
        if payload.len() > MAX_PENDING_BYTES {
            return AddOutcome::Dropped(DropReason::Capacity);
        }
        while (!self.pending.contains_key(&seq) && self.pending.len() >= MAX_PENDING_FRAMES)
            || self.pending_bytes + payload.len() > MAX_PENDING_BYTES
        {
            let Some((&oldest, _)) = self.pending.first_key_value() else {
                break;
            };
            self.drop_pending(oldest);
            if self.repair_enabled {
                self.retired_seq = self.retired_seq.max(oldest);
                self.drain_ready(now);
                if seq <= self.retired_seq {
                    return AddOutcome::Dropped(DropReason::StaleSeq);
                }
            }
        }
        let frame = self.pending.entry(seq).or_insert_with(|| PendingFrame {
            header,
            fragments: HashMap::new(),
            bytes: 0,
            created_at: now,
            contiguous: 0,
            highest_index: 0,
            repaired: 0,
            gap_at: None,
            deadline: None,
            retry_at: None,
            requested_at: None,
            completed_at: None,
            stall_used: Duration::ZERO,
            stall_counted_until: None,
            requested: HashSet::new(),
            retried: false,
            keyframe_requested: false,
        });
        frame.fragments.insert(index, payload.to_vec());
        frame.bytes += payload.len();
        frame.highest_index = frame.highest_index.max(index);
        while frame.contiguous < count && frame.fragments.contains_key(&frame.contiguous) {
            frame.contiguous += 1;
        }
        self.pending_bytes += payload.len();
        if header.is_retransmit {
            frame.repaired += 1;
        } else {
            self.counters.frags_received += 1;
            let origin = *self.clock_origin.get_or_insert(now);
            let transit = now.saturating_duration_since(origin).as_micros() as f64
                - header.capture_ts_us as f64;
            if let Some(previous) = self.previous_transit {
                self.jitter += ((transit - previous).abs() - self.jitter) / 16.0;
            }
            self.previous_transit = Some(transit);
        }
        self.counters.highest_seq = self.counters.highest_seq.max(seq);
        self.counters.stream_epoch = epoch;
        self.counters.jitter_us = self.jitter as u32;
        let complete = frame.complete();
        if complete {
            frame.completed_at = Some(now);
        }
        if self.repair_enabled {
            if !complete && frame.contiguous < frame.highest_index {
                let through = frame.highest_index;
                self.begin_gap(seq, through, now);
            }
            if complete {
                let older: Vec<_> = self
                    .pending
                    .range(..seq)
                    .filter(|(_, f)| !f.complete())
                    .map(|(&s, f)| (s, f.header.frag_count - 1))
                    .collect();
                for (s, through) in older {
                    self.begin_gap(s, through, now);
                }
            }
            self.drain_ready(now);
        } else if complete {
            self.deliver(seq);
            let stale: Vec<_> = self.pending.range(..=seq).map(|(&s, _)| s).collect();
            for s in stale {
                self.drop_pending(s);
            }
        }
        self.update_depth();
        if !self.repair_enabled {
            if let Some(frame) = self.ready.pop_front() {
                return AddOutcome::Completed(frame.payload);
            }
        }
        AddOutcome::Stored
    }

    pub fn tick(&mut self, now: Instant) {
        let keys: Vec<_> = self.pending.keys().copied().collect();
        for seq in keys {
            let Some(frame) = self.pending.get(&seq) else {
                continue;
            };
            if frame.complete() {
                continue;
            }
            if self.repair_enabled {
                if frame
                    .deadline
                    .is_some_and(|deadline| now >= deadline + self.stall(frame, now))
                {
                    if self.pending.first_key_value().map(|(&s, _)| s) == Some(seq) {
                        self.drop_pending(seq);
                        self.retired_seq = self.retired_seq.max(seq);
                        self.drain_ready(now);
                    }
                } else if frame.deadline.is_none()
                    && now.saturating_duration_since(frame.created_at) >= self.frame_period
                {
                    let through = frame.header.frag_count - 1;
                    self.begin_gap(seq, through, now);
                } else if !frame.retried && frame.retry_at.is_some_and(|retry| now >= retry) {
                    let through = frame.header.frag_count - 1;
                    if let Some(frame) = self.pending.get_mut(&seq) {
                        frame.retried = true;
                    }
                    self.request(seq, through, true, now);
                }
            } else if now.saturating_duration_since(frame.created_at) >= STALE_FRAME_TIMEOUT {
                self.drop_pending(seq);
            }
        }
        if self.repair_enabled {
            self.drain_ready(now);
        }
        self.update_depth();
    }

    /// The first gap starts the repair deadline and schedules the one retry;
    /// every gap requests the fragments it newly shows missing (see
    /// `FrameAssembler.beginGap`).
    fn begin_gap(&mut self, seq: u32, through: u16, now: Instant) {
        let Some(frame) = self.pending.get_mut(&seq) else {
            return;
        };
        if frame.deadline.is_none() {
            let window_us = (2 * self.rtt.as_micros() + self.frame_period.as_micros() * 3 / 2)
                .clamp(8_000, 25_000);
            frame.gap_at = Some(now);
            frame.deadline = Some(now + Duration::from_micros(window_us as u64));
            frame.retry_at = Some(now + self.rtt + Duration::from_millis(5));
        }
        self.request(seq, through, false, now);
    }

    /// How long this waiting frame has been stalled, not yet counted, within
    /// its remaining allowance (see `FrameAssembler.stall(of:at:)`).
    fn stall(&self, frame: &PendingFrame, now: Instant) -> Duration {
        let Some(gap) = frame.gap_at else {
            return Duration::ZERO;
        };
        let mut silent_from = self.last_arrival.map(|last| last + self.frame_period);
        if let Some(asked) = frame.requested_at {
            let due = self.last_repair.map_or(asked, |repair| repair.max(asked))
                + self.rtt
                + Duration::from_millis(5);
            silent_from = Some(silent_from.map_or(due, |silent| silent.min(due)));
        }
        let Some(mut from) = silent_from else {
            return Duration::ZERO;
        };
        from = from.max(gap);
        if let Some(counted) = frame.stall_counted_until {
            from = from.max(counted);
        }
        if now <= from {
            return Duration::ZERO;
        }
        (now - from).min(MAX_STALL_ALLOWANCE - frame.stall_used)
    }

    /// Traffic arrived: move each waiting frame's deadline past the stall so far.
    fn resume_after_stall(&mut self, now: Instant) {
        let seqs: Vec<_> = self.pending.keys().copied().collect();
        for seq in seqs {
            let extra = match self.pending.get(&seq) {
                Some(frame) if frame.deadline.is_some() => self.stall(frame, now),
                _ => continue,
            };
            if extra.is_zero() {
                continue;
            }
            if let Some(frame) = self.pending.get_mut(&seq) {
                frame.deadline = frame.deadline.map(|deadline| deadline + extra);
                frame.stall_used += extra;
                frame.stall_counted_until = Some(now);
            }
        }
    }

    fn request(&mut self, seq: u32, through: u16, again: bool, now: Instant) {
        let Some(frame) = self.pending.get_mut(&seq) else {
            return;
        };
        if frame.contiguous > through {
            return;
        }
        let missing: Vec<_> = (frame.contiguous..=through)
            .filter(|i| !frame.fragments.contains_key(i) && (again || !frame.requested.contains(i)))
            .collect();
        if missing.is_empty() {
            return;
        }
        frame.requested.extend(&missing);
        frame.requested_at.get_or_insert(now);
        let frag_count = frame.header.frag_count;
        for chunk in missing.chunks(64) {
            self.counters.nacks_sent += 1;
            self.nacks.push_back(Nack {
                stream_epoch: self.current_epoch.unwrap_or(0),
                frame_seq: seq,
                frag_count,
                missing: chunk.to_vec(),
            });
        }
    }

    fn request_keyframe(&mut self, seq: u32) {
        // A complete queued keyframe restores references after this loss.
        // Keep the repair deadline, but do not request another replacement.
        if self
            .pending
            .iter()
            .any(|(&later, frame)| later > seq && frame.header.is_keyframe && frame.complete())
        {
            return;
        }
        if let Some(frame) = self.pending.get_mut(&seq) {
            if !frame.keyframe_requested {
                self.keyframe_needed = true;
                frame.keyframe_requested = true;
            }
        }
    }

    /// Delivers complete frames in order; a complete frame that skips a
    /// sequence number waits up to half a frame period for it (see
    /// `FrameAssembler.drainReady(at:)`).
    fn drain_ready(&mut self, now: Instant) {
        let hold = Duration::from_micros(self.frame_period.as_micros() as u64 / 2);
        while let Some((&seq, frame)) = self.pending.first_key_value() {
            if !frame.complete() {
                break;
            }
            let resolved = self.latest_completed_seq.max(self.retired_seq);
            if resolved > 0
                && seq > resolved.wrapping_add(1)
                && !frame.header.is_keyframe
                && now < frame.completed_at.unwrap_or(now) + hold
            {
                break;
            }
            self.deliver(seq);
        }
    }

    fn deliver(&mut self, seq: u32) {
        let Some(frame) = self.pending.remove(&seq) else {
            return;
        };
        self.pending_bytes -= frame.bytes;
        let mut payload = Vec::with_capacity(frame.bytes);
        for index in 0..frame.header.frag_count {
            payload.extend_from_slice(&frame.fragments[&index]);
        }
        self.latest_completed_seq = seq;
        self.retired_seq = self.retired_seq.max(seq);
        self.counters.frames_complete += 1;
        self.counters.frags_repaired += frame.repaired;
        self.ready.push_back(AssembledFrame {
            seq,
            capture_ts_us: frame.header.capture_ts_us,
            is_keyframe: frame.header.is_keyframe,
            payload,
        });
    }

    fn drop_pending(&mut self, seq: u32) {
        self.request_keyframe(seq);
        if let Some(frame) = self.pending.remove(&seq) {
            self.pending_bytes -= frame.bytes;
            self.counters.frames_dropped += 1;
            self.counters.frags_lost += frame.missing_fragments();
        }
    }

    fn update_depth(&mut self) {
        self.counters.assembler_depth =
            self.pending.values().filter(|f| !f.complete()).count() as u8;
    }

    pub fn reset(&mut self) {
        self.reset_internal();
        self.current_epoch = None;
    }
    fn reset_internal(&mut self) {
        self.pending.clear();
        self.pending_bytes = 0;
        self.latest_completed_seq = 0;
        self.retired_seq = 0;
        self.stale_epoch_streak = 0;
        self.ready.clear();
        self.nacks.clear();
        self.keyframe_needed = false;
        self.clock_origin = None;
        self.previous_transit = None;
        self.jitter = 0.0;
        self.last_arrival = None;
        self.last_repair = None;
        self.counters = ReassemblyCounters::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_traces_match_the_swift_specification() {
        let base = Instant::now();
        let mut r = Reassembler::new();
        let mut delivered = Vec::new();
        let mut requests = Vec::new();
        let mut keyframes = 0;
        for (line_number, line) in include_str!("../testdata/repair_vectors.txt")
            .lines()
            .enumerate()
        {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.is_empty() || fields[0].starts_with('#') {
                continue;
            }
            let number = |i: usize| fields[i].parse::<u64>().unwrap();
            match fields[0] {
                "reset" => {
                    r.reset();
                    r.configure_repair(
                        true,
                        Duration::from_micros(number(1)),
                        Duration::from_micros(number(2)),
                    );
                    delivered.clear();
                    requests.clear();
                    keyframes = 0;
                }
                "add" => {
                    r.add_media(
                        MediaHeader {
                            session_id: 1,
                            stream_epoch: number(2) as u32,
                            frame_seq: number(3) as u32,
                            frag_index: number(4) as u16,
                            frag_count: number(5) as u16,
                            is_retransmit: number(6) != 0,
                            is_keyframe: fields.get(8).is_some_and(|value| *value == "1"),
                            capture_ts_us: 0,
                            payload_len: 1,
                        },
                        &[number(7) as u8],
                        base + Duration::from_micros(number(1)),
                    );
                }
                "tick" => r.tick(base + Duration::from_micros(number(1))),
                "expect" => {
                    let c = r.counters();
                    assert_eq!(
                        [
                            c.frames_complete,
                            c.frames_dropped,
                            c.frags_received,
                            c.frags_lost,
                            c.frags_repaired,
                            c.nacks_sent,
                            u64::from(c.assembler_depth)
                        ],
                        std::array::from_fn::<_, 7, _>(|i| number(i + 1)),
                        "line {}",
                        line_number + 1
                    );
                    assert_eq!(
                        if delivered.is_empty() {
                            "-".into()
                        } else {
                            delivered.join(",")
                        },
                        fields[8],
                        "line {}",
                        line_number + 1
                    );
                    assert_eq!(keyframes, number(9), "line {}", line_number + 1);
                    assert_eq!(
                        if requests.is_empty() {
                            "-".into()
                        } else {
                            requests.join("|")
                        },
                        fields[10],
                        "line {}",
                        line_number + 1
                    );
                }
                command => panic!("unknown trace command {command}"),
            }
            delivered.extend(r.take_ready().into_iter().map(|f| f.seq.to_string()));
            requests.extend(r.take_nacks().into_iter().map(|n| {
                format!(
                    "{}:{}",
                    n.frame_seq,
                    n.missing
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }));
            if r.take_keyframe_request().is_some() {
                keyframes += 1;
            }
        }
    }

    fn feed(
        r: &mut Reassembler,
        seq: u32,
        index: u16,
        count: u16,
        epoch: u32,
        byte: u8,
        now: Instant,
    ) -> AddOutcome {
        r.add_fragment(seq, index, count, epoch, &[byte], now)
    }

    #[test]
    fn assembles_in_order_and_out_of_order() {
        let now = Instant::now();
        let mut r = Reassembler::new();

        assert_eq!(feed(&mut r, 1, 0, 3, 1, 0xA, now), AddOutcome::Stored);
        assert_eq!(feed(&mut r, 1, 2, 3, 1, 0xC, now), AddOutcome::Stored);
        assert_eq!(
            feed(&mut r, 1, 1, 3, 1, 0xB, now),
            AddOutcome::Completed(vec![0xA, 0xB, 0xC])
        );
        assert_eq!(r.counters().frames_complete, 1);

        // Reordered single-fragment frame completes immediately.
        assert_eq!(
            feed(&mut r, 2, 0, 1, 1, 0xD, now),
            AddOutcome::Completed(vec![0xD])
        );
    }

    #[test]
    fn duplicate_of_completed_frame_is_ignored() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 1, 1, 0xA, now);
        assert_eq!(
            feed(&mut r, 1, 0, 1, 1, 0xA, now),
            AddOutcome::Dropped(DropReason::DuplicateCompleted)
        );
    }

    #[test]
    fn stale_seq_within_gap_is_dropped_but_big_backjump_resets() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 500, 0, 1, 1, 0xA, now);

        assert_eq!(
            feed(&mut r, 499, 0, 1, 1, 0xB, now),
            AddOutcome::Dropped(DropReason::StaleSeq)
        );

        // Backward jump beyond the gap = legacy restart detection.
        assert_eq!(
            feed(&mut r, 1, 0, 1, 1, 0xC, now),
            AddOutcome::Completed(vec![0xC])
        );
        assert_eq!(r.latest_completed_seq(), 1);
    }

    #[test]
    fn higher_epoch_resets_lower_epoch_is_dropped() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 900, 0, 1, 5, 0xA, now);

        // Same-epoch stale fragment: dropped by seq rules.
        assert_eq!(
            feed(&mut r, 890, 0, 1, 5, 0xB, now),
            AddOutcome::Dropped(DropReason::StaleSeq)
        );

        // New pipeline run: epoch bumps, small seq accepted immediately.
        assert_eq!(
            feed(&mut r, 1, 0, 1, 6, 0xC, now),
            AddOutcome::Completed(vec![0xC])
        );

        // Straggler from the old run: dropped on epoch alone.
        assert_eq!(
            feed(&mut r, 901, 0, 1, 5, 0xD, now),
            AddOutcome::Dropped(DropReason::StaleEpoch)
        );
    }

    #[test]
    fn one_bogus_epoch_cannot_strand_the_stream() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 10, 0, 1, 5, 0xA, now);

        // One corrupted/spoofed fragment claiming the maximum epoch.
        feed(&mut r, 11, 0, 1, u32::MAX, 0xFF, now);

        // The real stream is now "stale" against an epoch it can never reach.
        for i in 0..(EPOCH_RESYNC_THRESHOLD - 1) {
            assert_eq!(
                feed(&mut r, 100 + i, 0, 1, 5, 0xB, now),
                AddOutcome::Dropped(DropReason::StaleEpoch),
                "fragment {i} should still be dropped before the resync threshold"
            );
        }

        // Crossing the threshold re-syncs to the stream that is really there.
        assert_eq!(
            feed(&mut r, 900, 0, 1, 5, 0xC, now),
            AddOutcome::Completed(vec![0xC]),
            "the receiver must recover instead of staying bricked forever"
        );
        // And it keeps flowing afterwards.
        assert_eq!(
            feed(&mut r, 901, 0, 1, 5, 0xD, now),
            AddOutcome::Completed(vec![0xD])
        );
    }

    #[test]
    fn interleaved_stragglers_never_trip_the_resync() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 1, 7, 0xA, now);

        // A real restart: epoch 8 is live, epoch 7 stragglers keep arriving
        // alongside it. Far more stale drops than the threshold, but the
        // accepted fragments in between must keep resetting the streak.
        for i in 0..(EPOCH_RESYNC_THRESHOLD * 2) {
            assert_eq!(
                feed(&mut r, 1000 + i, 0, 1, 8, 0xB, now),
                AddOutcome::Completed(vec![0xB])
            );
            assert_eq!(
                feed(&mut r, 500 + i, 0, 1, 7, 0xC, now),
                AddOutcome::Dropped(DropReason::StaleEpoch),
                "old-run straggler {i} must stay dropped"
            );
        }
    }

    #[test]
    fn completion_evicts_older_partials_and_counts_them_dropped() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        // Frame 1 partial (1 of 2 fragments), then frame 2 completes.
        feed(&mut r, 1, 0, 2, 1, 0xA, now);
        assert_eq!(
            feed(&mut r, 2, 0, 1, 1, 0xB, now),
            AddOutcome::Completed(vec![0xB])
        );
        assert_eq!(r.pending_frames(), 0);
        assert_eq!(r.counters().frames_dropped, 1);
        // One fragment of the two-fragment frame never arrived.
        assert_eq!(r.counters().frags_lost, 1);

        // The evicted frame's late fragment is now stale.
        assert_eq!(
            feed(&mut r, 1, 1, 2, 1, 0xC, now),
            AddOutcome::Dropped(DropReason::StaleSeq)
        );
    }

    #[test]
    fn first_seen_fragment_count_wins_and_progress_survives() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 3, 1, 0xA, now);

        // A fragment claiming a different count for a live frame is ignored;
        // it must not discard what has already been buffered.
        assert_eq!(
            feed(&mut r, 1, 1, 2, 1, 0xB, now),
            AddOutcome::Dropped(DropReason::CountMismatch)
        );

        assert_eq!(feed(&mut r, 1, 1, 3, 1, 0xB, now), AddOutcome::Stored);
        assert_eq!(
            feed(&mut r, 1, 2, 3, 1, 0xC, now),
            AddOutcome::Completed(vec![0xA, 0xB, 0xC]),
            "the original fragments must still be there"
        );
    }

    #[test]
    fn replayed_fragment_neither_overwrites_nor_counts_twice() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 2, 1, 0xA, now);
        assert_eq!(
            feed(&mut r, 1, 0, 2, 1, 0xFF, now),
            AddOutcome::Dropped(DropReason::DuplicateFragment)
        );
        assert_eq!(r.counters().frags_received, 1, "a replay is not a receipt");
        assert_eq!(
            feed(&mut r, 1, 1, 2, 1, 0xB, now),
            AddOutcome::Completed(vec![0xA, 0xB]),
            "the first write must win"
        );
    }

    #[test]
    fn loss_counts_the_fragments_that_never_arrived() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        // Frame 1 gets 9 of 10 fragments, then a newer frame completes and
        // evicts it: exactly ONE fragment was lost, not nine.
        for index in 0..9u16 {
            feed(&mut r, 1, index, 10, 1, 0xA, now);
        }
        assert_eq!(
            feed(&mut r, 2, 0, 1, 1, 0xB, now),
            AddOutcome::Completed(vec![0xB])
        );
        assert_eq!(r.counters().frames_dropped, 1);
        assert_eq!(r.counters().frags_lost, 1);
    }

    #[test]
    fn invalid_fragments_are_rejected() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        assert_eq!(
            feed(&mut r, 1, 0, 0, 1, 0xA, now),
            AddOutcome::Dropped(DropReason::ZeroCount)
        );
        assert_eq!(
            feed(&mut r, 1, 2, 2, 1, 0xA, now),
            AddOutcome::Dropped(DropReason::IndexOutOfRange)
        );
    }

    #[test]
    fn stale_partials_are_evicted_on_cleanup_tick() {
        let start = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 1, 0, 2, 1, 0xA, start);

        // 99 more fragments to hit the cleanup interval, well past the timeout.
        let later = start + Duration::from_millis(500);
        for i in 0..99u32 {
            feed(&mut r, 10 + i, 0, 2, 1, 0xB, later);
        }
        assert!(
            !r.pending.contains_key(&1),
            "stale frame must be evicted by the periodic cleanup"
        );
        assert!(r.counters().frames_dropped >= 1);
    }

    #[test]
    fn reset_clears_epoch_and_state() {
        let now = Instant::now();
        let mut r = Reassembler::new();
        feed(&mut r, 5, 0, 2, 9, 0xA, now);
        r.reset();
        assert_eq!(r.pending_frames(), 0);
        // After reset any epoch is accepted again.
        assert_eq!(
            feed(&mut r, 1, 0, 1, 2, 0xB, now),
            AddOutcome::Completed(vec![0xB])
        );
    }
}
