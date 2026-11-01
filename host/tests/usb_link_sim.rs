//! Paused-clock simulation of a slow USB tunnel, as usbmuxd behaves on an
//! older PC. A fake encoder feeds the real `FramedLink`, the tunnel drains at
//! a fixed rate, and a fake iPad reassembles with the app's rules over USB
//! (no repair) and asks for a keyframe when it drops a partial frame.
//!
//! Every run is deterministic: the tokio clock only moves when every task
//! waits, so a busy machine cannot change the result.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eternal_host::transport::abr::AbrController;
use eternal_host::transport::link::{read_datagram, read_preamble, FramedLink, Link, PeerId};
use eternal_wire::reassembly::{AddOutcome, Reassembler};
use eternal_wire::v2::control::ReceiverReport;
use eternal_wire::v2::media::{MediaHeader, MEDIA_HEADER_SIZE};
use tokio::time::Instant;

const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const DATAGRAM: usize = 1400;

#[derive(Clone, Copy)]
struct Stream {
    gop: u64,
    keyframe_bytes: usize,
    delta_bytes: usize,
    /// The tunnel's socket buffering, between the link's writer and the iPad.
    buffer: usize,
    /// The frame before each periodic keyframe reaches the link in the same
    /// batch, before its writer runs, as a busy runtime can schedule them.
    batch_before_keyframe: bool,
}

/// The pipeline's natural keyframe interval (`set_gop(30)`), with keyframes
/// ten times the size of a delta frame.
const DESKTOP: Stream = Stream {
    gop: 30,
    keyframe_bytes: 60_000,
    delta_bytes: 6_000,
    buffer: 64 * 1024,
    batch_before_keyframe: false,
};
/// The host grants one client keyframe request per 500 ms, and the iPad
/// asks at most that often.
const REQUEST_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
struct Outcome {
    encoded: u64,
    forced_keyframes: u64,
    gap_requests: u64,
    host_drops: u64,
    /// The drops that showed the tunnel falling behind: ABR's signal.
    congestion_drops: u64,
    ipad_dropped: u64,
    ipad_frags_lost: u64,
    decodable: u64,
    longest_freeze_ms: u128,
    /// Where ABR left the bitrate, fed the tunnel's drops every 500 ms.
    final_bps: u32,
}

fn frame(stream: Stream, seq: u32, keyframe: bool) -> Vec<Vec<u8>> {
    let size = if keyframe {
        stream.keyframe_bytes
    } else {
        stream.delta_bytes
    };
    let payload = DATAGRAM - MEDIA_HEADER_SIZE;
    let count = size.div_ceil(payload) as u16;
    (0..count)
        .map(|index| {
            let mut bytes = vec![0xAC; DATAGRAM];
            MediaHeader {
                session_id: 1,
                stream_epoch: 1,
                frame_seq: seq,
                frag_index: index,
                frag_count: count,
                is_keyframe: keyframe,
                is_retransmit: false,
                capture_ts_us: 0,
                payload_len: payload as u16,
            }
            .encode_into(&mut bytes);
            bytes
        })
        .collect()
}

async fn simulate(bytes_per_sec: f64, run_for: Duration) -> Outcome {
    simulate_stream(DESKTOP, bytes_per_sec, run_for).await
}

async fn simulate_stream(stream: Stream, bytes_per_sec: f64, run_for: Duration) -> Outcome {
    let (host, mut ipad) = tokio::io::duplex(stream.buffer);
    let force_idr = Arc::new(AtomicBool::new(false));
    let client_request = Arc::new(AtomicBool::new(false));
    let peer = PeerId::usb(9);
    let link = FramedLink::connect(host, peer, Arc::clone(&force_idr))
        .await
        .unwrap();
    read_preamble(&mut ipad).await.unwrap();
    let stats = link.stats();
    let start = Instant::now();

    let encoder_request = Arc::clone(&client_request);
    let encoder = tokio::spawn(async move {
        let (mut encoded, mut forced) = (0u64, 0u64);
        let mut last_grant: Option<Instant> = None;
        while start.elapsed() < run_for {
            let now = Instant::now();
            let granted = encoder_request.swap(false, Ordering::SeqCst)
                && last_grant.is_none_or(|at| now - at >= REQUEST_INTERVAL);
            if granted {
                last_grant = Some(now);
            }
            let natural = encoded.is_multiple_of(stream.gop);
            let requested = force_idr.swap(false, Ordering::SeqCst) || granted;
            if requested && !natural {
                forced += 1;
            }
            for datagram in frame(stream, encoded as u32 + 1, natural || requested) {
                link.send(peer, &datagram).await.unwrap();
            }
            encoded += 1;
            if stream.batch_before_keyframe && encoded.is_multiple_of(stream.gop) {
                continue;
            }
            tokio::time::sleep_until(start + FRAME_INTERVAL * (encoded as u32)).await;
        }
        (encoded, forced, link)
    });

    // The tunnel drains at a fixed rate. Its clock restarts only after it sat
    // idle; restarting it after every read would add the timer's rounding
    // to each datagram and silently slow the tunnel down.
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
            drained_until += Duration::from_secs_f64((bytes.len() + 2) as f64 / bytes_per_sec);
            tokio::time::sleep_until(drained_until).await;
            if arrived.send(bytes).is_err() {
                return;
            }
        }
    });

    let origin = std::time::Instant::now();
    let clock = |at: Instant| origin + (at - start);
    let mut reassembler = Reassembler::new();
    let mut outcome = Outcome::default();
    let mut last_request: Option<Instant> = None;
    let mut decoded_seq = None;
    let mut decoded_at = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(5));
    let mut abr = AbrController::new(20_000_000, true);
    let mut report = ReceiverReport::default();
    let mut reports = tokio::time::interval(Duration::from_millis(500));
    while start.elapsed() < run_for {
        let bytes = tokio::select! {
            bytes = arrivals.recv() => bytes.unwrap(),
            _ = tick.tick() => {
                reassembler.tick(clock(Instant::now()));
                continue;
            }
            _ = reports.tick() => {
                report.frags_received = reassembler.counters().frags_received as u32;
                let drops = stats.congestion_drops.load(Ordering::Relaxed);
                abr.on_report(&report, Some(drops), clock(Instant::now()));
                continue;
            }
        };
        let now = Instant::now();
        let (header, payload) = MediaHeader::decode(&bytes).unwrap();
        if let AddOutcome::Completed(_) = reassembler.add_media(header, payload, clock(now)) {
            // A delta frame decodes only right after a frame that decoded.
            let decodes = header.is_keyframe || decoded_seq == Some(header.frame_seq - 1);
            decoded_seq = decodes.then_some(header.frame_seq);
            if decodes {
                decoded_at.push(now - start);
            }
        }
        reassembler.tick(clock(now));
        if reassembler.take_keyframe_request().is_some()
            && last_request.is_none_or(|at| now - at >= REQUEST_INTERVAL)
        {
            last_request = Some(now);
            outcome.gap_requests += 1;
            client_request.store(true, Ordering::SeqCst);
        }
    }
    let (encoded, forced, link) = encoder.await.unwrap();
    drop(link);
    tunnel.abort();
    outcome.encoded = encoded;
    outcome.forced_keyframes = forced;
    outcome.host_drops = stats.frames_dropped.load(Ordering::Relaxed);
    outcome.congestion_drops = stats.congestion_drops.load(Ordering::Relaxed);
    let counters = reassembler.counters();
    outcome.ipad_dropped = counters.frames_dropped;
    outcome.ipad_frags_lost = counters.frags_lost;
    outcome.decodable = decoded_at.len() as u64;
    outcome.final_bps = abr.current_bps();
    outcome.longest_freeze_ms = decoded_at
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_millis())
        .chain(decoded_at.last().map(|last| (run_for - *last).as_millis()))
        .max()
        .unwrap_or(run_for.as_millis());
    outcome
}

/// The stream averages 0.53 MB/s on the wire; each keyframe is a 62 KB
/// burst. A tunnel that drains 0.55 MB/s carries the average but needs
/// 112 ms, almost seven frame intervals, for a keyframe. 0.3.1 dropped the
/// queue behind each keyframe, cut the keyframe being written, and asked for
/// another one: no frame decoded for the whole run.
#[tokio::test(start_paused = true)]
async fn slow_tunnel_carries_keyframe_bursts_without_a_spiral() {
    let outcome = simulate(550_000.0, Duration::from_secs(20)).await;
    eprintln!("USB_SIM rate=550000 {outcome:?}");
    assert_eq!(
        outcome.ipad_frags_lost, 0,
        "a lossless tunnel loses nothing"
    );
    assert_eq!(outcome.gap_requests, 0);
    assert!(
        outcome.decodable * 100 >= outcome.encoded * 95,
        "at least 95% of frames decode: {outcome:?}"
    );
    assert!(outcome.longest_freeze_ms <= 250, "{outcome:?}");
    assert!(
        outcome.final_bps >= 15_000_000,
        "a tunnel that keeps up must not cut the bitrate: {outcome:?}"
    );
}

/// A tunnel slower than the stream itself must lose frames, but only whole
/// frames, with keyframe requests spaced out and the picture still moving.
#[tokio::test(start_paused = true)]
async fn tunnel_slower_than_the_stream_backs_off_its_keyframe_requests() {
    let outcome = simulate(400_000.0, Duration::from_secs(20)).await;
    eprintln!("USB_SIM rate=400000 {outcome:?}");
    assert_eq!(outcome.ipad_frags_lost, 0, "only whole frames are dropped");
    assert_eq!(outcome.gap_requests, 0);
    assert!(outcome.forced_keyframes <= 20, "{outcome:?}");
    assert!(outcome.decodable * 5 >= outcome.encoded * 2, "{outcome:?}");
    assert!(outcome.longest_freeze_ms <= 1000, "{outcome:?}");
}

/// Prints the measurement table used in the release report.
#[tokio::test(start_paused = true)]
async fn usb_tunnel_rates_table() {
    for rate in [2_000_000.0, 1_000_000.0, 700_000.0, 550_000.0, 400_000.0] {
        let outcome = simulate(rate, Duration::from_secs(20)).await;
        eprintln!("USB_SIM rate={rate} {outcome:?}");
    }
}

/// A stream a little faster than the tunnel, with frequent keyframes: each
/// keyframe clears the backlog before the queue overflows, so nothing was
/// ever counted as dropped and ABR raised the bitrate while a quarter of
/// the frames never reached the iPad.
#[tokio::test(start_paused = true)]
async fn keyframes_that_clear_a_backlog_still_step_the_bitrate_down() {
    let stream = Stream {
        gop: 10,
        keyframe_bytes: 30_000,
        delta_bytes: 6_000,
        ..DESKTOP
    };
    let outcome = simulate_stream(stream, 450_000.0, Duration::from_secs(20)).await;
    eprintln!("USB_SIM backlog {outcome:?}");
    // Every frame that never arrived is counted, but for the few still in
    // the queue and the tunnel's buffer when the run ends.
    let missing = outcome.encoded - outcome.decodable;
    assert!(
        outcome.host_drops <= missing && missing - outcome.host_drops <= 16,
        "{outcome:?}"
    );
    assert!(outcome.final_bps < 15_000_000, "{outcome:?}");
}

/// A tunnel with room to spare, where the frame before each keyframe reaches
/// the link just before the keyframe, before the writer has run. The tunnel
/// is not behind: that frame still goes out, and the bitrate must not come
/// down. Counting every replacement as congestion stepped this stream down
/// to the 4 Mbps floor in 13 s, and replacing the frame lost its picture.
#[tokio::test(start_paused = true)]
async fn a_frame_replaced_before_the_writer_ran_does_not_cut_the_bitrate() {
    let stream = Stream {
        buffer: 1024 * 1024,
        batch_before_keyframe: true,
        ..DESKTOP
    };
    let outcome = simulate_stream(stream, 10_000_000.0, Duration::from_secs(20)).await;
    eprintln!("USB_SIM healthy-batched {outcome:?}");
    assert_eq!(outcome.host_drops, 0, "no picture is lost");
    assert_eq!(outcome.decodable, outcome.encoded, "{outcome:?}");
    assert_eq!(outcome.congestion_drops, 0);
    assert!(outcome.final_bps >= 15_000_000, "{outcome:?}");
}
