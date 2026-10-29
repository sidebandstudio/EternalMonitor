//! Differential test of the USB link's video queue against the one it
//! replaced. Owner's rule: never degrade a working link. Wherever the old
//! queue lost no frame, the new one must lose none either and deliver the same
//! frames in the same order.
//!
//! `BaseLink` is the video path of `transport/link.rs` on
//! release/0.3.4-candidate before #118, copied as it was. The schedules carry
//! only video, so its control and audio paths, which never ran, are left out.
//! Both links run on the paused tokio clock through the tunnel model of
//! `usb_link_sim.rs`, which here also stands still at times, as usbmuxd does.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eternal_host::transport::link::{
    read_datagram, read_preamble, write_datagram, FramedLink, Link, PeerId,
};
use eternal_wire::v2::media::{MediaHeader, MEDIA_HEADER_SIZE};
use eternal_wire::v2::MAX_DGRAM_SIZE;
use parking_lot::Mutex;
use tokio::io::{AsyncWrite, DuplexStream, WriteHalf};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;

mod base {
    //! The candidate's queue before #118, unchanged but for names.
    use super::*;

    const FRAME_LIMIT: usize = 2;
    const BYTE_LIMIT: usize = 1024 * 1024;

    fn invalid(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    struct Frame {
        epoch: u32,
        seq: u32,
        frag_count: u16,
        datagrams: Vec<Vec<u8>>,
        bytes: usize,
    }

    impl Frame {
        fn complete(&self) -> bool {
            self.datagrams.len() == usize::from(self.frag_count)
        }
    }

    #[derive(Default)]
    struct VideoQueue {
        frames: VecDeque<Frame>,
        bytes: usize,
        revision: u64,
        awaiting_keyframe: bool,
        discarded: Option<(u32, u32)>,
    }

    impl VideoQueue {
        fn clear_for_recovery(&mut self) -> u64 {
            let dropped = self.frames.len() as u64;
            self.frames.clear();
            self.bytes = 0;
            self.revision = self.revision.wrapping_add(1);
            self.awaiting_keyframe = true;
            dropped
        }

        fn push(&mut self, bytes: &[u8]) -> io::Result<u64> {
            let (header, _) = MediaHeader::decode(bytes).map_err(|_| invalid("video header"))?;
            let key = (header.stream_epoch, header.frame_seq);
            if self.discarded == Some(key) {
                return Ok(0);
            }
            if self.awaiting_keyframe {
                if header.is_keyframe && header.frag_index == 0 {
                    self.awaiting_keyframe = false;
                } else {
                    self.discarded = Some(key);
                    return Ok(1);
                }
            }

            let mut dropped = 0;
            if header.frag_index == 0 {
                if self.frames.back().is_some_and(|frame| !frame.complete()) {
                    return Err(invalid("unfinished video access unit"));
                }
                if self.frames.len() >= FRAME_LIMIT || self.bytes + bytes.len() > BYTE_LIMIT {
                    dropped += self.clear_for_recovery();
                    if !header.is_keyframe {
                        self.discarded = Some(key);
                        return Ok(dropped + 1);
                    }
                    self.awaiting_keyframe = false;
                }
                self.frames.push_back(Frame {
                    epoch: header.stream_epoch,
                    seq: header.frame_seq,
                    frag_count: header.frag_count,
                    datagrams: Vec::with_capacity(usize::from(header.frag_count).min(1024)),
                    bytes: 0,
                });
            }
            let Some(frame) = self.frames.back_mut() else {
                return Err(invalid("missing first fragment"));
            };
            if (frame.epoch, frame.seq) != key
                || frame.frag_count != header.frag_count
                || frame.datagrams.len() != usize::from(header.frag_index)
            {
                return Err(invalid("video fragment order"));
            }
            if self.bytes + bytes.len() > BYTE_LIMIT {
                dropped += self.clear_for_recovery();
                self.discarded = Some(key);
                return Ok(dropped);
            }
            frame.bytes += bytes.len();
            frame.datagrams.push(bytes.to_vec());
            self.bytes += bytes.len();
            Ok(dropped)
        }

        fn pop(&mut self) -> Option<(u64, Frame)> {
            if !self.frames.front().is_some_and(Frame::complete) {
                return None;
            }
            let frame = self.frames.pop_front().unwrap();
            self.bytes -= frame.bytes;
            Some((self.revision, frame))
        }
    }

    struct WriterState {
        video: Mutex<VideoQueue>,
        ready: Notify,
        closed: watch::Sender<bool>,
        frames_dropped: AtomicU64,
        force_idr: AtomicBool,
    }

    impl WriterState {
        fn record_drops(&self, dropped: u64) {
            if dropped > 0 {
                self.frames_dropped.fetch_add(dropped, Ordering::Relaxed);
                self.force_idr.store(true, Ordering::SeqCst);
            }
        }
    }

    pub struct BaseLink {
        state: Arc<WriterState>,
        writing: JoinHandle<()>,
    }

    impl BaseLink {
        pub fn start(writer: WriteHalf<DuplexStream>) -> Self {
            let state = Arc::new(WriterState {
                video: Mutex::new(VideoQueue::default()),
                ready: Notify::new(),
                closed: watch::channel(false).0,
                frames_dropped: AtomicU64::new(0),
                force_idr: AtomicBool::new(false),
            });
            let write_state = Arc::clone(&state);
            let writing = tokio::spawn(async move {
                let _ = write_loop(writer, &write_state).await;
                let _ = write_state.closed.send(true);
            });
            Self { state, writing }
        }

        pub fn send(&self, datagram: &[u8]) -> io::Result<()> {
            let dropped = self.state.video.lock().push(datagram)?;
            self.state.record_drops(dropped);
            self.state.ready.notify_one();
            Ok(())
        }

        pub fn frames_dropped(&self) -> u64 {
            self.state.frames_dropped.load(Ordering::Relaxed)
        }

        pub fn take_keyframe_request(&self) -> bool {
            self.state.force_idr.swap(false, Ordering::SeqCst)
        }
    }

    impl Drop for BaseLink {
        fn drop(&mut self) {
            let _ = self.state.closed.send(true);
            self.writing.abort();
        }
    }

    async fn write_loop(
        mut writer: impl AsyncWrite + Unpin,
        state: &WriterState,
    ) -> io::Result<()> {
        let mut closed = state.closed.subscribe();
        loop {
            if *closed.borrow() {
                return Ok(());
            }
            let frame = state.video.lock().pop();
            if let Some((revision, frame)) = frame {
                let count = frame.datagrams.len();
                for (index, packet) in frame.datagrams.into_iter().enumerate() {
                    tokio::select! {
                        biased;
                        _ = closed.changed() => return Ok(()),
                        sent = write_datagram(&mut writer, &packet) => sent?,
                    }
                    if index + 1 < count && state.video.lock().revision != revision {
                        state.record_drops(1);
                        break;
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    _ = closed.changed() => return Ok(()),
                    _ = state.ready.notified() => {},
                }
            }
        }
    }
}

use base::BaseLink;

#[derive(Clone, Copy, Debug)]
struct Arrival {
    at_us: u64,
    bytes: usize,
    keyframe: bool,
}

#[derive(Clone, Debug)]
struct Schedule {
    bytes_per_sec: f64,
    /// The tunnel's socket buffering, between the link's writer and the iPad.
    buffer: usize,
    /// When the tunnel stands still and for how long, in µs: sorted and
    /// apart.
    pauses: Vec<(u64, u64)>,
    frames: Vec<Arrival>,
    /// The size of a keyframe the link asks for. The encoder makes the next
    /// frame one, as the pipeline does.
    requested_keyframe: usize,
}

#[derive(Debug, PartialEq)]
struct Run {
    /// Frames whose every fragment reached the iPad, in order.
    delivered: Vec<u32>,
    dropped: u64,
    congestion_drops: u64,
    /// Frames that became keyframes because the link asked for one.
    requested: u64,
}

fn datagrams(seq: u32, arrival: Arrival) -> Vec<Vec<u8>> {
    let payload = MAX_DGRAM_SIZE - MEDIA_HEADER_SIZE;
    let count = arrival.bytes.div_ceil(payload);
    (0..count)
        .map(|index| {
            let len = payload.min(arrival.bytes - index * payload);
            let mut bytes = vec![0xAC; MEDIA_HEADER_SIZE + len];
            MediaHeader {
                session_id: 1,
                stream_epoch: 1,
                frame_seq: seq,
                frag_index: index as u16,
                frag_count: count as u16,
                is_keyframe: arrival.keyframe,
                is_retransmit: false,
                capture_ts_us: arrival.at_us,
                payload_len: len as u16,
            }
            .encode_into(&mut bytes);
            bytes
        })
        .collect()
}

enum Sender {
    Head(FramedLink),
    Base(BaseLink),
}

async fn run(schedule: &Schedule, head: bool) -> Run {
    let (host, mut ipad) = tokio::io::duplex(schedule.buffer);
    let peer = PeerId::usb(9);
    let force_idr = Arc::new(AtomicBool::new(false));
    let sender = if head {
        let link = FramedLink::connect(host, peer, Arc::clone(&force_idr))
            .await
            .unwrap();
        read_preamble(&mut ipad).await.unwrap();
        Sender::Head(link)
    } else {
        Sender::Base(BaseLink::start(tokio::io::split(host).1))
    };
    let start = Instant::now();

    // The tunnel model of `usb_link_sim.rs`: a fixed drain rate whose clock
    // restarts only after the tunnel sat idle. While it stands still it
    // drains nothing, and the link's writes block once its buffer is full.
    let bytes_per_sec = schedule.bytes_per_sec;
    let at = move |us: u64| start + Duration::from_micros(us);
    let mut pauses = schedule
        .pauses
        .iter()
        .map(|&(from, len)| (at(from), at(from + len)))
        .collect::<VecDeque<_>>();
    let (arrived, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
    let tunnel = tokio::spawn(async move {
        let mut drained_until = start;
        loop {
            let waiting_since = Instant::now();
            let Ok(bytes) = read_datagram(&mut ipad).await else {
                return;
            };
            if Instant::now() > waiting_since {
                drained_until = drained_until.max(Instant::now());
            }
            // A datagram starts to drain after a stand-still it meets, and
            // one that begins while it drains holds it up for its length.
            while let Some(&(from, until)) = pauses.front() {
                if until <= drained_until {
                    pauses.pop_front();
                } else if from <= drained_until {
                    drained_until = until;
                    pauses.pop_front();
                } else {
                    break;
                }
            }
            drained_until += Duration::from_secs_f64((bytes.len() + 2) as f64 / bytes_per_sec);
            while let Some(&(from, until)) = pauses.front() {
                if from >= drained_until {
                    break;
                }
                drained_until += until - from;
                pauses.pop_front();
            }
            tokio::time::sleep_until(drained_until).await;
            if arrived.send(bytes).is_err() {
                return;
            }
        }
    });

    let (mut total, mut requested) = (0, 0);
    for (index, &arrival) in schedule.frames.iter().enumerate() {
        tokio::time::sleep_until(start + Duration::from_micros(arrival.at_us)).await;
        let asked = match &sender {
            Sender::Head(_) => force_idr.swap(false, Ordering::SeqCst),
            Sender::Base(link) => link.take_keyframe_request(),
        };
        let arrival = if asked && !arrival.keyframe {
            requested += 1;
            Arrival {
                bytes: schedule.requested_keyframe,
                keyframe: true,
                ..arrival
            }
        } else {
            arrival
        };
        for datagram in datagrams(index as u32 + 1, arrival) {
            total += datagram.len() + 2;
            match &sender {
                Sender::Head(link) => link.send(peer, &datagram).await.unwrap(),
                Sender::Base(link) => link.send(&datagram).unwrap(),
            }
        }
    }
    // Everything still queued drains, with time to spare.
    if let Some(&(from, len)) = schedule.pauses.last() {
        tokio::time::sleep_until(at(from + len)).await;
    }
    tokio::time::sleep(Duration::from_secs_f64(total as f64 / bytes_per_sec) * 2).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (dropped, congestion_drops) = match &sender {
        Sender::Head(link) => {
            let stats = link.stats();
            (
                stats.frames_dropped.load(Ordering::Relaxed),
                stats.congestion_drops.load(Ordering::Relaxed),
            )
        }
        Sender::Base(link) => (link.frames_dropped(), link.frames_dropped()),
    };
    drop(sender);
    tunnel.abort();

    let mut delivered = Vec::new();
    let mut next = None;
    while let Ok(bytes) = arrivals.try_recv() {
        let header = MediaHeader::decode(&bytes).unwrap().0;
        if header.frag_index == 0 {
            next = Some((header.frame_seq, 0));
        }
        if next != Some((header.frame_seq, header.frag_index)) {
            next = None;
            continue;
        }
        next = Some((header.frame_seq, header.frag_index + 1));
        if header.frag_index + 1 == header.frag_count {
            delivered.push(header.frame_seq);
        }
    }
    Run {
        delivered,
        dropped,
        congestion_drops,
        requested,
    }
}

/// SplitMix64: small, seedable, and the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn log_uniform(&mut self, low: f64, high: f64) -> f64 {
        (low.ln() + self.unit() * (high.ln() - low.ln())).exp()
    }
}

const MIN_FRAME: usize = 2_000;
const MAX_FRAME: usize = 300_000;

/// Encoder output as it reaches the link: a frame rate with jitter, bursts
/// of frames within a millisecond, long gaps of a still desktop, periodic
/// and requested keyframes, and deltas from tiny to keyframe-sized. How often
/// each happens varies by schedule, so that some are mostly bursts and some
/// mostly keyframes.
fn schedule(rng: &mut Rng) -> Schedule {
    let bytes_per_sec = rng.log_uniform(300_000.0, 20_000_000.0);
    let buffer = [4096, 16 * 1024, 64 * 1024][rng.below(3) as usize];
    let interval = [8_333, 16_667, 33_333][rng.below(3) as usize];
    let gop = 5 + rng.below(56) as usize;
    let keyframe_bytes = rng.log_uniform(20_000.0, MAX_FRAME as f64);
    let delta_bytes = rng.log_uniform(MIN_FRAME as f64, 60_000.0);
    let burst = rng.unit() * 0.5;
    let long_gap = rng.unit() * 0.3;
    let requested = rng.unit() * 0.4;
    let large_delta = rng.unit() * 0.3;
    let count = 3 + rng.below(28) as usize;
    let mut at_us = 0;
    let frames: Vec<Arrival> = (0..count)
        .map(|index| {
            if index > 0 {
                let gap = rng.unit();
                at_us += if gap < burst {
                    rng.below(1_000)
                } else if gap < burst + long_gap {
                    rng.log_uniform(20_000.0, 1_500_000.0) as u64
                } else {
                    interval / 2 + rng.below(interval * 3 / 2)
                };
            }
            let keyframe = index % gop == 0 || rng.unit() < requested;
            let bytes = if keyframe {
                keyframe_bytes * (0.7 + 0.6 * rng.unit())
            } else if rng.unit() < large_delta {
                rng.log_uniform(MIN_FRAME as f64, MAX_FRAME as f64)
            } else {
                delta_bytes * (0.5 + rng.unit())
            };
            Arrival {
                at_us,
                bytes: (bytes as usize).clamp(MIN_FRAME, MAX_FRAME),
                keyframe,
            }
        })
        .collect();
    Schedule {
        bytes_per_sec,
        buffer,
        pauses: stand_stills(rng, &frames),
        frames,
        requested_keyframe: keyframe_bytes as usize,
    }
}

/// Stand-stills of 50–500 ms in half the schedules. Half of them begin just
/// as a frame arrives, often one that a still desktop leaves alone for a
/// while; the rest anywhere in the schedule.
fn stand_stills(rng: &mut Rng, frames: &[Arrival]) -> Vec<(u64, u64)> {
    if rng.unit() < 0.5 {
        return Vec::new();
    }
    let sparse: Vec<&Arrival> = frames
        .iter()
        .zip(frames.iter().skip(1).map(Some).chain([None]))
        .filter(|(frame, next)| next.is_none_or(|next| next.at_us - frame.at_us >= 50_000))
        .map(|(frame, _)| frame)
        .collect();
    let end = frames.last().map_or(0, |last| last.at_us) + 200_000;
    let mut pauses: Vec<(u64, u64)> = (0..1 + rng.below(3))
        .map(|_| {
            let from = if rng.unit() < 0.5 {
                rng.below(end)
            } else {
                let frame = if rng.unit() < 0.5 {
                    sparse[rng.below(sparse.len() as u64) as usize]
                } else {
                    &frames[rng.below(frames.len() as u64) as usize]
                };
                frame.at_us + rng.below(20_000)
            };
            (from, rng.log_uniform(50_000.0, 500_000.0) as u64)
        })
        .collect();
    pauses.sort_unstable();
    let mut apart: Vec<(u64, u64)> = Vec::new();
    for (from, len) in pauses {
        match apart.last_mut() {
            Some((last, last_len)) if from <= *last + *last_len => {
                *last_len = (*last_len).max(from + len - *last);
            }
            _ => apart.push((from, len)),
        }
    }
    apart
}

/// Encoder output as the field sees it: 30, 60 or 120 fps within ±15%,
/// 10–20 Mbps of deltas, 100–300 KB keyframes every 0.5–2 s, and others in
/// between that the iPad or the link asked for, into a 1–3 MB/s tunnel that
/// also stands still.
fn realistic(rng: &mut Rng) -> Schedule {
    let fps = [30.0, 60.0, 120.0][rng.below(3) as usize];
    let interval = 1_000_000.0 / fps;
    let delta_bytes = rng.log_uniform(10_000_000.0, 20_000_000.0) / 8.0 / fps;
    let keyframe_every = rng.log_uniform(500_000.0, 2_000_000.0) as u64;
    let keyframe_bytes = rng.log_uniform(100_000.0, MAX_FRAME as f64);
    let requested = rng.unit() * 0.03;
    let count = 60 + rng.below(181) as usize;
    let (mut at_us, mut next_keyframe) = (0, 0);
    let frames: Vec<Arrival> = (0..count)
        .map(|index| {
            if index > 0 {
                at_us += (interval * (0.85 + 0.3 * rng.unit())) as u64;
            }
            let keyframe = at_us >= next_keyframe || rng.unit() < requested;
            if keyframe {
                next_keyframe = at_us + keyframe_every;
            }
            let bytes = if keyframe {
                (keyframe_bytes * (0.85 + 0.3 * rng.unit())).clamp(100_000.0, MAX_FRAME as f64)
            } else {
                delta_bytes * (0.5 + rng.unit())
            };
            Arrival {
                at_us,
                bytes: (bytes as usize).clamp(MIN_FRAME, MAX_FRAME),
                keyframe,
            }
        })
        .collect();
    Schedule {
        bytes_per_sec: rng.log_uniform(1_000_000.0, 3_000_000.0),
        buffer: [4096, 16 * 1024, 64 * 1024][rng.below(3) as usize],
        pauses: stand_stills(rng, &frames),
        frames,
        requested_keyframe: keyframe_bytes as usize,
    }
}

/// Runs `count` schedules from `generator` through both links. Wherever the
/// base queue loses nothing, the new one loses nothing, counts no congestion,
/// and delivers the same frames in the same order. Returns how many
/// schedules the base queue ran clean.
async fn differential(
    label: &str,
    seed: u64,
    count: u64,
    generator: fn(&mut Rng) -> Schedule,
) -> u64 {
    let mut rng = Rng(seed);
    let (mut clean, mut base_lost, mut head_lost_more, mut head_delivered_less) = (0, 0, 0, 0);
    let (mut paused, mut base_frames, mut head_frames) = (0, 0, 0);
    for case in 0..count {
        let schedule = generator(&mut rng);
        paused += u64::from(!schedule.pauses.is_empty());
        let base = run(&schedule, false).await;
        let head = run(&schedule, true).await;
        base_frames += base.delivered.len();
        head_frames += head.delivered.len();
        if base.dropped > 0 {
            base_lost += 1;
            head_lost_more += u64::from(head.dropped > base.dropped);
            head_delivered_less += u64::from(head.delivered.len() < base.delivered.len());
            continue;
        }
        clean += 1;
        let every_frame: Vec<u32> = (1..=schedule.frames.len() as u32).collect();
        assert_eq!(
            base.delivered, every_frame,
            "{label} case {case}: {schedule:?}"
        );
        assert_eq!(
            head,
            Run {
                delivered: every_frame,
                dropped: 0,
                congestion_drops: 0,
                requested: 0,
            },
            "{label} case {case}: {schedule:?}"
        );
    }
    eprintln!(
        "LINK_DIFF {label} schedules={count} with_stand_stills={paused} base_clean={clean} \
         base_lost={base_lost} head_lost_more_than_base={head_lost_more} \
         head_delivered_fewer_than_base={head_delivered_less} \
         frames_delivered base={base_frames} head={head_frames}"
    );
    clean
}

fn schedules(variable: &str, default: u64) -> u64 {
    std::env::var(variable)
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(default)
}

/// Schedules that stress the queue's rules: bursts, still-desktop gaps,
/// keyframes and large deltas, in proportions that vary by schedule.
/// `EM_LINK_SCHEDULES` runs more from the same seed.
#[tokio::test(start_paused = true)]
async fn the_queue_never_loses_a_frame_the_base_queue_delivered() {
    let count = schedules("EM_LINK_SCHEDULES", 4_000);
    let clean = differential("stress", 0x0E7E_5A1D_0118_0034, count, schedule).await;
    assert!(
        clean * 4 >= count,
        "too few schedules test anything: {clean}"
    );
}

/// Streams as the field sends them, through tunnels that carry them or
/// almost do. The base queue overflows behind most keyframes this size, so
/// only about one schedule in seventeen tests anything.
/// `EM_LINK_REALISTIC_SCHEDULES` runs more from the same seed.
#[tokio::test(start_paused = true)]
async fn realistic_streams_never_lose_a_frame_the_base_queue_delivered() {
    let count = schedules("EM_LINK_REALISTIC_SCHEDULES", 400);
    let clean = differential("realistic", 0x0E7E_5A1D_0118_0035, count, realistic).await;
    assert!(
        clean * 25 >= count,
        "too few schedules test anything: {clean}"
    );
}

/// Round 2's reproduction. A 120 KB delta is being written through a 4 KB
/// buffer at 1 MB/s; frame 3 arrives 1 ms after it and has waited 34 ms when
/// keyframe 4 arrives. The tunnel is moving and the keyframe fits, so frame
/// 3 still goes out. The cadence estimate took the 1 ms gap for the frame
/// rate, replaced frame 3, and counted it as congestion.
#[tokio::test(start_paused = true)]
async fn bursty_arrivals_on_a_healthy_link_keep_every_frame() {
    let frame = |at_ms: u64, bytes, keyframe| Arrival {
        at_us: at_ms * 1000,
        bytes,
        keyframe,
    };
    let schedule = Schedule {
        bytes_per_sec: 1_000_000.0,
        buffer: 4096,
        pauses: Vec::new(),
        requested_keyframe: 60_000,
        frames: vec![
            frame(0, 6_000, true),
            frame(65, 120_000, false),
            frame(66, 6_000, false),
            frame(100, 60_000, true),
        ],
    };
    let expected = Run {
        delivered: vec![1, 2, 3, 4],
        dropped: 0,
        congestion_drops: 0,
        requested: 0,
    };
    assert_eq!(run(&schedule, false).await, expected, "base");
    assert_eq!(run(&schedule, true).await, expected);
}

/// Round 3's reproduction. Keyframe 1, 11 KB, is being written through a
/// 4 KB buffer when the tunnel stands still at 1 ms; keyframe 2, 3 KB,
/// arrives during or before the stand-still. Both still go out. Dropping
/// everything after a write that had stood still for 100 ms threw away
/// keyframe 2, which needed nothing, with the rest of keyframe 1, and a
/// still desktop had nothing newer to send.
async fn stand_still(pause_ms: u64, keyframe_at_ms: u64) {
    let frame = |at_ms: u64, bytes| Arrival {
        at_us: at_ms * 1000,
        bytes,
        keyframe: true,
    };
    let schedule = Schedule {
        bytes_per_sec: 1_000_000.0,
        buffer: 4096,
        pauses: vec![(1_000, pause_ms * 1000)],
        requested_keyframe: 11_000,
        frames: vec![frame(0, 11_000), frame(keyframe_at_ms, 3_000)],
    };
    let expected = Run {
        delivered: vec![1, 2],
        dropped: 0,
        congestion_drops: 0,
        requested: 0,
    };
    assert_eq!(run(&schedule, false).await, expected, "base");
    assert_eq!(run(&schedule, true).await, expected);
}

#[tokio::test(start_paused = true)]
async fn a_150_ms_stand_still_keeps_the_keyframe_that_arrived_at_120_ms() {
    stand_still(150, 120).await;
}

#[tokio::test(start_paused = true)]
async fn a_300_ms_stand_still_keeps_the_keyframe_that_arrived_at_250_ms() {
    stand_still(300, 250).await;
}

#[tokio::test(start_paused = true)]
async fn a_300_ms_stand_still_keeps_the_keyframe_that_arrived_at_50_ms() {
    stand_still(300, 50).await;
}

#[tokio::test(start_paused = true)]
async fn a_90_ms_stand_still_keeps_the_keyframe_that_arrived_at_50_ms() {
    stand_still(90, 50).await;
}
