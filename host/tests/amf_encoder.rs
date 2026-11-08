//! The real encode stage under AMD's AMF encoder, fed synthetic frames and
//! checked by decoding its output with FFmpeg's software decoders.
//!
//! `ETERNAL_AMF_TEST` says what the AMF cases must prove on this machine:
//! - unset: whatever the linked FFmpeg allows. Skips when it has no AMF.
//! - `hardware`: AMF must open. Use this on a PC with an AMD GPU.
//! - `fallback`: AMF is compiled in but cannot open, so the stage must fall
//!   back to libx264. CI's Windows runner has no AMD driver.
//!
//! The mode-change cases also run on libx264 everywhere. Radeon Super
//! Resolution, Virtual Super Resolution and GPU scaling all change the
//! desktop mode under a running stream.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eternal_host::capture::synthetic::{decode_frame_counter_from_luma, render_synthetic_frame};
use eternal_host::capture::{frame_buffer, frame_slot, RawFrame};
use eternal_host::control::SharedControl;
use eternal_host::encoder::{
    forget_launch_failures, run_encode_stage, NALUnit, NAL_CHANNEL_CAPACITY,
};
use eternal_host::gpu::GpuInfo;
use eternal_host::stats::PIPELINE_STATS;
use eternal_wire::h264::{contains_nal_type, nal_type, parse_annex_b_units};
use ffmpeg_next::codec::Id;

/// The encode stage's state is process-wide: PIPELINE_STATS, the encoders
/// that failed to open this launch, and the lane FFmpeg's hardware opens
/// share. So is an AMD driver's limit on concurrent AMF sessions. Each test
/// holds [`Serial`] from its first line, through its AMF probes and stages.
static STAGE_LOCK: Mutex<()> = Mutex::new(());

/// One test's turn at the encoders. A stage and an AMF probe take it by
/// reference, so neither runs beside another test's, and PIPELINE_STATS is
/// read only for the stage that just ran.
struct Serial(#[allow(dead_code)] MutexGuard<'static, ()>);

/// Waits for this test's turn, then starts it from a clean launch: no
/// earlier test's encoder open is still running, and no earlier failure
/// keeps an encoder out of this test's stages.
fn serial() -> Serial {
    let guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    wait_for_clear_lane();
    forget_launch_failures();
    Serial(guard)
}

/// An open that went on in the background (see `encoder::Opening`) holds
/// the encoders' lane until it returns, and a stage's open in that lane is
/// refused meanwhile.
fn lane_busy() -> bool {
    eternal_host::stall::abandoned(eternal_host::stall::Call::EncoderOpen) > 0
}

fn wait_for_clear_lane() {
    // Past the 15 s limit of the slowest open in the lane.
    let deadline = Instant::now() + Duration::from_secs(30);
    while lane_busy() {
        assert!(
            Instant::now() < deadline,
            "an earlier encoder open is still running"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A stage whose hardware open outlasted the first picture's wait starts on
/// libx264 while the open goes on in the background, as it should when a
/// driver is slow. On a busy test PC that says nothing about what a test
/// checks, so the stage runs once more.
fn started_on_the_fallback(_: &Serial) -> bool {
    PIPELINE_STATS.lock().using_software_fallback && lane_busy()
}

const STAGE_TRIES: u32 = 2;

/// Before a stage runs again: its open in the background is released, and
/// what it remembered is forgotten, as for a new test.
fn before_another_try(encoder: &str) {
    eprintln!(
        "{encoder} was still opening when the first picture was due; running the stage again"
    );
    wait_for_clear_lane();
    forget_launch_failures();
}

const FRAME_INTERVAL: Duration = Duration::from_millis(20);
const FRAME_COUNT: u64 = 90;
/// Mid-GOP. The encoder's own IDRs land on frames 0, 30 and 60.
const KEYFRAME_REQUEST_AT: u64 = 40;
/// How far a requested keyframe's picture may trail the request. The encoder
/// takes the picture before any session reopen, so reopen time does not count.
/// Anything inside this window still beats the next GOP IDR.
const KEYFRAME_WINDOW: u64 = 5;
const SMALL: (u32, u32) = (640, 360);
const LARGE: (u32, u32) = (1280, 720);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmfExpectation {
    Any,
    Hardware,
    Fallback,
}

/// What this run must prove about AMF, or `None` to skip.
fn amf_expectation() -> Option<AmfExpectation> {
    ffmpeg_next::init().unwrap();
    let compiled = ffmpeg_next::encoder::find_by_name("h264_amf").is_some();
    let mode = std::env::var("ETERNAL_AMF_TEST").unwrap_or_default();
    let expectation = match mode.trim() {
        "" => AmfExpectation::Any,
        "hardware" => AmfExpectation::Hardware,
        "fallback" => AmfExpectation::Fallback,
        other => panic!("unknown ETERNAL_AMF_TEST={other}; use hardware or fallback"),
    };
    if compiled {
        Some(expectation)
    } else {
        assert_eq!(
            expectation,
            AmfExpectation::Any,
            "ETERNAL_AMF_TEST={mode} needs an FFmpeg build with h264_amf"
        );
        eprintln!("skipping: the linked FFmpeg has no h264_amf");
        None
    }
}

/// Whether an AMF encoder opens on this machine. Cases that need a working
/// AMF skip when it does not, except under `ETERNAL_AMF_TEST=hardware`.
fn amf_available(serial: &Serial, name: &str) -> bool {
    let Some(expectation) = amf_expectation() else {
        return false;
    };
    let opens = amf_opens(serial, name);
    if expectation == AmfExpectation::Hardware {
        assert!(
            opens,
            "{name} did not open; check the AMD driver and amfrt64.dll"
        );
    }
    if !opens {
        eprintln!("skipping: {name} does not open on this machine");
    }
    opens
}

fn amf_opens(_: &Serial, name: &str) -> bool {
    let Some(codec) = ffmpeg_next::encoder::find_by_name(name) else {
        return false;
    };
    let Ok(mut encoder) = ffmpeg_next::codec::Context::new_with_codec(codec)
        .encoder()
        .video()
    else {
        return false;
    };
    encoder.set_width(LARGE.0);
    encoder.set_height(LARGE.1);
    encoder.set_format(ffmpeg_next::format::Pixel::YUV420P);
    encoder.set_time_base(ffmpeg_next::Rational(1, 60));
    encoder.open().is_ok()
}

struct StageRun {
    packets: Vec<NALUnit>,
    software_fallback: bool,
    codec_name: String,
    /// Pictures the stage gave the encoder again (`keyframe_resubmits`).
    resubmits: u64,
    /// When each frame was published, by frame number.
    published: Vec<Instant>,
}

/// Feeds `FRAME_COUNT` frames through a fresh encode stage at
/// `FRAME_INTERVAL`, sized by `size_of`, and raises a keyframe request just
/// before frame `keyframe_at`. Panics if the stage fails or panics.
fn run_stage(
    serial: &Serial,
    encoder: &str,
    size_of: impl Fn(u64) -> (u32, u32),
    keyframe_at: Option<u64>,
) -> StageRun {
    for _ in 1..STAGE_TRIES {
        match run_stage_once(serial, encoder, &size_of, keyframe_at) {
            Some(run) => return run,
            None => before_another_try(encoder),
        }
    }
    run_stage_once(serial, encoder, &size_of, keyframe_at)
        .unwrap_or_else(|| panic!("{encoder} was still opening when the first picture was due"))
}

/// [`run_stage`] once. None if the stage started on the fallback while its
/// encoder was still opening (see [`started_on_the_fallback`]).
fn run_stage_once(
    serial: &Serial,
    encoder: &str,
    size_of: impl Fn(u64) -> (u32, u32),
    keyframe_at: Option<u64>,
) -> Option<StageRun> {
    ffmpeg_next::init().unwrap();
    let (producer, consumer) = frame_slot();
    let (tx, mut rx) = tokio::sync::mpsc::channel(NAL_CHANNEL_CAPACITY);
    let shared = SharedControl::new(0, 8_000_000);
    shared.running.store(true, Ordering::SeqCst);
    let gpu = GpuInfo {
        encoder_name: encoder.to_string(),
        codec_display_name: encoder.to_string(),
        ..GpuInfo::software_fallback()
    };
    let generation = eternal_host::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
    let resubmits_before = PIPELINE_STATS.lock().keyframe_resubmits;
    let stage_shared = shared.clone();
    let stage = std::thread::spawn(move || {
        run_encode_stage(consumer, tx, stage_shared, gpu, generation).map_err(|e| e.to_string())
    });
    // The output channel holds only a few frames, so drain it as we go.
    let (first_tx, first) = std::sync::mpsc::channel();
    let collector = std::thread::spawn(move || {
        let mut packets = Vec::new();
        while let Some(nal) = rx.blocking_recv() {
            if packets.is_empty() {
                let _ = first_tx.send(());
            }
            packets.push(nal);
        }
        packets
    });

    let mut published = Vec::new();
    let mut publish = |number: u64| {
        let (width, height) = size_of(number);
        let mut data = frame_buffer((width * height * 4) as usize);
        render_synthetic_frame(&mut data, width, height, number);
        published.push(Instant::now());
        producer.publish(RawFrame {
            frame_number: number,
            timestamp: Instant::now(),
            data: Arc::new(data),
            width,
            height,
            cached: false,
        });
    };
    // The stage opens its encoder on the first frame. The rest follow once
    // it is out, however long the open took.
    publish(0);
    if first.recv_timeout(Duration::from_secs(20)).is_ok() && started_on_the_fallback(serial) {
        producer.close();
        let _ = stage.join();
        let _ = collector.join();
        return None;
    }
    std::thread::sleep(FRAME_INTERVAL);

    for number in 1..FRAME_COUNT {
        if keyframe_at == Some(number) {
            shared.force_next_idr.store(true, Ordering::SeqCst);
        }
        publish(number);
        std::thread::sleep(FRAME_INTERVAL);
    }
    producer.close();

    match stage.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("{encoder} encode stage failed: {error}"),
        Err(_) => panic!("{encoder} encode stage panicked"),
    }
    let packets = collector.join().unwrap();
    assert!(!packets.is_empty(), "{encoder} produced no packets");
    let stats = PIPELINE_STATS.lock();
    Some(StageRun {
        packets,
        software_fallback: stats.using_software_fallback,
        codec_name: stats.codec_name.clone(),
        resubmits: stats.keyframe_resubmits - resubmits_before,
        published,
    })
}

/// A stream whose frames come every `FRAME_INTERVAL` is never still, so no
/// picture may be given to the encoder again and none encoded twice. A
/// picture may repeat only where the feeder itself stalled right after it,
/// for a frame and a half (the encode loop's wait, see `keyframe_flush_wait`),
/// at most once per 20 ms of that stall, and no more often in all than the
/// stage reports giving pictures again. Everywhere else counters advance.
fn assert_no_duplicate_encodes(run: &StageRun, pictures: &[(u32, u32, u64)]) {
    let counters: Vec<u64> = pictures.iter().map(|picture| picture.2).collect();
    assert!(
        counters.windows(2).all(|pair| pair[0] <= pair[1]),
        "frame counters go back: {counters:?}"
    );
    let mut repeats = std::collections::BTreeMap::<u64, u32>::new();
    for pair in counters.windows(2).filter(|pair| pair[0] == pair[1]) {
        *repeats.entry(pair[0]).or_default() += 1;
    }
    for (&counter, &times) in &repeats {
        let stall = run
            .published
            .get(counter as usize + 1)
            .zip(run.published.get(counter as usize))
            .map(|(next, this)| next.saturating_duration_since(*this));
        let allowed = stall
            .filter(|stall| *stall >= FRAME_INTERVAL * 3 / 2)
            .map_or(0, |stall| (stall.as_millis() / 20) as u32);
        assert!(
            times <= allowed,
            "frame {counter} encoded {} times, though the next frame came {stall:?} \
             after it: {counters:?}",
            times + 1
        );
    }
    let stalled = run
        .published
        .windows(2)
        .any(|pair| pair[1].saturating_duration_since(pair[0]) >= FRAME_INTERVAL * 3 / 2);
    assert!(
        stalled || run.resubmits == 0,
        "{} pictures given again though no frame came later than {FRAME_INTERVAL:?}",
        run.resubmits
    );
    let total: u64 = repeats.values().map(|&times| u64::from(times)).sum();
    assert!(
        total <= run.resubmits,
        "{total} pictures encoded again but {} given again: {counters:?}",
        run.resubmits
    );
}

/// Decodes `packets` with a fresh, strict software decoder and returns each
/// picture's size and frame counter. Any decode error fails the test.
fn decode(codec: Id, packets: &[&NALUnit]) -> Vec<(u32, u32, u64)> {
    let codec = ffmpeg_next::decoder::find(codec).unwrap();
    let mut options = ffmpeg_next::Dictionary::new();
    options.set("err_detect", "explode");
    let mut decoder = ffmpeg_next::codec::Context::new_with_codec(codec)
        .decoder()
        .open_as_with(codec, options)
        .unwrap()
        .video()
        .unwrap();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let mut pictures = Vec::new();
    let mut receive = |decoder: &mut ffmpeg_next::decoder::Video| loop {
        match decoder.receive_frame(&mut frame) {
            Ok(()) => {
                let counter =
                    decode_frame_counter_from_luma(frame.data(0), frame.stride(0), frame.width());
                pictures.push((frame.width(), frame.height(), counter));
            }
            Err(ffmpeg_next::Error::Eof) => break,
            Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
                break;
            }
            Err(error) => panic!("decoder rejected an access unit: {error}"),
        }
    };
    for packet in packets {
        decoder
            .send_packet(&ffmpeg_next::Packet::copy(&packet.data))
            .unwrap_or_else(|error| panic!("packet {} did not decode: {error}", packet.sequence));
        receive(&mut decoder);
    }
    decoder.send_eof().unwrap();
    receive(&mut decoder);
    assert_eq!(pictures.len(), packets.len(), "decoder lost pictures");
    pictures
}

fn assert_h264_keyframe(packet: &NALUnit, what: &str) {
    assert!(packet.is_keyframe, "{what} is not flagged as a keyframe");
    for (kind, name) in [(7, "SPS"), (8, "PPS"), (5, "IDR slice")] {
        assert!(
            contains_nal_type(&packet.data, kind),
            "{what} has no {name}"
        );
    }
}

fn assert_hevc_keyframe(packet: &NALUnit, what: &str) {
    assert!(packet.is_keyframe, "{what} is not flagged as a keyframe");
    assert!(
        eternal_wire::hevc::contains_keyframe(&packet.data),
        "{what} has no IRAP slice"
    );
    assert!(
        eternal_wire::hevc::contains_parameter_sets(&packet.data),
        "{what} lacks VPS/SPS/PPS"
    );
}

/// A keyframe request in the middle of a GOP must produce a keyframe whose
/// picture is at most `KEYFRAME_WINDOW` frames past the request, and a decoder
/// that joins there must decode everything after it.
fn assert_keyframe_on_request(run: &StageRun, codec: Id) {
    let all = decode(codec, &run.packets.iter().collect::<Vec<_>>());
    assert_no_duplicate_encodes(run, &all);
    let keyframes: Vec<u64> = run
        .packets
        .iter()
        .filter(|packet| packet.is_keyframe)
        .map(|packet| packet.sequence)
        .collect();
    let start = run
        .packets
        .iter()
        .position(|packet| packet.is_keyframe && packet.sequence >= KEYFRAME_REQUEST_AT)
        .unwrap_or_else(|| panic!("no keyframe after the request; keyframes at {keyframes:?}"));
    let joined = &run.packets[start];
    match codec {
        Id::H264 => assert_h264_keyframe(joined, "the requested keyframe"),
        _ => assert_hevc_keyframe(joined, "the requested keyframe"),
    }
    let tail: Vec<&NALUnit> = run.packets[start..].iter().collect();
    let pictures = decode(codec, &tail);
    assert!(
        pictures.len() >= 10,
        "only {} pictures decoded",
        pictures.len()
    );
    let counters: Vec<u64> = pictures.iter().map(|picture| picture.2).collect();
    let window = KEYFRAME_REQUEST_AT..=KEYFRAME_REQUEST_AT + KEYFRAME_WINDOW;
    assert!(
        window.contains(&counters[0]),
        "the first keyframe after the request shows frame {}, so the request \
         waited for the encoder's own IDR; keyframes at {keyframes:?}",
        counters[0]
    );
    // Advancing counters across the whole stream: `assert_no_duplicate_encodes`.
}

/// Size schedule for the mode-change runs: small, then large, then small.
fn mode_changes(number: u64) -> (u32, u32) {
    if (30..60).contains(&number) {
        LARGE
    } else {
        SMALL
    }
}

/// After each mode change the stream must restart on a keyframe at the new
/// size that a fresh decoder can join.
fn assert_follows_mode_changes(run: &StageRun, codec: Id) {
    let all = decode(codec, &run.packets.iter().collect::<Vec<_>>());
    assert_no_duplicate_encodes(run, &all);
    for (from, size) in [(30u64, LARGE), (60, SMALL)] {
        let start = run
            .packets
            .iter()
            .position(|packet| packet.sequence >= from)
            .unwrap_or_else(|| panic!("no packets after the mode change at frame {from}"));
        let what = format!("the first packet after the mode change at frame {from}");
        match codec {
            Id::H264 => assert_h264_keyframe(&run.packets[start], &what),
            _ => assert_hevc_keyframe(&run.packets[start], &what),
        }
        let segment: Vec<&NALUnit> = run.packets[start..]
            .iter()
            .take_while(|packet| packet.sequence < from + 30)
            .collect();
        let pictures = decode(codec, &segment);
        assert!(!pictures.is_empty(), "nothing decoded after frame {from}");
        for (width, height, counter) in pictures {
            assert_eq!(
                (width, height),
                size,
                "frame {counter} decoded at the wrong size"
            );
        }
    }
}

#[test]
fn a_repeat_is_excused_only_by_the_stall_right_after_it() {
    // Frames every 20 ms, except a 60 ms stall after frame 10.
    let start = Instant::now();
    let published: Vec<Instant> = (0..60u32)
        .map(|n| {
            start
                + FRAME_INTERVAL * n
                + if n > 10 {
                    FRAME_INTERVAL * 2
                } else {
                    Duration::ZERO
                }
        })
        .collect();
    let run = |resubmits| StageRun {
        packets: Vec::new(),
        software_fallback: false,
        codec_name: String::new(),
        resubmits,
        published: published.clone(),
    };
    let pictures = |repeated: u64| -> Vec<(u32, u32, u64)> {
        (0..60u64)
            .flat_map(|n| std::iter::repeat_n((0, 0, n), if n == repeated { 2 } else { 1 }))
            .collect()
    };
    assert_no_duplicate_encodes(&run(1), &pictures(10));
    for (resubmits, repeated, what) in [
        (1, 50, "a repeat far from the stall"),
        (0, 10, "a repeat the stage did not give again"),
        (1, 9, "a repeat just before the stall"),
    ] {
        let failed = std::panic::catch_unwind(|| {
            assert_no_duplicate_encodes(&run(resubmits), &pictures(repeated))
        })
        .is_err();
        assert!(failed, "{what} was accepted");
    }
    // Pictures given again with no stall at all.
    let steady = StageRun {
        published: (0..60u32).map(|n| start + FRAME_INTERVAL * n).collect(),
        ..run(1)
    };
    let counters: Vec<(u32, u32, u64)> = (0..60u64).map(|n| (0, 0, n)).collect();
    assert!(std::panic::catch_unwind(|| assert_no_duplicate_encodes(&steady, &counters)).is_err());
}

#[test]
fn software_encoder_follows_capture_mode_changes() {
    let serial = serial();
    let run = run_stage(&serial, "libx264", mode_changes, None);
    assert_follows_mode_changes(&run, Id::H264);
}

#[test]
fn software_encoder_honours_keyframe_requests() {
    let serial = serial();
    let run = run_stage(&serial, "libx264", |_| LARGE, Some(KEYFRAME_REQUEST_AT));
    assert!(
        !run.software_fallback,
        "explicit CPU selection is not a fallback"
    );
    assert_eq!(run.codec_name, "H.264 (x264)");
    assert_h264_keyframe(&run.packets[0], "the first packet");
    assert_keyframe_on_request(&run, Id::H264);
}

#[cfg(windows)]
#[test]
#[ignore = "requires the NVIDIA/AMD reference PC without an Intel GPU"]
fn unavailable_quick_sync_uses_another_gpu_and_decodes() {
    let serial = serial();
    let run = run_stage(&serial, "h264_qsv", |_| LARGE, None);
    assert!(!run.software_fallback, "fell straight from QSV to CPU");
    assert_eq!(run.codec_name, "H.264 (NVENC)");
    assert_h264_keyframe(&run.packets[0], "the first packet");
    let pictures = decode(Id::H264, &run.packets.iter().collect::<Vec<_>>());
    assert!(pictures.len() >= 30, "GPU fallback did not sustain video");
    assert!(pictures.windows(2).all(|pair| pair[0].2 < pair[1].2));
    eprintln!(
        "Unavailable QSV recovered through {} and decoded {} packets",
        run.codec_name,
        run.packets.len()
    );
}

/// After a hybrid laptop is switched to Power saving, capture runs on the
/// integrated card while detection still picks NVENC (the card with the most
/// video memory). Capture hands the encoder plain CPU bytes and FFmpeg's
/// NVENC opens its own CUDA context, so the cards never share a device.
/// To prove it, set this test exe to Power saving (`GpuPreference=1;` under
/// `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`) so DXGI runs the
/// process on the iGPU, then run
/// `cargo test -p eternal-host --test amf_encoder nvenc_encodes -- --ignored --nocapture`.
#[cfg(windows)]
#[test]
#[ignore = "native: an NVIDIA card, with this exe on another card"]
fn nvenc_encodes_while_the_process_runs_on_another_card() {
    let serial = serial();
    let facts = eternal_host::gpu_preference::facts().expect("graphics card facts");
    let app_gpu = facts.running_on().expect("a hardware adapter").name.clone();
    let started = Instant::now();
    let run = run_stage(&serial, "h264_nvenc", |_| LARGE, Some(KEYFRAME_REQUEST_AT));
    assert!(!run.software_fallback, "NVENC fell back to the CPU");
    assert!(run.codec_name.contains("NVENC"), "{}", run.codec_name);
    assert_keyframe_on_request(&run, Id::H264);
    eprintln!(
        "process on {app_gpu}: NVENC encoded {} packets in {:?}",
        run.packets.len(),
        started.elapsed()
    );
}

#[test]
fn strict_decoder_rejects_broken_access_units() {
    let serial = serial();
    let run = run_stage(&serial, "libx264", |_| SMALL, None);
    let predicted = run
        .packets
        .iter()
        .position(|packet| !packet.is_keyframe)
        .expect("the stream has no predicted frames");
    let tail: Vec<&NALUnit> = run.packets[predicted..].iter().collect();
    assert!(
        std::panic::catch_unwind(|| decode(Id::H264, &tail)).is_err(),
        "a decoder joining without a keyframe accepted the stream"
    );

    let mut broken = run.packets[predicted].clone();
    broken.data = vec![0, 0, 0, 1, 0x65, 0xff];
    let mut packets: Vec<&NALUnit> = run.packets.iter().collect();
    packets[predicted] = &broken;
    assert!(
        std::panic::catch_unwind(|| decode(Id::H264, &packets)).is_err(),
        "a decoder accepted a corrupt slice"
    );
}

#[test]
fn amf_honours_keyframe_requests() {
    let serial = serial();
    let Some(expectation) = amf_expectation() else {
        return;
    };
    let run = run_stage(&serial, "h264_amf", |_| LARGE, Some(KEYFRAME_REQUEST_AT));
    match expectation {
        AmfExpectation::Hardware => assert!(
            !run.software_fallback,
            "AMF did not open; check the AMD driver and amfrt64.dll"
        ),
        AmfExpectation::Fallback => assert!(
            run.software_fallback,
            "AMF opened, so this machine cannot test the missing-driver fallback"
        ),
        AmfExpectation::Any => {}
    }
    eprintln!(
        "h264_amf {}",
        if run.software_fallback {
            "could not open; checked the libx264 fallback"
        } else {
            "opened on this GPU"
        }
    );
    assert_h264_keyframe(&run.packets[0], "the first packet");
    assert_keyframe_on_request(&run, Id::H264);

    if !run.software_fallback {
        // The iPad's VideoToolbox decoder depends on these h264_amf options.
        let sps = parse_annex_b_units(&run.packets[0].data)
            .into_iter()
            .find(|unit| nal_type(unit) == Some(7))
            .unwrap();
        assert_eq!(sps[1], 66, "profile is not baseline");
        assert_ne!(sps[2] & 0x40, 0, "baseline profile is not constrained");
        assert!(sps[3] >= 51, "level {} is below 5.1", sps[3]);
        assert!(
            run.packets
                .iter()
                .all(|packet| !contains_nal_type(&packet.data, 9)),
            "AMF emitted access unit delimiters"
        );
    }
}

#[test]
fn amf_follows_capture_mode_changes() {
    let serial = serial();
    if !amf_available(&serial, "h264_amf") {
        return;
    }
    let run = run_stage(&serial, "h264_amf", mode_changes, None);
    assert!(!run.software_fallback, "AMF fell back to libx264");
    assert_follows_mode_changes(&run, Id::H264);
}

#[test]
fn amf_hevc_follows_capture_mode_changes() {
    let serial = serial();
    if !amf_available(&serial, "hevc_amf") {
        return;
    }
    let run = run_stage(&serial, "hevc_amf", mode_changes, None);
    assert!(!run.software_fallback, "AMF fell back to libx264");
    assert_follows_mode_changes(&run, Id::HEVC);
}

#[test]
fn amf_hevc_honours_keyframe_requests() {
    let serial = serial();
    if !amf_available(&serial, "hevc_amf") {
        return;
    }
    let run = run_stage(&serial, "hevc_amf", |_| LARGE, Some(KEYFRAME_REQUEST_AT));
    assert_hevc_keyframe(&run.packets[0], "the first packet");
    assert_keyframe_on_request(&run, Id::HEVC);
}

/// How long a keyframe requested on a still desktop may take to come out.
/// Capture resends the last picture once for the request, and then nothing
/// until the 750 ms keepalive, so an encoder that holds a frame until the
/// next one arrives kept a new viewer waiting that long. Nothing is published
/// meanwhile: a keyframe that comes out at all came out without a next frame.
/// How fast is printed, not checked, because an AMD driver's own reopen took
/// from 50 to over 400 ms in a fresh test process.
const STILL_WINDOW: Duration = Duration::from_millis(1500);
/// How long the encoder may take to open and encode the warm-up frames.
const WARM_UP_LIMIT: Duration = Duration::from_secs(10);
/// The warm-up is over once no packet has come for this long.
const WARM_UP_QUIET: Duration = Duration::from_millis(300);

/// Streams a few frames and waits for the encoder to encode them, goes still,
/// then requests a keyframe with one resent picture and nothing after it, as
/// for a reconnecting viewer.
fn keyframe_on_a_still_desktop(serial: &Serial, encoder: &str) -> StillRun {
    for _ in 1..STAGE_TRIES {
        match keyframe_on_a_still_desktop_once(serial, encoder) {
            Some(result) => return result,
            None => before_another_try(encoder),
        }
    }
    keyframe_on_a_still_desktop_once(serial, encoder)
        .unwrap_or_else(|| panic!("{encoder} was still opening when the first picture was due"))
}

/// [`keyframe_on_a_still_desktop`] once, or None as [`run_stage_once`].
fn keyframe_on_a_still_desktop_once(serial: &Serial, encoder: &str) -> Option<StillRun> {
    ffmpeg_next::init().unwrap();
    let (producer, consumer) = frame_slot();
    let (tx, mut rx) = tokio::sync::mpsc::channel(NAL_CHANNEL_CAPACITY);
    let shared = SharedControl::new(0, 8_000_000);
    shared.running.store(true, Ordering::SeqCst);
    let gpu = GpuInfo {
        encoder_name: encoder.to_string(),
        codec_display_name: encoder.to_string(),
        ..GpuInfo::software_fallback()
    };
    let generation = eternal_host::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
    let resubmits_before = PIPELINE_STATS.lock().keyframe_resubmits;
    let stage_shared = shared.clone();
    let stage = std::thread::spawn(move || {
        run_encode_stage(consumer, tx, stage_shared, gpu, generation).map_err(|e| e.to_string())
    });
    let (packets_tx, packets) = std::sync::mpsc::channel();
    let collector = std::thread::spawn(move || {
        while let Some(nal) = rx.blocking_recv() {
            if packets_tx.send((Instant::now(), nal)).is_err() {
                break;
            }
        }
    });
    let publish = |number: u64| {
        let (width, height) = LARGE;
        let mut data = frame_buffer((width * height * 4) as usize);
        render_synthetic_frame(&mut data, width, height, number);
        producer.publish(RawFrame {
            frame_number: number,
            timestamp: Instant::now(),
            data: Arc::new(data),
            width,
            height,
            // The warm-up's frames are new; the resent one is the cache.
            cached: number == RESENT,
        });
    };

    for number in 0..10 {
        publish(number);
        std::thread::sleep(FRAME_INTERVAL);
    }
    // The request must find a session that has encoded frames, so that it
    // takes the reopen a reconnect takes, whatever the encoder's open cost.
    let warm_up_until = Instant::now() + WARM_UP_LIMIT;
    let mut seen = Vec::new();
    loop {
        match packets.recv_timeout(WARM_UP_QUIET) {
            Ok(packet) => {
                if seen.is_empty() && started_on_the_fallback(serial) {
                    producer.close();
                    let _ = stage.join();
                    let _ = collector.join();
                    return None;
                }
                seen.push(packet);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !seen.is_empty() => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => assert!(
                Instant::now() < warm_up_until,
                "{encoder} encoded nothing within {WARM_UP_LIMIT:?}"
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "{encoder} stage ended during the warm-up: {:?}",
                    stage.join()
                )
            }
        }
    }

    // A viewer connects to the still desktop.
    let requested = Instant::now();
    shared.force_next_idr.store(true, Ordering::SeqCst);
    publish(RESENT);
    let mut keyframe = None;
    while keyframe.is_none() {
        let left = STILL_WINDOW.saturating_sub(requested.elapsed());
        match packets.recv_timeout(left) {
            Ok((at, nal)) => {
                if nal.is_keyframe && nal.sequence == RESENT {
                    keyframe = Some(at.duration_since(requested));
                }
                seen.push((at, nal));
            }
            Err(_) => break,
        }
    }
    producer.close();

    match stage.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("{encoder} encode stage failed: {error}"),
        Err(_) => panic!("{encoder} encode stage panicked"),
    }
    collector.join().unwrap();
    let (software_fallback, resubmits) = {
        let stats = PIPELINE_STATS.lock();
        (
            stats.using_software_fallback,
            stats.keyframe_resubmits - resubmits_before,
        )
    };
    eprintln!(
        "{encoder}: still-desktop keyframe after {keyframe:?}; packets {:?}",
        seen.iter()
            .map(|(at, nal)| (
                nal.sequence,
                nal.is_keyframe,
                at.saturating_duration_since(requested).as_millis() as i64
                    - requested.saturating_duration_since(*at).as_millis() as i64
            ))
            .collect::<Vec<_>>()
    );
    Some(StillRun {
        keyframe,
        software_fallback,
        resubmits,
        sequences: seen.iter().map(|(_, nal)| nal.sequence).collect(),
    })
}

/// The picture resent for the viewer, after the warm-up's frames 0 to 9.
const RESENT: u64 = 100;

struct StillRun {
    /// How long the resent picture's keyframe took to leave the stage, or
    /// `None` if it did not within [`STILL_WINDOW`].
    keyframe: Option<Duration>,
    software_fallback: bool,
    resubmits: u64,
    /// The capture number of each packet, in order.
    sequences: Vec<u64>,
}

fn assert_still_keyframe(serial: &Serial, encoder: &str) {
    let run = keyframe_on_a_still_desktop(serial, encoder);
    assert!(
        encoder == "libx264" || !run.software_fallback,
        "{encoder} fell back to libx264"
    );
    assert!(
        run.keyframe.is_some(),
        "{encoder} held the requested keyframe until the next captured frame"
    );
    // Only the still picture may be encoded twice, and only by being given
    // again: the warm-up's frames came every 20 ms.
    let warm_up: Vec<u64> = run
        .sequences
        .iter()
        .copied()
        .filter(|&sequence| sequence != RESENT)
        .collect();
    assert!(
        warm_up.windows(2).all(|pair| pair[0] < pair[1]),
        "{encoder} encoded a moving picture twice: {:?}",
        run.sequences
    );
    let still = run
        .sequences
        .iter()
        .filter(|&&sequence| sequence == RESENT)
        .count() as u64;
    assert!(
        still.saturating_sub(1) <= run.resubmits,
        "{encoder}: the still picture came out {still} times, given again {}",
        run.resubmits
    );
    if eternal_host::encoder::holds_last_frame(encoder) {
        assert!(
            run.resubmits >= 1,
            "{encoder} brought out the keyframe without being given the picture again"
        );
    } else {
        assert_eq!(
            run.resubmits, 0,
            "{encoder} returns each frame, so nothing is given again"
        );
    }
}

#[test]
fn software_encoder_sends_a_requested_keyframe_on_a_still_desktop() {
    let serial = serial();
    assert_still_keyframe(&serial, "libx264");
}

#[test]
fn amf_sends_a_requested_keyframe_on_a_still_desktop() {
    let serial = serial();
    for name in ["h264_amf", "hevc_amf"] {
        if !amf_available(&serial, name) {
            continue;
        }
        assert_still_keyframe(&serial, name);
    }
}

#[cfg(windows)]
#[test]
#[ignore = "native: an NVIDIA card"]
fn nvenc_sends_a_requested_keyframe_on_a_still_desktop() {
    let serial = serial();
    for name in ["h264_nvenc", "hevc_nvenc"] {
        assert_still_keyframe(&serial, name);
    }
}

#[test]
fn amf_fails_cleanly_without_an_amd_driver() {
    let serial = serial();
    if amf_expectation() != Some(AmfExpectation::Fallback) {
        return;
    }
    // Without an AMD driver, both AMF encoders must fail to open rather than
    // hang or crash. The HEVC one is chosen at runtime for HEVC clients.
    for name in ["h264_amf", "hevc_amf"] {
        assert!(
            !amf_opens(&serial, name),
            "{name} opened without an AMD driver"
        );
    }
}
