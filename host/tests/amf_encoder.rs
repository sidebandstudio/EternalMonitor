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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eternal_host::capture::synthetic::{decode_frame_counter_from_luma, render_synthetic_frame};
use eternal_host::capture::{frame_slot, RawFrame};
use eternal_host::control::SharedControl;
use eternal_host::encoder::{run_encode_stage, NALUnit, NAL_CHANNEL_CAPACITY};
use eternal_host::gpu::GpuInfo;
use eternal_host::stats::PIPELINE_STATS;
use eternal_wire::h264::{contains_nal_type, nal_type, parse_annex_b_units};
use ffmpeg_next::codec::Id;

/// The stage and PIPELINE_STATS are process-global, so stages run one at a time.
static STAGE_LOCK: Mutex<()> = Mutex::new(());

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
fn amf_available(name: &str) -> bool {
    let Some(expectation) = amf_expectation() else {
        return false;
    };
    let opens = amf_opens(name);
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

fn amf_opens(name: &str) -> bool {
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
}

/// Feeds `FRAME_COUNT` frames through a fresh encode stage at
/// `FRAME_INTERVAL`, sized by `size_of`, and raises a keyframe request just
/// before frame `keyframe_at`. Panics if the stage fails or panics.
fn run_stage(
    encoder: &str,
    size_of: impl Fn(u64) -> (u32, u32),
    keyframe_at: Option<u64>,
) -> StageRun {
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
    let stage_shared = shared.clone();
    let stage = std::thread::spawn(move || {
        run_encode_stage(consumer, tx, stage_shared, gpu, generation).map_err(|e| e.to_string())
    });
    // The output channel holds only a few frames, so drain it as we go.
    let collector = std::thread::spawn(move || {
        let mut packets = Vec::new();
        while let Some(nal) = rx.blocking_recv() {
            packets.push(nal);
        }
        packets
    });

    for number in 0..FRAME_COUNT {
        if keyframe_at == Some(number) {
            shared.force_next_idr.store(true, Ordering::SeqCst);
        }
        let (width, height) = size_of(number);
        let mut data = vec![0u8; (width * height * 4) as usize];
        render_synthetic_frame(&mut data, width, height, number);
        producer.publish(RawFrame {
            frame_number: number,
            timestamp: Instant::now(),
            data: Arc::new(data),
            width,
            height,
            #[cfg(target_os = "macos")]
            surface: None,
        });
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
    StageRun {
        packets,
        software_fallback: stats.using_software_fallback,
        codec_name: stats.codec_name.clone(),
    }
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
    decode(codec, &run.packets.iter().collect::<Vec<_>>());
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
    assert!(
        counters.windows(2).all(|pair| pair[0] < pair[1]),
        "frame counters do not advance: {counters:?}"
    );
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
    decode(codec, &run.packets.iter().collect::<Vec<_>>());
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
fn software_encoder_follows_capture_mode_changes() {
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("libx264", mode_changes, None);
    assert_follows_mode_changes(&run, Id::H264);
}

#[test]
fn software_encoder_honours_keyframe_requests() {
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("libx264", |_| LARGE, Some(KEYFRAME_REQUEST_AT));
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
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("h264_qsv", |_| LARGE, None);
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

#[test]
fn strict_decoder_rejects_broken_access_units() {
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("libx264", |_| SMALL, None);
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
    let Some(expectation) = amf_expectation() else {
        return;
    };
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("h264_amf", |_| LARGE, Some(KEYFRAME_REQUEST_AT));
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
    if !amf_available("h264_amf") {
        return;
    }
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("h264_amf", mode_changes, None);
    assert!(!run.software_fallback, "AMF fell back to libx264");
    assert_follows_mode_changes(&run, Id::H264);
}

#[test]
fn amf_hevc_follows_capture_mode_changes() {
    if !amf_available("hevc_amf") {
        return;
    }
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("hevc_amf", mode_changes, None);
    assert!(!run.software_fallback, "AMF fell back to libx264");
    assert_follows_mode_changes(&run, Id::HEVC);
}

#[test]
fn amf_hevc_honours_keyframe_requests() {
    if !amf_available("hevc_amf") {
        return;
    }
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let run = run_stage("hevc_amf", |_| LARGE, Some(KEYFRAME_REQUEST_AT));
    assert_hevc_keyframe(&run.packets[0], "the first packet");
    assert_keyframe_on_request(&run, Id::HEVC);
}

#[test]
fn amf_fails_cleanly_without_an_amd_driver() {
    if amf_expectation() != Some(AmfExpectation::Fallback) {
        return;
    }
    // Without an AMD driver, both AMF encoders must fail to open rather than
    // hang or crash. The HEVC one is chosen at runtime for HEVC clients.
    for name in ["h264_amf", "hevc_amf"] {
        assert!(!amf_opens(name), "{name} opened without an AMD driver");
    }
}
