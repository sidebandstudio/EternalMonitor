//! C ABI over `eternal_host::encoder::videotoolbox`, so the Swift probe can
//! feed ScreenCaptureKit's IOSurface-backed pixel buffers straight into the
//! host's direct encoder (`VtSession::encode_pixel_buffer`, zero copy). The
//! header is `../Sources/ProbeBridge.h`.
#![cfg(target_os = "macos")]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void, CString};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_host::encoder::videotoolbox::config::{plan_session, Codec, HevcLimit, Limits, Tier};
use eternal_host::encoder::videotoolbox::session::{
    EncodedFrame, FrameInfo, OutputQueue, PixelBuffer, Tuning, VtSession,
};

extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

#[repr(C)]
pub struct EmPlan {
    pub encode_fps: u32,
    pub level_idc: u8,
    pub high_tier: bool,
    pub max_bitrate_bps: u32,
}

#[repr(C)]
pub struct EmFrame {
    /// Valid until the next `em_encoder_next` or `em_encoder_destroy`.
    pub data: *const u8,
    pub len: usize,
    pub frame_number: u64,
    pub encode_us: u64,
    pub keyframe: bool,
}

#[repr(C)]
pub struct EmLevel {
    pub valid: bool,
    pub profile_idc: u8,
    pub high_tier: bool,
    pub level_idc: u8,
}

pub struct ProbeEncoder {
    session: VtSession,
    output: Arc<OutputQueue>,
    current: Option<EncodedFrame>,
    error: CString,
}

fn write_error(buffer: *mut c_char, capacity: usize, message: &str) {
    if buffer.is_null() || capacity == 0 {
        return;
    }
    let bytes = message.as_bytes();
    let n = bytes.len().min(capacity - 1);
    // SAFETY: the caller passes a writable buffer of `capacity` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast(), n);
        *buffer.add(n) = 0;
    }
}

/// Plans and opens a hardware session. `experimental` allows HEVC High tier
/// and level 5.2 (`ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER`). Returns null and
/// writes a message into `error` on failure.
#[no_mangle]
pub unsafe extern "C" fn em_encoder_create(
    hevc: bool,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u32,
    experimental: bool,
    plan_out: *mut EmPlan,
    error: *mut c_char,
    error_capacity: usize,
) -> *mut ProbeEncoder {
    let codec = if hevc { Codec::Hevc } else { Codec::H264 };
    let limits = Limits {
        hevc: if experimental {
            HevcLimit::ExperimentalHighTier
        } else {
            HevcLimit::MainTier
        },
        ..Limits::default()
    };
    let plan = match plan_session(codec, width, height, fps, bitrate_bps, limits) {
        Ok(plan) => plan,
        Err(e) => {
            write_error(error, error_capacity, &e.to_string());
            return std::ptr::null_mut();
        }
    };
    let output = OutputQueue::new();
    match VtSession::new(plan, bitrate_bps, Tuning::default(), output.clone()) {
        Ok(session) => {
            if !plan_out.is_null() {
                *plan_out = EmPlan {
                    encode_fps: plan.encode_fps,
                    level_idc: plan.level.idc,
                    high_tier: plan.tier == Tier::High,
                    max_bitrate_bps: plan.max_bitrate_bps,
                };
            }
            Box::into_raw(Box::new(ProbeEncoder {
                session,
                output,
                current: None,
                error: CString::default(),
            }))
        }
        Err(e) => {
            write_error(error, error_capacity, &e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Encodes a `CVPixelBufferRef` as is (the caller keeps its reference).
/// Returns 0, or -1 with `em_encoder_error` describing why.
#[no_mangle]
pub unsafe extern "C" fn em_encoder_encode(
    encoder: *mut ProbeEncoder,
    pixel_buffer: *mut c_void,
    frame_number: u64,
    force_keyframe: bool,
) -> i32 {
    let encoder = &mut *encoder;
    CFRetain(pixel_buffer);
    let buffer = PixelBuffer::from_owned(pixel_buffer.cast());
    let info = FrameInfo {
        captured: Instant::now(),
        frame_number,
    };
    match encoder
        .session
        .encode_pixel_buffer(&buffer, info, force_keyframe)
    {
        Ok(()) => 0,
        Err(e) => {
            encoder.error = CString::new(e.to_string()).unwrap_or_default();
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn em_encoder_error(encoder: *const ProbeEncoder) -> *const c_char {
    (*encoder).error.as_ptr()
}

/// The next encoded access unit (Annex B, parameter sets on keyframes).
#[no_mangle]
pub unsafe extern "C" fn em_encoder_next(encoder: *mut ProbeEncoder, frame: *mut EmFrame) -> bool {
    let encoder = &mut *encoder;
    encoder.current = encoder.output.pop_timeout(Duration::ZERO);
    let Some(current) = encoder.current.as_ref() else {
        return false;
    };
    *frame = EmFrame {
        data: current.data.as_ptr(),
        len: current.data.len(),
        frame_number: current.frame_number,
        encode_us: current.encode_us as u64,
        keyframe: current.keyframe,
    };
    true
}

#[no_mangle]
pub unsafe extern "C" fn em_encoder_in_flight(encoder: *const ProbeEncoder) -> u32 {
    (*encoder).session.in_flight() as u32
}

/// Profile, tier and level from the first keyframe's VPS or SPS.
#[no_mangle]
pub unsafe extern "C" fn em_encoder_emitted(encoder: *const ProbeEncoder) -> EmLevel {
    match (*encoder).session.emitted_level() {
        Some(level) => EmLevel {
            valid: true,
            profile_idc: level.profile_idc,
            high_tier: level.high_tier,
            level_idc: level.level_idc,
        },
        None => EmLevel {
            valid: false,
            profile_idc: 0,
            high_tier: false,
            level_idc: 0,
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn em_encoder_flush(encoder: *mut ProbeEncoder) -> i32 {
    match (*encoder).session.flush() {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn em_encoder_destroy(encoder: *mut ProbeEncoder) {
    if !encoder.is_null() {
        drop(Box::from_raw(encoder));
    }
}
