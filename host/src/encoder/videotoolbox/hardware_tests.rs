//! Hardware checks for the direct VideoToolbox encoder. They are ignored by
//! default because CI's macOS VMs have no hardware encoder. On a Mac:
//!
//! ```sh
//! PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig cargo test --release \
//!   -p eternal-host --lib videotoolbox::hardware_tests -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Each run encodes the synthetic capture pattern at a paced rate, then
//! decodes every access unit twice: with VideoToolbox, the way the iPad app
//! does (`ios/EternalMonitor/Decode/VideoDecoder.swift`), and with FFmpeg's
//! software decoder. Both must return every frame with its frame counter.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use parking_lot::Mutex;

use super::bitstream::{self, ProfileTierLevel};
use super::config::{plan_session, Codec, HevcLimit, Limits, SessionPlan, Tier};
use super::ffi;
use super::session::{
    registered_callbacks, EncodedFrame, FrameInfo, OutputQueue, Tuning, VtError, VtSession,
};
use crate::capture::synthetic::{
    decode_frame_counter_from_luma, draw_frame_counter, render_synthetic_frame, COUNTER_BITS,
    COUNTER_MARGIN, COUNTER_SQUARE, COUNTER_STRIP_H,
};

const FULL: (u32, u32) = (2670, 1878);
const OUTER: (u32, u32) = (2034, 1398);

struct Spec {
    codec: Codec,
    size: (u32, u32),
    fps: u32,
    seconds: f64,
    bitrate: u32,
    /// (seconds into the run, new bitrate)
    bitrate_change: Option<(f64, u32)>,
    keyframe_every: Option<f64>,
    tuning: Tuning,
    limits: Limits,
}

impl Spec {
    fn hevc(size: (u32, u32), fps: u32) -> Self {
        Self {
            codec: Codec::Hevc,
            size,
            fps,
            // ETERNAL_VT_TEST_SECONDS lengthens a run into a soak.
            seconds: std::env::var("ETERNAL_VT_TEST_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(10.0),
            bitrate: 15_000_000,
            bitrate_change: Some((5.0, 30_000_000)),
            keyframe_every: Some(1.0),
            // ETERNAL_VT_* overrides apply, for comparing settings.
            tuning: Tuning::from_env(),
            limits: Limits {
                hevc: HevcLimit::from_env(),
                ..Limits::default()
            },
        }
    }

    fn experimental(self) -> Self {
        Self {
            limits: Limits {
                hevc: HevcLimit::ExperimentalHighTier,
                ..self.limits
            },
            ..self
        }
    }
}

struct Run {
    plan: SessionPlan,
    submitted: usize,
    outputs: Vec<EncodedFrame>,
    keyframe_requests: Vec<u64>,
    call_us: Vec<u64>,
    /// Gaps between successive encode calls: the harness's own pacing.
    submit_gap_us: Vec<u64>,
    bitrate_change_us: Option<u128>,
    change_frame: Option<u64>,
    emitted: Option<ProfileTierLevel>,
    dropped: u64,
    failed: u64,
    load: String,
}

fn load_average() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Eight distinct pattern frames, cycled; the counter strip is redrawn per frame.
fn pattern_frames(width: u32, height: u32) -> Vec<Vec<u8>> {
    (0..8u64)
        .map(|n| {
            let mut frame = vec![0u8; width as usize * height as usize * 4];
            render_synthetic_frame(&mut frame, width, height, n * 7);
            frame
        })
        .collect()
}

fn run(spec: &Spec) -> Run {
    // Pace like the pipeline's capture thread; at the default QoS a loaded
    // machine oversleeps the frame interval by 100 ms and more.
    crate::capture::timing::set_stream_qos();
    // As in the encode loop: the level is planned for the ceiling, here the
    // higher of the two bitrates.
    let ceiling = spec
        .bitrate_change
        .map_or(spec.bitrate, |(_, to)| to.max(spec.bitrate));
    let plan = plan_session(
        spec.codec,
        spec.size.0,
        spec.size.1,
        spec.fps,
        ceiling,
        spec.limits,
    )
    .unwrap();
    let output = OutputQueue::new();
    let mut session = VtSession::new(plan, spec.bitrate, spec.tuning, output.clone())
        .expect("hardware VideoToolbox session");
    eprintln!("session {}", session.describe());
    let (width, height) = spec.size;
    let mut frames = pattern_frames(width, height);
    let fps = plan.encode_fps;
    let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
    let total = (spec.seconds * f64::from(fps)).round() as u64;
    let change_frame = spec
        .bitrate_change
        .map(|(at, _)| (at * f64::from(fps)).round() as u64);
    let key_period = spec
        .keyframe_every
        .map(|every| (every * f64::from(fps)).round() as u64);
    let mut keyframe_requests = Vec::new();
    let mut call_us = Vec::new();
    let mut submit_gap_us = Vec::new();
    let mut last_call: Option<Instant> = None;
    let mut bitrate_change_us = None;
    let load = load_average();
    let started = Instant::now();
    for i in 0..total {
        let due = started + interval * i as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if Some(i) == change_frame {
            let at = Instant::now();
            session.set_bitrate(spec.bitrate_change.unwrap().1).unwrap();
            bitrate_change_us = Some(at.elapsed().as_micros());
        }
        let force = key_period.is_some_and(|period| i > 0 && i % period == 0);
        if force {
            keyframe_requests.push(i);
        }
        let frame = &mut frames[(i % 8) as usize];
        draw_frame_counter(frame, width, i);
        let call = Instant::now();
        if let Some(last) = last_call.replace(call) {
            submit_gap_us.push(call.duration_since(last).as_micros() as u64);
        }
        session
            .encode_bgra(
                frame,
                width as usize * 4,
                FrameInfo {
                    captured: call,
                    frame_number: i,
                },
                force,
            )
            .unwrap();
        call_us.push(call.elapsed().as_micros() as u64);
    }
    session.flush().unwrap();
    let counters = session.counters();
    let emitted = session.emitted_level();
    drop(session);
    output.close();
    let mut outputs = Vec::new();
    while let Some(frame) = output.pop_timeout(Duration::ZERO) {
        outputs.push(frame);
    }
    Run {
        plan,
        submitted: total as usize,
        outputs,
        keyframe_requests,
        call_us,
        submit_gap_us,
        bitrate_change_us,
        change_frame,
        emitted,
        dropped: counters.dropped,
        failed: counters.failed,
        load,
    }
}

fn percentile(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let index = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[index] as f64
}

fn ms_stats(values: impl IntoIterator<Item = u64>) -> (f64, f64, f64) {
    let mut values: Vec<u64> = values.into_iter().collect();
    values.sort_unstable();
    (
        percentile(&values, 0.5) / 1000.0,
        percentile(&values, 0.95) / 1000.0,
        percentile(&values, 0.99) / 1000.0,
    )
}

struct Summary {
    out_fps: f64,
    latency: (f64, f64, f64),
    keyframe_latency: (f64, f64, f64),
    max_gap_near_change_ms: f64,
    p99_gap_ms: f64,
    mbps_before: f64,
    mbps_after: f64,
    keys_near_change: Vec<u64>,
}

fn summarize(label: &str, run: &Run) -> Summary {
    let fps = run.plan.encode_fps;
    let outs = &run.outputs;
    let span = outs
        .last()
        .unwrap()
        .delivered
        .duration_since(outs[0].delivered)
        .as_secs_f64();
    let out_fps = (outs.len() - 1) as f64 / span;
    let latency = ms_stats(outs.iter().map(|o| o.encode_us as u64));
    let keyframe_latency = ms_stats(
        outs.iter()
            .filter(|o| o.keyframe_requested)
            .map(|o| o.encode_us as u64),
    );
    let gaps: Vec<(u64, u64)> = outs
        .windows(2)
        .map(|pair| {
            (
                pair[1].frame_number,
                pair[1]
                    .delivered
                    .duration_since(pair[0].delivered)
                    .as_micros() as u64,
            )
        })
        .collect();
    let mut sorted_gaps: Vec<u64> = gaps.iter().map(|g| g.1).collect();
    sorted_gaps.sort_unstable();
    let p99_gap_ms = percentile(&sorted_gaps, 0.99) / 1000.0;
    let (mut max_gap_near_change_ms, mut mbps_before, mut mbps_after) = (0.0, 0.0, 0.0);
    let mut keys_near_change = Vec::new();
    if let Some(change) = run.change_frame {
        let near = change.saturating_sub(u64::from(fps) / 4)..change + u64::from(fps) / 2;
        max_gap_near_change_ms = gaps
            .iter()
            .filter(|(frame, _)| near.contains(frame))
            .map(|g| g.1)
            .max()
            .unwrap_or(0) as f64
            / 1000.0;
        keys_near_change = outs
            .iter()
            .filter(|o| o.keyframe && !o.keyframe_requested && near.contains(&o.frame_number))
            .map(|o| o.frame_number)
            .collect();
        // One second before the change and the second after it settles.
        let bits = |range: std::ops::Range<u64>| {
            outs.iter()
                .filter(|o| range.contains(&o.frame_number) && !o.keyframe)
                .map(|o| o.data.len() as f64 * 8.0)
                .sum::<f64>()
                / range.count() as f64
                * f64::from(fps)
                / 1e6
        };
        mbps_before = bits(change - u64::from(fps)..change);
        mbps_after = bits(change + u64::from(fps)..change + 2 * u64::from(fps));
    }
    let unexpected_keys: Vec<u64> = outs
        .iter()
        .filter(|o| o.keyframe && !o.keyframe_requested && o.frame_number != 0)
        .map(|o| o.frame_number)
        .collect();
    let calls = ms_stats(run.call_us.iter().copied());
    let submit_gap_p99_ms = ms_stats(run.submit_gap_us.iter().copied()).2;
    eprintln!(
        "RESULT {label} codec={:?} size={}x{} fps={} load=[{}] submitted={} out={} dropped={} failed={} \
         out_fps={out_fps:.1} latency_ms p50={:.1} p95={:.1} p99={:.1} \
         encode_call_ms p50={:.2} p95={:.2} p99={:.2} \
         keyframe_req={} keyframe_ms p50={:.1} p95={:.1} max={:.1} \
         level={} required={} bitrate_change_us={:?} gap_near_change_ms={max_gap_near_change_ms:.1} \
         p99_gap_ms={p99_gap_ms:.1} submit_gap_p99_ms={submit_gap_p99_ms:.1} mbps_before={mbps_before:.1} mbps_after={mbps_after:.1} \
         unrequested_keys={unexpected_keys:?}",
        run.plan.codec,
        run.plan.width,
        run.plan.height,
        fps,
        run.load,
        run.submitted,
        outs.len(),
        run.dropped,
        run.failed,
        latency.0,
        latency.1,
        latency.2,
        calls.0,
        calls.1,
        calls.2,
        run.keyframe_requests.len(),
        keyframe_latency.0,
        keyframe_latency.1,
        keyframe_latency.2,
        run.emitted
            .map(|level| level.describe(run.plan.codec))
            .unwrap_or_default(),
        run.plan.level.name,
        run.bitrate_change_us,
    );
    Summary {
        out_fps,
        latency,
        keyframe_latency,
        max_gap_near_change_ms,
        p99_gap_ms,
        mbps_before,
        mbps_after,
        keys_near_change,
    }
}

/// What every streaming run must satisfy, whatever the machine's load.
fn assert_stream(run: &Run, summary: &Summary) {
    let fps = f64::from(run.plan.encode_fps);
    assert_eq!(run.failed, 0, "no frame may fail");
    assert_eq!(
        run.outputs.len() as u64 + run.dropped,
        run.submitted as u64,
        "every frame comes back"
    );
    assert!(run.outputs[0].keyframe, "the stream starts on a keyframe");
    for request in &run.keyframe_requests {
        let out = run
            .outputs
            .iter()
            .find(|o| o.frame_number == *request)
            .unwrap_or_else(|| panic!("requested keyframe {request} was dropped"));
        assert!(out.keyframe, "frame {request} was asked to be a keyframe");
        assert!(
            eternal_wire::hevc::contains_parameter_sets(&out.data) || run.plan.codec == Codec::H264
        );
    }
    let emitted = run.emitted.expect("a keyframe's parameter sets");
    assert!(
        emitted.level_idc >= run.plan.level.idc,
        "level {} below the required {}",
        emitted.describe(run.plan.codec),
        run.plan.level.name
    );
    assert_eq!(
        emitted.high_tier,
        run.plan.tier == Tier::High,
        "VideoToolbox wrote {} for a {:?}-tier plan",
        emitted.describe(run.plan.codec),
        run.plan.tier
    );
    assert!(
        summary.out_fps > fps * 0.9,
        "{:.1} fps out for {fps}",
        summary.out_fps
    );
    // Loose: the shared devbox runs at load 100-800. The numbers are printed.
    assert!(summary.latency.0 < 100.0, "p50 {:.1} ms", summary.latency.0);
    assert!(summary.keyframe_latency.2 < 250.0);
    if run.change_frame.is_some() {
        assert!(
            run.bitrate_change_us.unwrap() < 20_000,
            "a live bitrate change is a property set"
        );
        assert!(
            summary.keys_near_change.is_empty(),
            "a bitrate change must not insert an IDR: {:?}",
            summary.keys_near_change
        );
        let interval_ms = 1000.0 / fps;
        assert!(
            summary.max_gap_near_change_ms <= (4.0 * interval_ms).max(summary.p99_gap_ms * 1.5),
            "gap of {:.1} ms around the bitrate change (p99 gap {:.1} ms)",
            summary.max_gap_near_change_ms,
            summary.p99_gap_ms
        );
        assert!(
            summary.mbps_after > summary.mbps_before * 1.3,
            "bitrate did not follow the change: {:.1} -> {:.1} Mbps",
            summary.mbps_before,
            summary.mbps_after
        );
    }
}

/// Luma (8-bit) in the counter strip's solid black run past the last
/// square, and at the centre of bit 0's square. Both sit well inside flat
/// areas, away from the ringing at square edges.
fn range_probe(luma: &[u8], stride: usize) -> (u8, u8) {
    let row = COUNTER_STRIP_H / 2;
    let black_x = COUNTER_MARGIN + COUNTER_BITS * (COUNTER_SQUARE + COUNTER_MARGIN) + 32;
    let center = COUNTER_MARGIN + COUNTER_SQUARE / 2;
    (luma[row * stride + black_x], luma[center * stride + center])
}

struct DecodeReport {
    counters: Vec<u64>,
    /// (black, white) luma on an odd frame, where bit 0's square is white.
    luma_black_white: Option<(u8, u8)>,
    detail: String,
}

fn assert_decoded(label: &str, run: &Run, report: &DecodeReport) {
    let expected: Vec<u64> = run.outputs.iter().map(|o| o.frame_number).collect();
    assert_eq!(
        report.counters.len(),
        expected.len(),
        "{label}: decoded {} of {} frames",
        report.counters.len(),
        expected.len()
    );
    assert_eq!(
        report.counters, expected,
        "{label}: decoded frame counters must match"
    );
    // Video range: black is 16 and white 235, not 0 and 255.
    let (black, white) = report.luma_black_white.expect("an odd frame");
    assert!(black.abs_diff(16) <= 4, "{label}: black luma {black}");
    assert!(white.abs_diff(235) <= 4, "{label}: white luma {white}");
    eprintln!(
        "DECODE {label}: {} frames, counters match, black={black} white={white} {}",
        report.counters.len(),
        report.detail
    );
}

fn ffmpeg_decode(run: &Run) -> DecodeReport {
    let _ = ffmpeg_next::init();
    let id = match run.plan.codec {
        Codec::Hevc => ffmpeg_next::codec::Id::HEVC,
        Codec::H264 => ffmpeg_next::codec::Id::H264,
    };
    let codec = ffmpeg_next::decoder::find(id).unwrap();
    let mut context = ffmpeg_next::codec::Context::new_with_codec(codec);
    context.set_threading(ffmpeg_next::threading::Config {
        kind: ffmpeg_next::threading::Type::Slice,
        count: 0,
    });
    let mut decoder = context.decoder().video().unwrap();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let mut report = DecodeReport {
        counters: Vec::new(),
        luma_black_white: None,
        detail: String::new(),
    };
    let width = run.plan.width;
    let mut receive = |decoder: &mut ffmpeg_next::decoder::Video, report: &mut DecodeReport| {
        while decoder.receive_frame(&mut frame).is_ok() {
            let counter = decode_frame_counter_from_luma(frame.data(0), frame.stride(0), width);
            if counter % 2 == 1 && report.luma_black_white.is_none() {
                report.luma_black_white = Some(range_probe(frame.data(0), frame.stride(0)));
                report.detail = format!(
                    "range={:?} space={:?} primaries={:?} transfer={:?} format={:?} {}x{}",
                    frame.color_range(),
                    frame.color_space(),
                    frame.color_primaries(),
                    frame.color_transfer_characteristic(),
                    frame.format(),
                    frame.width(),
                    frame.height()
                );
                assert_eq!(frame.color_range(), ffmpeg_next::color::Range::MPEG);
                assert_eq!(frame.color_space(), ffmpeg_next::color::Space::BT709);
                assert_eq!(
                    frame.color_primaries(),
                    ffmpeg_next::color::Primaries::BT709
                );
                assert_eq!((frame.width(), frame.height()), (width, run.plan.height));
            }
            report.counters.push(counter);
        }
    };
    for out in &run.outputs {
        decoder
            .send_packet(&ffmpeg_next::Packet::copy(&out.data))
            .unwrap_or_else(|error| panic!("FFmpeg rejected frame {}: {error}", out.frame_number));
        receive(&mut decoder, &mut report);
    }
    decoder.send_eof().unwrap();
    receive(&mut decoder, &mut report);
    // The parser's reading of the real parameter sets must match FFmpeg's.
    // SAFETY: plain field reads of an open decoder context.
    let (profile, level) = unsafe {
        let raw = decoder.as_ptr();
        ((*raw).profile, (*raw).level)
    };
    let emitted = run.emitted.unwrap();
    assert_eq!(level, i32::from(emitted.level_idc), "FFmpeg's level");
    assert_eq!(profile, i32::from(emitted.profile_idc), "FFmpeg's profile");
    report
        .detail
        .push_str(&format!(" ffmpeg_level={level} ffmpeg_profile={profile}"));
    report
}

// --- VideoToolbox decode, as VideoDecoder.swift does it ------------------

type CMVideoFormatDescriptionRef = *const c_void;
type VTDecompressionSessionRef = *mut c_void;

#[repr(C)]
struct DecompressionCallbackRecord {
    callback: extern "C" fn(
        refcon: *mut c_void,
        frame_refcon: *mut c_void,
        status: i32,
        flags: u32,
        image: ffi::CVPixelBufferRef,
        pts: ffi::CMTime,
        duration: ffi::CMTime,
    ),
    refcon: *mut c_void,
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: *const c_void,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        extensions: ffi::CFDictionaryRef,
        out: *mut CMVideoFormatDescriptionRef,
    ) -> i32;
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: *const c_void,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        out: *mut CMVideoFormatDescriptionRef,
    ) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: *const c_void,
        memory: *mut c_void,
        block_length: usize,
        block_allocator: *const c_void,
        custom_source: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut ffi::CMBlockBufferRef,
    ) -> i32;
    fn CMBlockBufferReplaceDataBytes(
        source: *const c_void,
        destination: ffi::CMBlockBufferRef,
        offset: usize,
        length: usize,
    ) -> i32;
    fn CMSampleBufferCreateReady(
        allocator: *const c_void,
        data: ffi::CMBlockBufferRef,
        format: CMVideoFormatDescriptionRef,
        samples: isize,
        timing_entries: isize,
        timing: *const c_void,
        size_entries: isize,
        sizes: *const usize,
        out: *mut ffi::CMSampleBufferRef,
    ) -> i32;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTDecompressionSessionCreate(
        allocator: *const c_void,
        format: CMVideoFormatDescriptionRef,
        decoder_specification: ffi::CFDictionaryRef,
        image_attributes: ffi::CFDictionaryRef,
        callback: *const DecompressionCallbackRecord,
        out: *mut VTDecompressionSessionRef,
    ) -> i32;
    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample: ffi::CMSampleBufferRef,
        flags: u32,
        frame_refcon: *mut c_void,
        info_out: *mut u32,
    ) -> i32;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: VTDecompressionSessionRef) -> i32;
    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);
    static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: CFStringRef;
    static kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder: CFStringRef;
    static kVTDecompressionPropertyKey_RealTime: CFStringRef;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVBufferCopyAttachment(buffer: *mut c_void, key: CFStringRef, mode: *mut u32) -> CFTypeRef;
    static kCVImageBufferYCbCrMatrixKey: CFStringRef;
}

#[derive(Default)]
struct VtDecoded {
    counters: Vec<u64>,
    errors: Vec<i32>,
    probe: Option<(u8, u8)>,
    matrix: Option<String>,
}

extern "C" fn on_decoded(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    _flags: u32,
    image: ffi::CVPixelBufferRef,
    _pts: ffi::CMTime,
    _duration: ffi::CMTime,
) {
    // SAFETY: refcon is the Arc'd Mutex the test keeps alive past the
    // session's invalidation; `image` is valid for the callback.
    let decoded = unsafe { &*(refcon as *const Mutex<VtDecoded>) };
    let mut decoded = decoded.lock();
    if status != 0 || image.is_null() {
        decoded.errors.push(status);
        return;
    }
    unsafe {
        ffi::CVPixelBufferLockBaseAddress(image, 1);
        let luma = ffi::CVPixelBufferGetBaseAddressOfPlane(image, 0);
        let stride = ffi::CVPixelBufferGetBytesPerRowOfPlane(image, 0);
        let rows = ffi::CVPixelBufferGetHeightOfPlane(image, 0);
        let width = ffi::CVPixelBufferGetWidth(image) as u32;
        let plane = std::slice::from_raw_parts(luma, stride * rows);
        let counter = decode_frame_counter_from_luma(plane, stride, width);
        if counter % 2 == 1 && decoded.probe.is_none() {
            decoded.probe = Some(range_probe(plane, stride));
            let matrix =
                CVBufferCopyAttachment(image, kCVImageBufferYCbCrMatrixKey, std::ptr::null_mut());
            decoded.matrix = (!matrix.is_null())
                .then(|| format!("{:?}", CFType::wrap_under_create_rule(matrix)));
        }
        decoded.counters.push(counter);
        ffi::CVPixelBufferUnlockBaseAddress(image, 1);
    }
}

/// Decodes the run with a hardware `VTDecompressionSession`, rebuilt from
/// each keyframe's parameter sets: what VideoDecoder.swift does. SEI and
/// AUD are stripped and the slices resubmitted as 4-byte-length samples.
fn videotoolbox_decode(run: &Run) -> DecodeReport {
    let hevc = run.plan.codec == Codec::Hevc;
    let decoded = Arc::new(Mutex::new(VtDecoded::default()));
    let mut session: VTDecompressionSessionRef = std::ptr::null_mut();
    let mut format: CMVideoFormatDescriptionRef = std::ptr::null();
    let mut hardware = Vec::new();
    let record = DecompressionCallbackRecord {
        callback: on_decoded,
        refcon: Arc::as_ptr(&decoded) as *mut c_void,
    };
    let key = |k: CFStringRef| unsafe { CFString::wrap_under_get_rule(k) };
    for out in &run.outputs {
        let units = eternal_wire::h264::parse_annex_b_units(&out.data);
        let (mut sets, mut slices) = (Vec::new(), Vec::new());
        for unit in &units {
            let kind = if hevc {
                (unit[0] >> 1) & 0x3f
            } else {
                unit[0] & 0x1f
            };
            match (hevc, kind) {
                (true, 32..=34) | (false, 7 | 8) => sets.push(unit.as_slice()),
                (true, 35 | 39 | 40) | (false, 6 | 9) => {}
                _ => slices.push(unit.as_slice()),
            }
        }
        if !sets.is_empty() {
            unsafe {
                if !session.is_null() {
                    VTDecompressionSessionWaitForAsynchronousFrames(session);
                    VTDecompressionSessionInvalidate(session);
                    ffi::CFRelease(session as CFTypeRef);
                    session = std::ptr::null_mut();
                }
                if !format.is_null() {
                    ffi::CFRelease(format);
                }
                let pointers: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
                let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
                let status = if hevc {
                    CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                        std::ptr::null(),
                        sets.len(),
                        pointers.as_ptr(),
                        sizes.as_ptr(),
                        4,
                        std::ptr::null(),
                        &mut format,
                    )
                } else {
                    CMVideoFormatDescriptionCreateFromH264ParameterSets(
                        std::ptr::null(),
                        sets.len(),
                        pointers.as_ptr(),
                        sizes.as_ptr(),
                        4,
                        &mut format,
                    )
                };
                assert_eq!(
                    status, 0,
                    "format description from frame {}",
                    out.frame_number
                );
                let spec = CFDictionary::from_CFType_pairs(&[(
                    key(kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder),
                    CFBoolean::true_value().as_CFType(),
                )]);
                let empty: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[]);
                let attributes = CFDictionary::from_CFType_pairs(&[
                    (
                        key(ffi::kCVPixelBufferPixelFormatTypeKey),
                        CFNumber::from(ffi::PIXEL_FORMAT_NV12_VIDEO_RANGE as i64).as_CFType(),
                    ),
                    (
                        key(ffi::kCVPixelBufferIOSurfacePropertiesKey),
                        empty.as_CFType(),
                    ),
                ]);
                let status = VTDecompressionSessionCreate(
                    std::ptr::null(),
                    format,
                    spec.as_concrete_TypeRef().cast(),
                    attributes.as_concrete_TypeRef().cast(),
                    &record,
                    &mut session,
                );
                assert_eq!(status, 0, "hardware decoder for frame {}", out.frame_number);
                ffi::VTSessionSetProperty(
                    session,
                    kVTDecompressionPropertyKey_RealTime,
                    CFBoolean::true_value().as_CFTypeRef(),
                );
                let mut value: CFTypeRef = std::ptr::null();
                ffi::VTSessionCopyProperty(
                    session,
                    kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                    std::ptr::null(),
                    &mut value,
                );
                hardware.push(
                    !value.is_null()
                        && CFType::wrap_under_create_rule(value)
                            .downcast::<CFBoolean>()
                            .is_some_and(bool::from),
                );
            }
        }
        assert!(
            !session.is_null(),
            "frame {} arrived before a keyframe",
            out.frame_number
        );
        let mut sample_bytes = Vec::new();
        for slice in &slices {
            sample_bytes.extend_from_slice(&(slice.len() as u32).to_be_bytes());
            sample_bytes.extend_from_slice(slice);
        }
        unsafe {
            let mut block: ffi::CMBlockBufferRef = std::ptr::null_mut();
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    sample_bytes.len(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    sample_bytes.len(),
                    1,
                    &mut block,
                ),
                0
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(
                    sample_bytes.as_ptr().cast(),
                    block,
                    0,
                    sample_bytes.len()
                ),
                0
            );
            let mut sample: ffi::CMSampleBufferRef = std::ptr::null_mut();
            let size = sample_bytes.len();
            assert_eq!(
                CMSampleBufferCreateReady(
                    std::ptr::null(),
                    block,
                    format,
                    1,
                    0,
                    std::ptr::null(),
                    1,
                    &size,
                    &mut sample,
                ),
                0
            );
            let status = VTDecompressionSessionDecodeFrame(
                session,
                sample,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if status != 0 {
                decoded.lock().errors.push(status);
            }
            ffi::CFRelease(sample as CFTypeRef);
            ffi::CFRelease(block as CFTypeRef);
        }
    }
    unsafe {
        if !session.is_null() {
            VTDecompressionSessionWaitForAsynchronousFrames(session);
            VTDecompressionSessionInvalidate(session);
            ffi::CFRelease(session as CFTypeRef);
        }
        if !format.is_null() {
            ffi::CFRelease(format);
        }
    }
    let decoded = std::mem::take(&mut *decoded.lock());
    assert!(
        decoded.errors.is_empty(),
        "decode errors {:?}",
        decoded.errors
    );
    assert!(
        hardware.iter().all(|&h| h),
        "VideoToolbox must decode in hardware: {hardware:?}"
    );
    let matrix = decoded.matrix.clone().unwrap_or_default();
    // MetalView picks BT.601 only when the buffer says so; BT.709 otherwise.
    assert!(matrix.contains("709"), "decoded buffers carry {matrix:?}");
    DecodeReport {
        counters: decoded.counters,
        luma_black_white: decoded.probe,
        detail: format!("hardware sessions={} matrix={matrix}", hardware.len()),
    }
}

fn stream_and_verify(label: &str, spec: Spec) -> Summary {
    let run = run(&spec);
    let summary = summarize(label, &run);
    assert_stream(&run, &summary);
    assert_decoded(
        &format!("{label} videotoolbox"),
        &run,
        &videotoolbox_decode(&run),
    );
    assert_decoded(&format!("{label} ffmpeg"), &run, &ffmpeg_decode(&run));
    summary
}

#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn hevc_full_size_at_60() {
    stream_and_verify("hevc_full_60", Spec::hevc(FULL, 60));
}

/// The default: Main tier, so a 120 Hz request at full size runs at 60.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn hevc_full_size_at_120_runs_main_tier_at_60() {
    let spec = Spec {
        limits: Limits::default(),
        ..Spec::hevc(FULL, 120)
    };
    let run = run(&spec);
    assert_eq!((run.plan.encode_fps, run.plan.tier), (60, Tier::Main));
    let summary = summarize("hevc_full_120_default", &run);
    assert_stream(&run, &summary);
    assert_eq!(
        run.emitted.unwrap().describe(Codec::Hevc),
        "Main Main tier 5.1"
    );
    assert_decoded(
        "hevc_full_120_default videotoolbox",
        &run,
        &videotoolbox_decode(&run),
    );
    assert_decoded("hevc_full_120_default ffmpeg", &run, &ffmpeg_decode(&run));
}

/// `ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER`: full size at 120 is Main High
/// tier 5.2, which no device decoder has been checked with.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn experimental_hevc_full_size_at_120_is_high_tier_5_2() {
    let summary_spec = Spec::hevc(FULL, 120).experimental();
    let run = run(&summary_spec);
    assert_eq!((run.plan.encode_fps, run.plan.tier), (120, Tier::High));
    let summary = summarize("hevc_full_120_experimental", &run);
    assert_stream(&run, &summary);
    assert_eq!(
        run.emitted.unwrap().describe(Codec::Hevc),
        "Main High tier 5.2"
    );
    assert_decoded(
        "hevc_full_120_experimental videotoolbox",
        &run,
        &videotoolbox_decode(&run),
    );
    assert_decoded(
        "hevc_full_120_experimental ffmpeg",
        &run,
        &ffmpeg_decode(&run),
    );
}

/// The outer screen keeps 120 fps on Main tier 5.1 by default.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn hevc_outer_size_at_120() {
    let spec = Spec {
        limits: Limits::default(),
        ..Spec::hevc(OUTER, 120)
    };
    stream_and_verify("hevc_outer_120", spec);
}

#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn h264_full_size_at_120_thins_to_60_at_level_5_2() {
    let spec = Spec {
        codec: Codec::H264,
        seconds: 6.0,
        bitrate_change: Some((3.0, 30_000_000)),
        ..Spec::hevc(FULL, 120)
    };
    stream_and_verify("h264_full_120", spec);
}

#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn h264_outer_size_at_120_runs_at_60() {
    let spec = Spec {
        codec: Codec::H264,
        seconds: 6.0,
        bitrate_change: Some((3.0, 30_000_000)),
        ..Spec::hevc(OUTER, 120)
    };
    stream_and_verify("h264_outer_120", spec);
}

/// What the encode loop does when the session dies underneath it (sleep,
/// media server reset): the dead session reports itself lost, a new one
/// starts on an IDR with parameter sets, and the joined stream decodes.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn a_lost_session_is_replaced_and_the_stream_still_decodes() {
    let plan = plan_session(Codec::Hevc, 1280, 720, 60, 8_000_000, Limits::default()).unwrap();
    let output = OutputQueue::new();
    let mut frames = pattern_frames(1280, 720);
    let mut encode = |session: &mut VtSession, i: u64, force: bool| {
        let frame = &mut frames[(i % 8) as usize];
        draw_frame_counter(frame, 1280, i);
        session.encode_bgra(
            frame,
            1280 * 4,
            FrameInfo {
                captured: Instant::now(),
                frame_number: i,
            },
            force,
        )
    };
    let mut first = VtSession::new(plan, 8_000_000, Tuning::default(), output.clone()).unwrap();
    for i in 0..30 {
        encode(&mut first, i, false).unwrap();
        std::thread::sleep(Duration::from_millis(16));
    }
    first.flush().unwrap();
    first.invalidate_for_test();
    let error = encode(&mut first, 30, false).unwrap_err();
    assert!(matches!(error, VtError::SessionLost(_)), "{error:?}");
    assert!(first.is_lost());
    drop(first);

    let mut second = VtSession::new(plan, 8_000_000, Tuning::default(), output.clone()).unwrap();
    for i in 31..61 {
        encode(&mut second, i, i == 31).unwrap();
        std::thread::sleep(Duration::from_millis(16));
    }
    // A frame of the wrong size is an input error, not a lost session.
    let small = vec![0u8; 64 * 64 * 4];
    assert!(matches!(
        second.encode_bgra(
            &small,
            64 * 4,
            FrameInfo {
                captured: Instant::now(),
                frame_number: 99
            },
            false
        ),
        Err(VtError::Input(_))
    ));
    second.flush().unwrap();
    let emitted = second.emitted_level();
    drop(second);
    output.close();
    let mut outputs = Vec::new();
    while let Some(frame) = output.pop_timeout(Duration::ZERO) {
        outputs.push(frame);
    }
    let restart = outputs.iter().position(|o| o.frame_number == 31).unwrap();
    assert!(outputs[restart].keyframe);
    assert!(eternal_wire::hevc::contains_parameter_sets(
        &outputs[restart].data
    ));
    let run = Run {
        plan,
        submitted: outputs.len(),
        outputs,
        keyframe_requests: vec![31],
        call_us: Vec::new(),
        submit_gap_us: Vec::new(),
        bitrate_change_us: None,
        change_frame: None,
        emitted,
        dropped: 0,
        failed: 0,
        load: load_average(),
    };
    assert_eq!(run.outputs.len(), 60);
    assert_decoded("recovery videotoolbox", &run, &videotoolbox_decode(&run));
    assert_decoded("recovery ffmpeg", &run, &ffmpeg_decode(&run));
}

/// Informational: the latency and throughput of each tuning knob at full
/// size and 120 fps, to justify the defaults. Prints one RESULT per row.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder; slow"]
fn tuning_matrix_at_full_size_120() {
    let base = Tuning::default();
    let rows: Vec<(&str, Tuning)> = vec![
        ("default", base),
        (
            "realtime_on",
            Tuning {
                real_time: true,
                ..base
            },
        ),
        (
            "data_rate_limit",
            Tuning {
                data_rate_limit: true,
                ..base
            },
        ),
        (
            "speed_off",
            Tuning {
                prioritize_speed: false,
                ..base
            },
        ),
        (
            "max_delay_0",
            Tuning {
                max_frame_delay: Some(0),
                ..base
            },
        ),
        (
            "max_delay_1",
            Tuning {
                max_frame_delay: Some(1),
                ..base
            },
        ),
        (
            "max_delay_3",
            Tuning {
                max_frame_delay: Some(3),
                ..base
            },
        ),
    ];
    let mut table = HashMap::new();
    for (label, tuning) in rows {
        let spec = Spec {
            seconds: 8.0,
            bitrate_change: None,
            keyframe_every: Some(2.0),
            tuning,
            // 120 fps at full size needs High tier.
            ..Spec::hevc(FULL, 120).experimental()
        };
        let run = run(&spec);
        let summary = summarize(&format!("tuning_{label}"), &run);
        table.insert(label, (summary.out_fps, summary.latency, run.dropped));
    }
    eprintln!("TUNING {table:?}");
}

/// Runs the real encode loop on the synthetic pattern at `size` and `fps`
/// for `seconds`. Halfway through, the user's ceiling and the ABR's pick
/// both rise from `from` to `to`, and a few frames later a keyframe is
/// requested, so the stream's next parameter sets show the level the
/// session declares after the change. Returns every access unit and the
/// frame number the change came at.
fn run_loop_with_bitrate_increase(
    codec: Codec,
    size: (u32, u32),
    fps: u32,
    seconds: f64,
    from: u32,
    to: u32,
) -> (Vec<super::NALUnit>, u64, crate::control::SharedControl) {
    use std::sync::atomic::Ordering;

    let shared = crate::control::SharedControl::new(0, from);
    shared.running.store(true, Ordering::SeqCst);
    shared.target_fps.store(fps, Ordering::SeqCst);
    shared
        .hevc_enabled
        .store(codec == Codec::Hevc, Ordering::SeqCst);
    if codec == Codec::Hevc {
        // A client that advertises HEVC decode, as HELLO2 would register it.
        struct Config;
        impl crate::transport::session::ConfigSource for Config {
            fn stream_config(&self) -> eternal_wire::v2::control::StreamConfig {
                eternal_wire::v2::control::StreamConfig {
                    stream_epoch: 1,
                    width: 0,
                    height: 0,
                    fps: 0,
                    codec: 0,
                    flags: 0,
                    bitrate_bps: 0,
                }
            }
            fn host_name(&self) -> String {
                "test".into()
            }
        }
        use eternal_wire::v2::control::{ControlMessage, Hello2, CAP_DECODE_H264, CAP_DECODE_HEVC};
        shared.session.lock().handle_control(
            crate::transport::link::PeerId::udp(([127, 0, 0, 1], 50_000).into()),
            0,
            ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: 50_000,
                decoder_caps: CAP_DECODE_H264 | CAP_DECODE_HEVC,
                feature_caps: 0,
                screen_px_w: size.0 as u16,
                screen_px_h: size.1 as u16,
                screen_pt_w: size.0 as u16 / 2,
                screen_pt_h: size.1 as u16 / 2,
                refresh_hz: 120,
                device_name: "test".into(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
            &Config,
            Instant::now(),
        );
        assert!(shared.session.lock().client_supports_hevc());
    }
    let (producer, consumer) = crate::capture::frame_slot();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
    let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
    let loop_shared = shared.clone();
    // The pipeline's capture and encode threads run at this QoS
    // (`run_encode_stage`); without it a loaded machine starves them.
    let encoder = std::thread::spawn(move || {
        crate::capture::timing::set_stream_qos();
        super::run_encode_loop(consumer, tx, loop_shared, generation).map_err(|e| e.to_string())
    });
    crate::capture::timing::set_stream_qos();
    let (width, height) = size;
    let mut frames = pattern_frames(width, height);
    let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
    let total = (seconds * f64::from(fps)).round() as u64;
    let change = total / 2;
    let started = Instant::now();
    for i in 0..total {
        if let Some(wait) = (started + interval * i as u32).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if i == change {
            shared.bitrate_bps.store(to, Ordering::SeqCst);
            shared.abr_current_bps.store(to, Ordering::SeqCst);
        }
        if i == change + 5 {
            shared.force_next_idr.store(true, Ordering::SeqCst);
        }
        let frame = &mut frames[(i % 8) as usize];
        draw_frame_counter(frame, width, i);
        producer.publish(crate::capture::RawFrame {
            frame_number: i,
            timestamp: Instant::now(),
            data: Arc::new(frame.clone()),
            width,
            height,
            #[cfg(target_os = "macos")]
            surface: None,
        });
    }
    eprintln!(
        "LOOP published {total} frames in {:.2} s (asked for {seconds} s)",
        started.elapsed().as_secs_f64()
    );
    producer.close();
    encoder.join().unwrap().unwrap();
    let mut units = Vec::new();
    while let Ok(unit) = rx.try_recv() {
        units.push(unit);
    }
    (units, change, shared)
}

/// The profile, tier and level in an access unit's VPS (HEVC) or SPS.
fn access_unit_ptl(codec: Codec, unit: &[u8]) -> Option<ProfileTierLevel> {
    let starts: Vec<usize> = unit
        .windows(4)
        .enumerate()
        .filter(|(_, window)| *window == [0, 0, 0, 1])
        .map(|(at, _)| at + 4)
        .collect();
    starts.iter().enumerate().find_map(|(index, &start)| {
        let end = starts.get(index + 1).map_or(unit.len(), |next| next - 4);
        bitstream::parameter_set_ptl(codec, &unit[start..end])
    })
}

/// Decodes what the encode loop sent, with VideoToolbox and FFmpeg.
fn assert_loop_stream_decodes(label: &str, plan: SessionPlan, units: &[super::NALUnit]) {
    let outputs: Vec<EncodedFrame> = units
        .iter()
        .map(|unit| EncodedFrame {
            keyframe: unit.is_keyframe,
            keyframe_requested: false,
            captured: unit.timestamp,
            frame_number: unit.sequence,
            encode_us: unit.encode_duration_us,
            delivered: Instant::now(),
            data: unit.data.clone(),
        })
        .collect();
    let run = Run {
        plan,
        submitted: outputs.len(),
        outputs,
        keyframe_requests: Vec::new(),
        call_us: Vec::new(),
        submit_gap_us: Vec::new(),
        bitrate_change_us: None,
        change_frame: None,
        // FFmpeg reports the parameter sets it saw last.
        emitted: units
            .iter()
            .rev()
            .find_map(|unit| access_unit_ptl(plan.codec, &unit.data)),
        dropped: 0,
        failed: 0,
        load: load_average(),
    };
    let numbers: Vec<u64> = run.outputs.iter().map(|o| o.frame_number).collect();
    let skipped: Vec<u64> = numbers
        .windows(2)
        .filter(|pair| pair[1] - pair[0] > 1)
        .map(|pair| pair[0] + 1)
        .collect();
    eprintln!(
        "LOOP {label}: {} frames out, first {:?}, gaps after {skipped:?}",
        numbers.len(),
        numbers.first()
    );
    assert_decoded(
        &format!("{label} videotoolbox"),
        &run,
        &videotoolbox_decode(&run),
    );
    assert_decoded(&format!("{label} ffmpeg"), &run, &ffmpeg_decode(&run));
}

fn level_after_increase(label: &str, codec: Codec, from: u32, to: u32) -> (u8, u8) {
    let (units, change, _) = run_loop_with_bitrate_increase(codec, (1280, 720), 30, 4.0, from, to);
    let plan = plan_session(codec, 1280, 720, 30, to, Limits::default()).unwrap();
    assert_loop_stream_decodes(&format!("loop {label}"), plan, &units);
    let levels: Vec<(u64, ProfileTierLevel)> = units
        .iter()
        .filter(|unit| unit.is_keyframe)
        .filter_map(|unit| Some((unit.sequence, access_unit_ptl(codec, &unit.data)?)))
        .collect();
    eprintln!(
        "LEVELS {label} load=[{}] change_at={change} {:?}",
        load_average(),
        levels
            .iter()
            .map(|(frame, ptl)| format!("{frame}:{}", ptl.describe(codec)))
            .collect::<Vec<_>>()
    );
    let before = levels.first().expect("a first keyframe").1.level_idc;
    let after = levels
        .iter()
        .rfind(|(frame, _)| *frame > change)
        .expect("a keyframe after the change")
        .1
        .level_idc;
    (before, after)
}

/// Sol's repro: H.264 1280x720 at 30 fps, 15 Mbps raised to 50 live. Level
/// 3.1 carries 17.5 Mbps; the stream must declare 4.1 after the raise.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn a_live_bitrate_increase_raises_the_h264_level() {
    let (before, after) = level_after_increase("h264", Codec::H264, 15_000_000, 50_000_000);
    assert_eq!(before, 31);
    assert!(after >= 41, "SPS declares level_idc {after} at 50 Mbps");
}

/// HEVC at 1280x720 and 30 fps: AutoLevel writes 4.1 at 15 Mbps, which
/// carries 20; 30 Mbps needs 5.1.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn a_live_bitrate_increase_raises_the_hevc_level() {
    let (before, after) = level_after_increase("hevc", Codec::Hevc, 15_000_000, 30_000_000);
    assert_eq!(before, 123);
    assert!(after >= 153, "VPS declares level_idc {after} at 30 Mbps");
}

/// The encode loop publishes the rate it plans: a 120 Hz request at full
/// size runs at 60 on Main tier, and that is what the wire advertises.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn the_planned_rate_is_published_and_the_stream_decodes() {
    let (units, change, shared) =
        run_loop_with_bitrate_increase(Codec::Hevc, FULL, 120, 3.0, 15_000_000, 30_000_000);
    assert_eq!(
        shared.active_fps.load(std::sync::atomic::Ordering::SeqCst),
        60
    );
    assert_eq!(shared.advertised_fps(), 60);
    let plan = plan_session(
        Codec::Hevc,
        FULL.0,
        FULL.1,
        120,
        30_000_000,
        Limits::default(),
    )
    .unwrap();
    // Thinned to 60, and capped further by how fast this test can copy
    // 20 MB frames: what matters is that frames span the change.
    assert!(units.len() >= 30, "{} frames", units.len());
    assert!(units.iter().any(|unit| unit.sequence > change));
    assert_loop_stream_decodes("loop full_120", plan, &units);
}

/// Sessions lost mid-encode, over and over: each one's callback state is
/// freed once the session is dropped, the registry is back where it began,
/// and a late callback for a dead session is a no-op.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn lost_sessions_free_their_callback_state() {
    let plan = plan_session(Codec::Hevc, 640, 360, 60, 4_000_000, Limits::default()).unwrap();
    let mut frame = vec![0u8; 640 * 360 * 4];
    render_synthetic_frame(&mut frame, 640, 360, 1);
    let baseline = registered_callbacks();
    let mut probes = Vec::new();
    for round in 0..200u64 {
        let output = OutputQueue::new();
        let mut session = VtSession::new(plan, 4_000_000, Tuning::default(), output).unwrap();
        // Frames in flight when the session dies.
        for i in 0..3 {
            session
                .encode_bgra(
                    &frame,
                    640 * 4,
                    FrameInfo {
                        captured: Instant::now(),
                        frame_number: round * 10 + i,
                    },
                    i == 0,
                )
                .unwrap();
        }
        session.invalidate_for_test();
        let error = session
            .encode_bgra(
                &frame,
                640 * 4,
                FrameInfo {
                    captured: Instant::now(),
                    frame_number: round * 10 + 9,
                },
                false,
            )
            .unwrap_err();
        assert!(matches!(error, VtError::SessionLost(_)), "{error:?}");
        assert!(session.is_lost());
        let probe = session.callback_probe();
        drop(session);
        assert!(!probe.is_registered());
        probe.late_callback(0);
        probe.late_callback(ffi::VT_INVALID_SESSION);
        probes.push(probe);
    }
    // A VideoToolbox callback still running at teardown holds its own clone
    // for the length of that call.
    let deadline = Instant::now() + Duration::from_secs(2);
    while probes.iter().any(|probe| !probe.is_freed()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let leaked = probes.iter().filter(|probe| !probe.is_freed()).count();
    eprintln!(
        "LOST 200 sessions: leaked={leaked} registry={} baseline={baseline}",
        registered_callbacks()
    );
    assert_eq!(leaked, 0);
    assert_eq!(registered_callbacks(), baseline);
}

/// The profile, tier and level VideoToolbox writes in the first keyframe of
/// a session prepared at `bitrate`, at exactly `fps`.
fn emitted_ptl(size: (u32, u32), fps: u32, bitrate: u32) -> ProfileTierLevel {
    let plan = SessionPlan {
        encode_fps: fps,
        target_fps: fps,
        level_bitrate_bps: bitrate,
        max_bitrate_bps: bitrate,
        ..plan_session(Codec::Hevc, size.0, size.1, 1, bitrate, Limits::default()).unwrap()
    };
    let output = OutputQueue::new();
    let mut session = VtSession::new(plan, bitrate, Tuning::default(), output).unwrap();
    let mut frame = vec![0u8; size.0 as usize * size.1 as usize * 4];
    render_synthetic_frame(&mut frame, size.0, size.1, 3);
    for i in 0..2 {
        session
            .encode_bgra(
                &frame,
                size.0 as usize * 4,
                FrameInfo {
                    captured: Instant::now(),
                    frame_number: i,
                },
                i == 0,
            )
            .unwrap();
    }
    session.flush().unwrap();
    session.emitted_level().expect("a keyframe")
}

/// Where HEVC AutoLevel lands, by size, rate and bitrate: the data behind
/// `config::VT_HEVC_MAIN_TIER_MAX_LUMA_RATE` and `config::tests::VT_HEVC_SURVEY`.
/// Prints one SURVEY line per session and checks the planner's prediction.
#[test]
#[ignore = "needs a hardware VideoToolbox encoder"]
fn hevc_auto_level_survey() {
    let mut mismatches = Vec::new();
    for (size, rates) in [
        ((1280, 720), &[30, 60, 120, 240][..]),
        ((1920, 1080), &[60, 90, 120, 240][..]),
        (OUTER, &[60, 100, 120][..]),
        ((2460, 1640), &[120][..]),
        ((2500, 1700), &[120][..]),
        (FULL, &[30, 60, 90, 98, 99, 100, 120][..]),
    ] {
        for &fps in rates {
            for mbps in [4, 12, 13, 15, 26, 30, 39, 41, 50] {
                let bitrate = mbps * 1_000_000;
                let written = emitted_ptl(size, fps, bitrate);
                let tier = super::config::vt_hevc_tier(size.0, size.1, fps);
                let predicted =
                    super::config::min_level(Codec::Hevc, tier, size.0, size.1, fps, bitrate)
                        .unwrap();
                let matches =
                    written.level_idc == predicted.idc && written.high_tier == (tier == Tier::High);
                eprintln!(
                    "SURVEY {}x{} fps={fps} mbps={mbps} -> {} predicted {:?} {}{}",
                    size.0,
                    size.1,
                    written.describe(Codec::Hevc),
                    tier,
                    predicted.name,
                    if matches { "" } else { " MISMATCH" }
                );
                if !matches {
                    mismatches.push((size, fps, mbps));
                }
            }
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:?}");
}
