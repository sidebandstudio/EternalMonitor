//! Spike a, direct path: a `VTCompressionSession` without FFmpeg, to test
//! what FFmpeg 7.1 cannot set: low-latency rate control for HEVC,
//! `MaxFrameDelayCount`, `ExpectedFrameRate`, zero-copy IOSurface input,
//! and a live bitrate change on an open session.
//!
//! cargo run --release -p eternal-mac-spikes --example vt_direct_session -- \
//!   --codec hevc --size inner --fps 120 --llrc 1 --seconds 10 \
//!   --change_at 5 --change_mbps 30

fn main() {
    #[cfg(target_os = "macos")]
    imp::main();
}

#[cfg(target_os = "macos")]
#[path = "common/mod.rs"]
mod common;

#[cfg(target_os = "macos")]
mod imp {
    use super::common::{self, bitstream, vt, Args};
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    struct Output {
        index: u64,
        at: Instant,
        bytes: usize,
        key: bool,
    }

    #[derive(Default)]
    struct Shared {
        submitted: Mutex<HashMap<u64, Instant>>,
        outputs: Mutex<Vec<Output>>,
        parameter_sets: Mutex<Option<Vec<Vec<u8>>>>,
        first_key: Mutex<Option<Vec<u8>>>,
        errors: Mutex<Vec<i32>>,
        /// Frames the encoder dropped: status 0, no sample,
        /// `kVTEncodeInfo_FrameDropped` set.
        dropped: Mutex<Vec<u64>>,
        hevc: bool,
    }

    extern "C" fn on_output(
        refcon: *mut c_void,
        frame_refcon: *mut c_void,
        status: vt::OSStatus,
        info: u32,
        sample: vt::CMSampleBufferRef,
    ) {
        let at = Instant::now();
        let shared = unsafe { &*(refcon as *const Shared) };
        let index = frame_refcon as u64;
        if status == 0 && (sample.is_null() || info & 0x2 != 0) {
            shared.dropped.lock().unwrap().push(index);
            return;
        }
        if status != 0 || sample.is_null() {
            shared.errors.lock().unwrap().push(status);
            return;
        }
        let key = vt::sample_is_keyframe(sample);
        let block = unsafe { vt::CMSampleBufferGetDataBuffer(sample) };
        let bytes = if block.is_null() {
            0
        } else {
            unsafe { vt::CMBlockBufferGetDataLength(block) }
        };
        if key && shared.parameter_sets.lock().unwrap().is_none() {
            *shared.parameter_sets.lock().unwrap() = Some(vt::parameter_sets(sample, shared.hevc));
            let mut data = vec![0u8; bytes];
            unsafe { vt::CMBlockBufferCopyDataBytes(block, 0, bytes, data.as_mut_ptr().cast()) };
            *shared.first_key.lock().unwrap() = Some(data);
        }
        shared.outputs.lock().unwrap().push(Output {
            index,
            at,
            bytes,
            key,
        });
    }

    fn key(k: CFStringRef) -> CFString {
        unsafe { CFString::wrap_under_get_rule(k) }
    }

    fn set(session: vt::VTCompressionSessionRef, name: &str, k: CFStringRef, value: &CFType) {
        let started = Instant::now();
        let status = unsafe { vt::VTSessionSetProperty(session, k, value.as_CFTypeRef()) };
        println!(
            "set {name} status={status} took_us={}",
            started.elapsed().as_micros()
        );
    }

    pub fn main() {
        let args = Args::from_env();
        let hevc = args.or("codec", "hevc") == "hevc";
        let (width, height) = common::parse_size(args.or("size", "inner"));
        let fps: u32 = args.num("fps", 120);
        let mbps: f64 = args.num("mbps", 15.0);
        let llrc = args.or("llrc", "0") == "1";
        let realtime = args.or("realtime", "1") == "1";
        let seconds: f64 = args.num("seconds", 10.0);
        let change_at: f64 = args.num("change_at", 0.0);
        let change_mbps: f64 = args.num("change_mbps", mbps * 2.0);
        let keyframe_every: f64 = args.num("keyframe_every", 0.0);
        let gop: i32 = args.num("gop", 100_000);
        let bgra = args.or("pix", "nv12") == "bgra";
        let throughput = args.or("mode", "paced") == "throughput";
        let label = args.or("label", "");
        println!(
            "config label={label} codec={} size={width}x{height} fps={fps} mbps={mbps} llrc={llrc} realtime={realtime} pix={} mode={} gop={gop}",
            if hevc { "hevc" } else { "h264" },
            if bgra { "bgra" } else { "nv12" },
            if throughput { "throughput" } else { "paced" },
        );
        println!("state start {}", common::machine_state());

        let shared = Box::leak(Box::new(Shared {
            hevc,
            ..Default::default()
        }));

        let mut spec: Vec<(CFString, CFType)> = vec![(
            key(unsafe { vt::kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder }),
            CFBoolean::true_value().as_CFType(),
        )];
        if llrc {
            spec.push((
                key(unsafe { vt::kVTVideoEncoderSpecification_EnableLowLatencyRateControl }),
                CFBoolean::true_value().as_CFType(),
            ));
        }
        let spec = CFDictionary::from_CFType_pairs(&spec);
        let empty: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[]);
        let attrs = CFDictionary::from_CFType_pairs(&[
            (
                key(unsafe { vt::kCVPixelBufferPixelFormatTypeKey }),
                CFNumber::from(if bgra {
                    vt::PIXEL_BGRA
                } else {
                    vt::PIXEL_NV12_VIDEO
                } as i32)
                .as_CFType(),
            ),
            (
                key(unsafe { vt::kCVPixelBufferWidthKey }),
                CFNumber::from(width as i32).as_CFType(),
            ),
            (
                key(unsafe { vt::kCVPixelBufferHeightKey }),
                CFNumber::from(height as i32).as_CFType(),
            ),
            (
                key(unsafe { vt::kCVPixelBufferIOSurfacePropertiesKey }),
                empty.as_CFType(),
            ),
        ]);

        let mut session: vt::VTCompressionSessionRef = std::ptr::null_mut();
        let created = Instant::now();
        let status = unsafe {
            vt::VTCompressionSessionCreate(
                std::ptr::null(),
                width as i32,
                height as i32,
                if hevc { vt::CODEC_HEVC } else { vt::CODEC_H264 },
                spec.as_concrete_TypeRef().cast(),
                attrs.as_concrete_TypeRef().cast(),
                std::ptr::null(),
                Some(on_output),
                (shared as *const Shared).cast_mut().cast(),
                &mut session,
            )
        };
        println!(
            "create status={status} took_ms={:.1}",
            created.elapsed().as_secs_f64() * 1000.0
        );
        if status != 0 {
            println!("RESULT label={label} status=create_failed code={status}");
            return;
        }

        let profile = unsafe {
            if hevc {
                vt::kVTProfileLevel_HEVC_Main_AutoLevel
            } else {
                vt::kVTProfileLevel_H264_Baseline_AutoLevel
            }
        };
        unsafe {
            set(
                session,
                "RealTime",
                vt::kVTCompressionPropertyKey_RealTime,
                &CFBoolean::from(realtime).as_CFType(),
            );
            set(
                session,
                "AllowFrameReordering",
                vt::kVTCompressionPropertyKey_AllowFrameReordering,
                &CFBoolean::false_value().as_CFType(),
            );
            set(
                session,
                "ProfileLevel",
                vt::kVTCompressionPropertyKey_ProfileLevel,
                &key(profile).as_CFType(),
            );
            set(
                session,
                "AverageBitRate",
                vt::kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::from((mbps * 1e6) as i64).as_CFType(),
            );
            set(
                session,
                "ExpectedFrameRate",
                vt::kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::from(fps as i32).as_CFType(),
            );
            set(
                session,
                "MaxKeyFrameInterval",
                vt::kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::from(gop).as_CFType(),
            );
            if let Some(speed) = args.get("prio_speed") {
                set(
                    session,
                    "PrioritizeEncodingSpeedOverQuality",
                    vt::kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                    &CFBoolean::from(speed == "1").as_CFType(),
                );
            }
            if let Some(delay) = args.get("max_delay") {
                set(
                    session,
                    "MaxFrameDelayCount",
                    vt::kVTCompressionPropertyKey_MaxFrameDelayCount,
                    &CFNumber::from(delay.parse::<i32>().unwrap()).as_CFType(),
                );
            }
            println!(
                "prepare status={}",
                vt::VTCompressionSessionPrepareToEncodeFrames(session)
            );
        }
        for line in vt::describe_session(session) {
            println!("open session {line}");
        }

        // Fill a few IOSurface buffers once and reuse them: the zero-copy
        // shape a ScreenCaptureKit source would have.
        let pool = unsafe { vt::VTCompressionSessionGetPixelBufferPool(session) };
        assert!(!pool.is_null(), "no pixel buffer pool");
        let buffers: Vec<vt::CVPixelBufferRef> = (0..8)
            .map(|n| unsafe {
                let mut buffer = std::ptr::null_mut();
                assert_eq!(
                    vt::CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut buffer),
                    0
                );
                vt::CVPixelBufferLockBaseAddress(buffer, 0);
                let planes = if bgra { 1 } else { 2 };
                for plane in 0..planes {
                    let base = vt::CVPixelBufferGetBaseAddressOfPlane(buffer, plane);
                    let stride = vt::CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
                    let rows = vt::CVPixelBufferGetHeightOfPlane(buffer, plane);
                    let data = std::slice::from_raw_parts_mut(base, stride * rows);
                    let (w, h) = (width as usize, height as usize);
                    match (bgra, plane) {
                        (true, _) => common::fill_bgra(data, stride, w, h, n),
                        (false, 0) => common::fill_luma(data, stride, w, h, n),
                        _ => common::fill_chroma(data, stride, w, h / 2, n),
                    }
                }
                vt::CVPixelBufferUnlockBaseAddress(buffer, 0);
                buffer
            })
            .collect();

        let force_key = CFDictionary::from_CFType_pairs(&[(
            key(unsafe { vt::kVTEncodeFrameOptionKey_ForceKeyFrame }),
            CFBoolean::true_value().as_CFType(),
        )]);
        let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        let total = (seconds * f64::from(fps)).round() as u64;
        let change_frame = (change_at * f64::from(fps)).round() as u64;
        let keyframe_period = (keyframe_every * f64::from(fps)).round() as u64;
        let mut requested_keys = Vec::new();
        let mut encode_call_us = Vec::new();
        let started = Instant::now();
        for i in 0..total {
            if !throughput {
                common::sleep_until(started + interval * i as u32);
            }
            if change_frame > 0 && i == change_frame {
                set(
                    session,
                    "AverageBitRate(live)",
                    unsafe { vt::kVTCompressionPropertyKey_AverageBitRate },
                    &CFNumber::from((change_mbps * 1e6) as i64).as_CFType(),
                );
            }
            let keyframe = keyframe_period > 0 && i > 0 && i % keyframe_period == 0;
            if keyframe {
                requested_keys.push(i);
            }
            let call = Instant::now();
            shared.submitted.lock().unwrap().insert(i, call);
            let status = unsafe {
                vt::VTCompressionSessionEncodeFrame(
                    session,
                    buffers[(i % 8) as usize],
                    vt::CMTime::micros((i as f64 * 1e6 / f64::from(fps)) as i64),
                    vt::CMTime::INVALID,
                    if keyframe {
                        force_key.as_concrete_TypeRef().cast()
                    } else {
                        std::ptr::null()
                    },
                    i as *mut c_void,
                    std::ptr::null_mut(),
                )
            };
            encode_call_us.push(call.elapsed().as_micros() as u64);
            assert_eq!(status, 0, "encode frame {i}");
        }
        let submitted_all = started.elapsed();
        unsafe { vt::VTCompressionSessionCompleteFrames(session, vt::CMTime::INVALID) };
        let wall = started.elapsed().as_secs_f64();
        for line in vt::describe_session(session) {
            println!("end session {line}");
        }
        println!("state end {}", common::machine_state());

        let outputs = shared.outputs.lock().unwrap();
        let submitted = shared.submitted.lock().unwrap();
        let latency_of = |o: &Output| (o.at - submitted[&o.index]).as_micros() as u64;
        let latencies: Vec<u64> = outputs.iter().map(latency_of).collect();
        let key_latencies: Vec<u64> = outputs
            .iter()
            .filter(|o| requested_keys.contains(&o.index))
            .map(|o| {
                assert!(o.key, "requested keyframe {} came out as a delta", o.index);
                latency_of(o)
            })
            .collect();
        // One-second buckets by frame index: bitrate before and after a live change.
        let per_second: Vec<String> = outputs
            .chunks(fps as usize)
            .map(|chunk| {
                let bytes: usize = chunk.iter().map(|o| o.bytes).sum();
                let keys = chunk.iter().filter(|o| o.key).count();
                format!(
                    "{:.1}{}",
                    bytes as f64 * 8.0 / 1e6,
                    if keys > 0 { "k" } else { "" }
                )
            })
            .collect();
        let unexpected_keys: Vec<u64> = outputs
            .iter()
            .filter(|o| o.key && o.index != 0 && !requested_keys.contains(&o.index))
            .map(|o| o.index)
            .collect();
        if let Some(sets) = shared.parameter_sets.lock().unwrap().as_ref() {
            for pl in bitstream::describe_parameter_sets(sets, hevc) {
                println!(
                    "bitstream {} profile=\"{}\" level={} level_idc={}",
                    pl.nal, pl.profile, pl.level, pl.level_idc
                );
            }
        }
        let (name, idc, _) = if hevc {
            bitstream::hevc_min_level(width, height, fps)
        } else {
            bitstream::h264_min_level(width, height, fps)
        };
        println!("bitstream required_level={name} required_idc={idc}");
        // One guard for both counts: a second lock inside the same
        // statement would deadlock on the first's temporary.
        let dropped = shared.dropped.lock().unwrap().clone();
        let dropped_key_requests = requested_keys
            .iter()
            .filter(|i| dropped.contains(i))
            .count();
        println!(
            "RESULT label={label} status=ok frames={total} packets={} errors={:?} dropped={} dropped_key_requests={} wall_s={wall:.2} submit_s={:.2} out_fps={:.1} latency[{}] encode_call[{}] keyframe_req[{}] unexpected_keys={:?} mbps_per_s=[{}]",
            outputs.len(),
            shared.errors.lock().unwrap(),
            dropped.len(),
            dropped_key_requests,
            submitted_all.as_secs_f64(),
            outputs.len() as f64 / wall,
            common::summarize(latencies),
            common::summarize(encode_call_us),
            common::summarize(key_latencies),
            unexpected_keys,
            per_second.join(" ")
        );
        unsafe { vt::VTCompressionSessionInvalidate(session) };
    }
}
