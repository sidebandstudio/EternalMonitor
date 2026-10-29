//! Deterministic simulation of the iPad's repair over a WiFi-like link.
//!
//! The host sends frames, keeps every fragment for repair and answers each
//! NACK at once; it grants one keyframe per 500 ms, as the app's session does.
//! The iPad reassembles with the app's rules (`Reassembler` mirrors
//! `FrameAssembler.swift`), ticks every millisecond like its repair timer,
//! and asks for a keyframe at most once per 500 ms, like `ControlChannel`.
//!
//! The link models three WiFi behaviours. Each datagram is lost with
//! probability `loss`. A packet capture recorded iPad repair requests
//! arriving about 100 ms late, so some uplink datagrams are 50-150 ms late.
//! Power saving lets the iPad's radio doze 50 ms after its last traffic; the
//! access point then holds downlink datagrams until the next 102.4 ms beacon.
//! And now and then the downlink stalls for 40-150 ms and then releases
//! everything at once.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use eternal_wire::reassembly::Reassembler;
use eternal_wire::v2::control::Nack;
use eternal_wire::v2::media::MediaHeader;

const KEYFRAME_SPACING_US: u64 = 500_000;
const GOP: u64 = 30;

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    fps: u64,
    delta_fragments: u16,
    keyframe_fragments: u16,
    loss: f64,
    late_uplink: f64,
    stall_every_s: u64,
    seconds: u64,
    /// Number frames as 0.3.1 did, by capture: one number in seven skipped,
    /// as the encoder mailbox skipped 14% of captured frames in the field.
    capture_skips: bool,
}

#[derive(Debug, Default)]
struct Outcome {
    frames: u64,
    decodable: u64,
    /// Delta frames shown with a missing reference: the smearing a user
    /// sees until the next keyframe.
    broken: u64,
    dropped: u64,
    keyframe_requests: u64,
    keyframes_sent: u64,
    nacks: u64,
    latency_p50_ms: f64,
    latency_p90_ms: f64,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

enum Event {
    SendFrame,
    Media(MediaHeader),
    Nack(Nack),
    KeyframeRequest,
    Tick,
}

const DOZE_AFTER_US: u64 = 50_000;
const BEACON_US: u64 = 102_400;

/// A datagram's fate depends only on which datagram it is, never on what
/// else was sent before it. Changing the receiver therefore cannot change
/// which original fragments are lost, and runs compare like for like.
fn noise(key: [u64; 3], draw: u64) -> f64 {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for part in key.into_iter().chain([draw]) {
        x = (x ^ part).wrapping_add(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
    }
    (x >> 11) as f64 / (1u64 << 53) as f64
}

struct Link {
    loss: f64,
    late_uplink: f64,
    awake_until: u64,
    stalls: Vec<(u64, u64)>,
}

impl Link {
    fn air_us(key: [u64; 3]) -> u64 {
        500 + (noise(key, 1) * 2_500.0) as u64
    }

    /// When a datagram sent now reaches the iPad, or `None` if it is lost.
    fn downlink(&mut self, now: u64, key: [u64; 3]) -> Option<u64> {
        if noise(key, 0) < self.loss {
            return None;
        }
        let mut at = now + Self::air_us(key);
        if let Some(&(_, end)) = self
            .stalls
            .iter()
            .find(|(start, end)| (*start..*end).contains(&at))
        {
            at = end;
        }
        if at > self.awake_until {
            at = at.div_ceil(BEACON_US) * BEACON_US;
        }
        self.awake_until = self.awake_until.max(at + DOZE_AFTER_US);
        Some(at)
    }

    /// When a datagram the iPad sends now reaches the PC.
    fn uplink(&mut self, now: u64, key: [u64; 3]) -> Option<u64> {
        self.awake_until = self.awake_until.max(now + DOZE_AFTER_US);
        if noise(key, 0) < self.loss {
            return None;
        }
        Some(if noise(key, 2) < self.late_uplink {
            now + 50_000 + (noise(key, 3) * 100_000.0) as u64
        } else {
            now + Self::air_us(key)
        })
    }
}

fn simulate(s: Scenario) -> Outcome {
    let period = 1_000_000 / s.fps;
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let stalls = (1..s.seconds / s.stall_every_s.max(1))
        .map(|n| {
            let start = n * s.stall_every_s * 1_000_000 + (rng.next() * 1_000_000.0) as u64;
            (start, start + 40_000 + (rng.next() * 110_000.0) as u64)
        })
        .filter(|_| s.stall_every_s > 0)
        .collect();
    let mut link = Link {
        loss: s.loss,
        late_uplink: s.late_uplink,
        awake_until: 0,
        stalls,
    };
    let base = Instant::now();
    let at = |us: u64| base + Duration::from_micros(us);
    let mut r = Reassembler::new();
    r.configure_repair(
        true,
        Duration::from_micros(period),
        Duration::from_millis(3),
    );

    let mut events: BTreeMap<(u64, u64), Event> = BTreeMap::new();
    let mut order = 0u64;
    let mut push = |events: &mut BTreeMap<(u64, u64), Event>, time: u64, event: Event| {
        order += 1;
        events.insert((time, order), event);
    };
    push(&mut events, 0, Event::SendFrame);
    push(&mut events, 0, Event::Tick);

    let end = s.seconds * 1_000_000;
    let mut outcome = Outcome::default();
    let mut seq = 0u32;
    let mut sent_at: HashMap<u32, (u64, u16, bool)> = HashMap::new();
    let mut keyframe_pending = false;
    let mut last_grant: Option<u64> = None;
    let mut last_request: Option<u64> = None;
    let mut decodes_from = None::<u32>;
    let mut latencies = Vec::new();
    let mut resends: HashMap<(u32, u16), u64> = HashMap::new();
    let mut previous_sent: HashMap<u32, u32> = HashMap::new();
    let mut nacks_sent: HashMap<u32, u64> = HashMap::new();

    while let Some(((now, _), event)) = events.pop_first() {
        if now > end {
            break;
        }
        match event {
            Event::SendFrame => {
                let previous = seq;
                seq += 1;
                if s.capture_skips && outcome.frames % 7 == 6 {
                    seq += 1;
                }
                let keyframe = outcome.frames.is_multiple_of(GOP) || keyframe_pending;
                keyframe_pending = false;
                outcome.keyframes_sent += u64::from(keyframe);
                let count = if keyframe {
                    s.keyframe_fragments
                } else {
                    s.delta_fragments
                };
                sent_at.insert(seq, (now, count, keyframe));
                previous_sent.insert(seq, previous);
                for index in 0..count {
                    let header = header(seq, index, count, keyframe, false);
                    let key = [1, u64::from(seq), u64::from(index)];
                    if let Some(arrival) = link.downlink(now + 50 * u64::from(index), key) {
                        push(&mut events, arrival, Event::Media(header));
                    }
                }
                outcome.frames += 1;
                push(&mut events, now + period, Event::SendFrame);
            }
            Event::Media(header) => {
                r.add_media(header, &[0], at(now));
            }
            Event::Nack(nack) => {
                for &index in &nack.missing {
                    let Some(&(_, count, keyframe)) = sent_at.get(&nack.frame_seq) else {
                        continue;
                    };
                    let attempt = resends.entry((nack.frame_seq, index)).or_insert(0u64);
                    *attempt += 1;
                    let key = [
                        2,
                        u64::from(nack.frame_seq) << 16 | u64::from(index),
                        *attempt,
                    ];
                    if let Some(arrival) = link.downlink(now, key) {
                        let header = header(nack.frame_seq, index, count, keyframe, true);
                        push(&mut events, arrival, Event::Media(header));
                    }
                }
            }
            Event::KeyframeRequest => {
                if last_grant.is_none_or(|grant| now - grant >= KEYFRAME_SPACING_US) {
                    last_grant = Some(now);
                    keyframe_pending = true;
                }
            }
            Event::Tick => {
                r.tick(at(now));
                push(&mut events, now + 1_000, Event::Tick);
            }
        }
        for nack in r.take_nacks() {
            outcome.nacks += 1;
            let sent = nacks_sent.entry(nack.frame_seq).or_insert(0u64);
            *sent += 1;
            if let Some(arrival) = link.uplink(now, [3, u64::from(nack.frame_seq), *sent]) {
                push(&mut events, arrival, Event::Nack(nack));
            }
        }
        if r.take_keyframe_request().is_some()
            && last_request.is_none_or(|request| now - request >= KEYFRAME_SPACING_US)
        {
            last_request = Some(now);
            outcome.keyframe_requests += 1;
            if let Some(arrival) = link.uplink(now, [4, outcome.keyframe_requests, 0]) {
                push(&mut events, arrival, Event::KeyframeRequest);
            }
        }
        for frame in r.take_ready() {
            // A delta frame decodes only right after the frame sent before it,
            // if that one decoded.
            let decodes =
                frame.is_keyframe || decodes_from == previous_sent.get(&frame.seq).copied();
            decodes_from = decodes.then_some(frame.seq);
            if decodes {
                outcome.decodable += 1;
                latencies.push((now - sent_at[&frame.seq].0) as f64 / 1000.0);
            } else {
                outcome.broken += 1;
            }
        }
    }
    outcome.dropped = r.counters().frames_dropped;
    latencies.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        latencies
            .get(((latencies.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    outcome.latency_p50_ms = percentile(0.5);
    outcome.latency_p90_ms = percentile(0.9);
    outcome
}

fn header(seq: u32, index: u16, count: u16, keyframe: bool, retransmit: bool) -> MediaHeader {
    MediaHeader {
        session_id: 1,
        stream_epoch: 1,
        frame_seq: seq,
        frag_index: index,
        frag_count: count,
        is_keyframe: keyframe,
        is_retransmit: retransmit,
        capture_ts_us: 0,
        payload_len: 1,
    }
}

const STILL: Scenario = Scenario {
    name: "still desktop, 4 fps",
    fps: 4,
    delta_fragments: 2,
    keyframe_fragments: 40,
    loss: 0.05,
    late_uplink: 0.3,
    stall_every_s: 0,
    seconds: 600,
    capture_skips: false,
};
const MOVING: Scenario = Scenario {
    name: "moving, 60 fps",
    fps: 60,
    delta_fragments: 10,
    keyframe_fragments: 60,
    loss: 0.01,
    late_uplink: 0.3,
    stall_every_s: 5,
    seconds: 120,
    capture_skips: false,
};
const PROMOTION: Scenario = Scenario {
    name: "moving, 120 fps",
    fps: 120,
    delta_fragments: 6,
    keyframe_fragments: 60,
    loss: 0.01,
    late_uplink: 0.3,
    stall_every_s: 5,
    seconds: 120,
    capture_skips: false,
};

/// A still desktop was where one iPad dropped 503 of its 525 frames and asked
/// for a keyframe each time: a frame with nothing newer behind it was dropped
/// at its deadline although asking again would have repaired it. Its repair
/// counters imply several percent loss in that phase.
#[test]
fn a_still_desktop_waits_for_late_repairs_instead_of_keyframes() {
    let outcome = simulate(STILL);
    eprintln!("WIFI_SIM {} {outcome:?}", STILL.name);
    assert!(
        outcome.keyframe_requests * 200 <= outcome.frames,
        "{outcome:?}"
    );
    assert!(outcome.dropped * 200 <= outcome.frames, "{outcome:?}");
}

/// Prints the measurement table used in the release report, with frames
/// numbered contiguously and as 0.3.1 numbered them.
#[test]
fn wifi_repair_table() {
    for scenario in [STILL, MOVING, PROMOTION] {
        for capture_skips in [false, true] {
            let outcome = simulate(Scenario {
                capture_skips,
                ..scenario
            });
            let numbering = if capture_skips {
                "capture"
            } else {
                "contiguous"
            };
            eprintln!("WIFI_SIM {} {numbering} {outcome:?}", scenario.name);
        }
    }
}
