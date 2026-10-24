//! One hardware `VTCompressionSession`: created from a [`SessionPlan`],
//! tuned for real-time streaming, fed BGRA frames (converted into the
//! session's own IOSurface pool) or ready-made pixel buffers, and drained
//! through [`OutputQueue`] as Annex B access units.
//!
//! Reading the framework's exported `CFStringRef` constants (`ffi::kVT…`)
//! needs `unsafe` but is always sound: they are immutable statics the
//! frameworks initialise before `main`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use parking_lot::{Condvar, Mutex};
use tracing::{info, warn};

use super::bitstream::{self, ProfileTierLevel};
use super::config::{self, classify_status, Codec, SessionPlan, StatusClass};
use super::ffi;
use crate::encoder::vimage::{Nv12Converter, Plane};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VtError {
    /// No hardware encoder for this codec and size. The host never falls
    /// back to a software encoder on macOS.
    NoHardware(String),
    /// The session was invalidated (sleep, media server reset): make a new one.
    SessionLost(i32),
    /// The encoder is briefly unavailable: make a new one after a pause.
    RetryLater(i32),
    Failed {
        call: &'static str,
        status: i32,
    },
    /// The frame cannot be encoded as given (size or format mismatch).
    Input(String),
}

impl VtError {
    fn from_status(call: &'static str, status: i32) -> Self {
        match classify_status(status) {
            StatusClass::SessionLost => Self::SessionLost(status),
            StatusClass::RetryLater => Self::RetryLater(status),
            StatusClass::NoHardware => Self::NoHardware(format!(
                "{call} found no hardware video encoder (status {status})"
            )),
            StatusClass::Ok | StatusClass::Failed => Self::Failed { call, status },
        }
    }
}

impl std::fmt::Display for VtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHardware(detail) => write!(
                f,
                "VideoToolbox hardware encoding is unavailable ({detail}); the macOS host has no software encoder"
            ),
            Self::SessionLost(status) => {
                write!(f, "VideoToolbox session was invalidated (status {status})")
            }
            Self::RetryLater(status) => {
                write!(f, "VideoToolbox encoder is not available now (status {status})")
            }
            Self::Failed { call, status } => write!(f, "{call} failed with status {status}"),
            Self::Input(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for VtError {}

/// Encoder knobs outside the plan. The defaults are what the host ships;
/// `ETERNAL_VT_*` variables override them for measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuning {
    pub real_time: bool,
    pub prioritize_speed: bool,
    /// `MaxFrameDelayCount`; `None` leaves VideoToolbox's default (unlimited).
    pub max_frame_delay: Option<i32>,
    /// Also cap bursts with `DataRateLimits`. Off by default: on an M1 Max
    /// any limit (1x-4x the average, 0.25-4 s windows) cut HEVC at 2670x1878
    /// from 119 to 63-85 fps, doubled latency to ~100 ms, and overshot the
    /// average bitrate by a quarter (`hardware_tests`).
    pub data_rate_limit: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            // RealTime=true lets VideoToolbox pace itself to ExpectedFrameRate.
            // On an M1 Max at 2670x1878 and 120 fps that measured p50 39-62 ms
            // and fell to 92 fps under load; RealTime=false held 120.0 fps at
            // p50 15.5 ms / p99 72 ms over 60 s at load 470 (`hardware_tests`).
            real_time: false,
            prioritize_speed: true,
            max_frame_delay: None,
            data_rate_limit: false,
        }
    }
}

impl Tuning {
    pub fn from_env() -> Self {
        let mut tuning = Self::default();
        let flag = |name: &str| std::env::var(name).ok().map(|value| value.trim() == "1");
        if let Some(real_time) = flag("ETERNAL_VT_REALTIME") {
            tuning.real_time = real_time;
        }
        if let Some(limit) = flag("ETERNAL_VT_DATA_RATE_LIMIT") {
            tuning.data_rate_limit = limit;
        }
        if let Some(speed) = flag("ETERNAL_VT_PRIORITIZE_SPEED") {
            tuning.prioritize_speed = speed;
        }
        if let Some(delay) = std::env::var("ETERNAL_VT_MAX_FRAME_DELAY")
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok())
        {
            tuning.max_frame_delay = (delay >= 0).then_some(delay);
        }
        tuning
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FrameInfo {
    pub captured: Instant,
    pub frame_number: u64,
}

/// One encoded access unit, Annex B, parameter sets first on keyframes.
#[derive(Debug)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub keyframe_requested: bool,
    pub captured: Instant,
    pub frame_number: u64,
    /// From the start of the encode call (including the BGRA conversion)
    /// to the callback that delivered this frame.
    pub encode_us: u128,
    pub delivered: Instant,
}

/// Where sessions deliver frames. One queue outlives the sessions of an
/// encode loop, so frames a closing session flushes stay ahead of the next
/// session's first keyframe.
pub struct OutputQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

#[derive(Default)]
struct QueueState {
    frames: VecDeque<EncodedFrame>,
    closed: bool,
}

impl OutputQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(QueueState::default()),
            changed: Condvar::new(),
        })
    }

    fn push(&self, frame: EncodedFrame) {
        self.state.lock().frames.push_back(frame);
        self.changed.notify_all();
    }

    /// Blocks for the next frame; `None` once closed and drained.
    pub fn pop_blocking(&self) -> Option<EncodedFrame> {
        let mut state = self.state.lock();
        loop {
            if let Some(frame) = state.frames.pop_front() {
                drop(state);
                self.changed.notify_all();
                return Some(frame);
            }
            if state.closed {
                return None;
            }
            self.changed.wait(&mut state);
        }
    }

    pub fn pop_timeout(&self, timeout: Duration) -> Option<EncodedFrame> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        loop {
            if let Some(frame) = state.frames.pop_front() {
                drop(state);
                self.changed.notify_all();
                return Some(frame);
            }
            if state.closed || self.changed.wait_until(&mut state, deadline).timed_out() {
                return state.frames.pop_front();
            }
        }
    }

    /// Encoded frames waiting for the forwarder.
    pub fn queued(&self) -> usize {
        self.state.lock().frames.len()
    }

    /// Waits up to `timeout` for a push or a pop.
    pub fn wait_for_change(&self, timeout: Duration) {
        let mut state = self.state.lock();
        let _ = self.changed.wait_for(&mut state, timeout);
    }

    pub fn close(&self) {
        self.state.lock().closed = true;
        self.changed.notify_all();
    }
}

/// An owned (+1) `CVPixelBuffer`: the session pool's buffers now, and
/// ScreenCaptureKit's later.
pub struct PixelBuffer(ffi::CVPixelBufferRef);

// SAFETY: CVPixelBuffer is a reference-counted CF object that may be
// retained, released and read from any thread.
unsafe impl Send for PixelBuffer {}

impl PixelBuffer {
    /// # Safety
    /// `raw` must be a valid, non-null `CVPixelBuffer` whose +1 reference the
    /// caller hands over; it is released on drop.
    pub unsafe fn from_owned(raw: ffi::CVPixelBufferRef) -> Self {
        Self(raw)
    }

    pub fn width(&self) -> usize {
        // SAFETY: self.0 is a valid pixel buffer for our lifetime.
        unsafe { ffi::CVPixelBufferGetWidth(self.0) }
    }

    pub fn height(&self) -> usize {
        // SAFETY: as above.
        unsafe { ffi::CVPixelBufferGetHeight(self.0) }
    }
}

impl Drop for PixelBuffer {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference.
        unsafe { ffi::CFRelease(self.0 as CFTypeRef) };
    }
}

/// Frame bookkeeping, keyed by the token passed as the frame's refcon.
struct Pending {
    info: FrameInfo,
    started: Instant,
    keyframe_requested: bool,
}

/// Callback states by session ID. VideoToolbox gets the ID as the output
/// callback's refcon instead of a pointer. A callback looks its state up
/// here and clones the `Arc`; `VtSession`'s Drop removes the entry, so a
/// late callback from a lost session finds nothing and returns, and the
/// state is freed with its last clone.
static CALLBACKS: Mutex<BTreeMap<u64, Arc<CallbackState>>> = Mutex::new(BTreeMap::new());
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn callback_state(session_id: u64) -> Option<Arc<CallbackState>> {
    CALLBACKS.lock().get(&session_id).cloned()
}

/// State the output callback reaches through its session ID.
struct CallbackState {
    codec: Codec,
    output: Arc<OutputQueue>,
    pending: Mutex<HashMap<u64, Pending>>,
    /// A callback reported the session invalidated.
    lost: AtomicBool,
    /// A frame failed or a requested keyframe was dropped: ask again.
    rekey: AtomicBool,
    dropped: AtomicU64,
    failed: AtomicU64,
    last_failure: AtomicI32,
    emitted: Mutex<Option<ProfileTierLevel>>,
}

impl CallbackState {
    fn on_frame(&self, token: u64, status: i32, flags: u32, sample: ffi::CMSampleBufferRef) {
        let Some(pending) = self.pending.lock().remove(&token) else {
            return;
        };
        if status != 0 {
            // A frame missing from the stream can break the reference
            // chain, so the next frame is an IDR.
            self.failed.fetch_add(1, Ordering::Relaxed);
            self.last_failure.store(status, Ordering::Relaxed);
            self.rekey.store(true, Ordering::Relaxed);
            if classify_status(status) == StatusClass::SessionLost {
                self.lost.store(true, Ordering::Relaxed);
            }
            warn!(
                status,
                frame = pending.info.frame_number,
                "VideoToolbox failed to encode a frame"
            );
            return;
        }
        if sample.is_null() || flags & ffi::ENCODE_INFO_FRAME_DROPPED != 0 {
            // A dropped frame is never referenced, so the stream stays
            // decodable; only a keyframe asked for on it must move on.
            self.dropped.fetch_add(1, Ordering::Relaxed);
            if pending.keyframe_requested {
                self.rekey.store(true, Ordering::Relaxed);
            }
            return;
        }
        // SAFETY: VideoToolbox keeps `sample` valid for the callback.
        match unsafe { read_sample(self.codec, sample) } {
            Ok((data, keyframe, level)) => {
                if let Some(level) = level {
                    self.emitted.lock().get_or_insert(level);
                }
                let delivered = Instant::now();
                self.output.push(EncodedFrame {
                    data,
                    keyframe,
                    keyframe_requested: pending.keyframe_requested,
                    captured: pending.info.captured,
                    frame_number: pending.info.frame_number,
                    encode_us: delivered.duration_since(pending.started).as_micros(),
                    delivered,
                });
            }
            Err(error) => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                self.rekey.store(true, Ordering::Relaxed);
                warn!(%error, frame = pending.info.frame_number, "Discarding an unreadable encoded frame");
            }
        }
    }
}

extern "C" fn on_encoded(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: ffi::OSStatus,
    flags: u32,
    sample: ffi::CMSampleBufferRef,
) {
    // `refcon` is the session ID, an integer and never dereferenced.
    let Some(state) = callback_state(refcon as u64) else {
        return;
    };
    let token = frame_refcon as u64;
    // A panic must not unwind into VideoToolbox.
    if std::panic::catch_unwind(AssertUnwindSafe(|| {
        state.on_frame(token, status, flags, sample)
    }))
    .is_err()
    {
        state.failed.fetch_add(1, Ordering::Relaxed);
        state.rekey.store(true, Ordering::Relaxed);
    }
}

/// Copies a sample out as an Annex B access unit.
///
/// # Safety
/// `sample` must be a valid `CMSampleBuffer` for the duration of the call.
unsafe fn read_sample(
    codec: Codec,
    sample: ffi::CMSampleBufferRef,
) -> Result<(Vec<u8>, bool, Option<ProfileTierLevel>), String> {
    let keyframe = sample_is_sync(sample);
    let block = ffi::CMSampleBufferGetDataBuffer(sample);
    if block.is_null() {
        return Err("sample has no data buffer".into());
    }
    let length = ffi::CMBlockBufferGetDataLength(block);
    let mut data = vec![0u8; length];
    let status = ffi::CMBlockBufferCopyDataBytes(block, 0, length, data.as_mut_ptr().cast());
    if status != 0 {
        return Err(format!(
            "CMBlockBufferCopyDataBytes failed with status {status}"
        ));
    }
    let (sets, level) = if keyframe {
        let sets = parameter_sets(codec, ffi::CMSampleBufferGetFormatDescription(sample))?;
        let level = sets
            .iter()
            .find_map(|set| bitstream::parameter_set_ptl(codec, set));
        (sets, level)
    } else {
        (Vec::new(), None)
    };
    let unit = bitstream::access_unit(&sets, data).map_err(|error| error.to_string())?;
    Ok((unit, keyframe, level))
}

/// A sample with no `NotSync` attachment is a sync (key) frame.
///
/// # Safety
/// `sample` must be a valid `CMSampleBuffer`.
unsafe fn sample_is_sync(sample: ffi::CMSampleBufferRef) -> bool {
    let array = ffi::CMSampleBufferGetSampleAttachmentsArray(sample, 0);
    if array.is_null() {
        return true;
    }
    let array: CFArray<CFDictionary<CFString, CFType>> = CFArray::wrap_under_get_rule(array.cast());
    let Some(first) = array.get(0) else {
        return true;
    };
    let key = CFString::wrap_under_get_rule(ffi::kCMSampleAttachmentKey_NotSync);
    match first.find(&key) {
        Some(value) => value
            .downcast::<CFBoolean>()
            .is_none_or(|not_sync| !bool::from(not_sync)),
        None => true,
    }
}

/// VPS/SPS/PPS (HEVC) or SPS/PPS (H.264) of a sample's format.
///
/// # Safety
/// `description` must be null or a valid video format description.
unsafe fn parameter_sets(
    codec: Codec,
    description: ffi::CMFormatDescriptionRef,
) -> Result<Vec<Vec<u8>>, String> {
    if description.is_null() {
        return Err("keyframe has no format description".into());
    }
    let get = match codec {
        Codec::Hevc => ffi::CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
        Codec::H264 => ffi::CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    };
    let mut sets = Vec::new();
    let mut count = 1;
    let mut index = 0;
    while index < count {
        let mut data: *const u8 = std::ptr::null();
        let (mut size, mut header_length) = (0usize, 0i32);
        let status = get(
            description,
            index,
            &mut data,
            &mut size,
            &mut count,
            &mut header_length,
        );
        if status != 0 || data.is_null() {
            return Err(format!(
                "parameter set {index} unavailable (status {status})"
            ));
        }
        if header_length != 4 {
            return Err(format!("unexpected NAL length size {header_length}"));
        }
        sets.push(std::slice::from_raw_parts(data, size).to_vec());
        index += 1;
    }
    let expected = match codec {
        Codec::Hevc => 3,
        Codec::H264 => 2,
    };
    if sets.len() < expected {
        return Err(format!("keyframe has {} parameter sets", sets.len()));
    }
    Ok(sets)
}

fn cf_key(key: CFStringRef) -> CFString {
    // SAFETY: framework constants are valid for the life of the process.
    unsafe { CFString::wrap_under_get_rule(key) }
}

/// Counters a session keeps for the encode loop's stats and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionCounters {
    pub dropped: u64,
    pub failed: u64,
    pub last_failure: i32,
}

pub struct VtSession {
    raw: ffi::VTCompressionSessionRef,
    id: u64,
    state: Arc<CallbackState>,
    plan: SessionPlan,
    live_fps: u32,
    bitrate_bps: u32,
    data_rate_limit: bool,
    converter: Nv12Converter,
    next_token: u64,
    epoch: Option<Instant>,
    last_pts_us: i64,
    force_keyframe: CFDictionary<CFString, CFType>,
}

// SAFETY: a VTCompressionSession may be driven from any thread as long as
// calls are not concurrent; a VtSession is owned and called by one encode
// thread at a time.
unsafe impl Send for VtSession {}

impl VtSession {
    pub fn new(
        plan: SessionPlan,
        bitrate_bps: u32,
        tuning: Tuning,
        output: Arc<OutputQueue>,
    ) -> Result<Self, VtError> {
        let converter = Nv12Converter::new().map_err(VtError::Input)?;
        let true_value = CFBoolean::true_value().as_CFType();
        // SAFETY (all `unsafe` reads below): framework CFString constants.
        let spec = CFDictionary::from_CFType_pairs(&[(
            cf_key(unsafe {
                ffi::kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder
            }),
            true_value.clone(),
        )]);
        let no_options: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[]);
        let number = |value: i64| CFNumber::from(value).as_CFType();
        let source_attributes = CFDictionary::from_CFType_pairs(&[
            (
                cf_key(unsafe { ffi::kCVPixelBufferPixelFormatTypeKey }),
                // CPU-converted NV12 beat a BGRA pool that VideoToolbox
                // converts itself: 40 vs 49 ms p50 at full size and 120 fps.
                number(i64::from(ffi::PIXEL_FORMAT_NV12_VIDEO_RANGE)),
            ),
            (
                cf_key(unsafe { ffi::kCVPixelBufferWidthKey }),
                number(i64::from(plan.width)),
            ),
            (
                cf_key(unsafe { ffi::kCVPixelBufferHeightKey }),
                number(i64::from(plan.height)),
            ),
            (
                cf_key(unsafe { ffi::kCVPixelBufferIOSurfacePropertiesKey }),
                no_options.as_CFType(),
            ),
        ]);
        let (width, height) = (
            i32::try_from(plan.width).map_err(|_| VtError::Input("width too large".into()))?,
            i32::try_from(plan.height).map_err(|_| VtError::Input("height too large".into()))?,
        );
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(CallbackState {
            codec: plan.codec,
            output,
            pending: Mutex::new(HashMap::new()),
            lost: AtomicBool::new(false),
            rekey: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            last_failure: AtomicI32::new(0),
            emitted: Mutex::new(None),
        });
        CALLBACKS.lock().insert(id, state.clone());
        let mut raw: ffi::VTCompressionSessionRef = std::ptr::null_mut();
        // SAFETY: the dictionaries outlive the call. The refcon is the
        // session ID, an integer the callback looks up.
        let status = unsafe {
            ffi::VTCompressionSessionCreate(
                std::ptr::null(),
                width,
                height,
                match plan.codec {
                    Codec::Hevc => ffi::CODEC_HEVC,
                    Codec::H264 => ffi::CODEC_H264,
                },
                spec.as_concrete_TypeRef().cast(),
                source_attributes.as_concrete_TypeRef().cast(),
                std::ptr::null(),
                Some(on_encoded),
                id as *mut c_void,
                &mut raw,
            )
        };
        if status != 0 || raw.is_null() {
            CALLBACKS.lock().remove(&id);
            return Err(match classify_status(status) {
                StatusClass::NoHardware => VtError::NoHardware(format!(
                    "no hardware {} encoder for {}x{} (status {status})",
                    plan.codec.display_name(),
                    plan.width,
                    plan.height
                )),
                _ => VtError::from_status("VTCompressionSessionCreate", status),
            });
        }
        let force_keyframe = CFDictionary::from_CFType_pairs(&[(
            cf_key(unsafe { ffi::kVTEncodeFrameOptionKey_ForceKeyFrame }),
            true_value,
        )]);
        // From here on Drop tears the session down on any error.
        let mut session = Self {
            raw,
            id,
            state,
            plan,
            live_fps: plan.encode_fps,
            // Prepared at the level's bitrate; `set_bitrate` below moves it.
            bitrate_bps: plan.level_bitrate_bps,
            data_rate_limit: tuning.data_rate_limit,
            converter,
            next_token: 0,
            epoch: None,
            last_pts_us: -1,
            force_keyframe,
        };
        session.configure(tuning)?;
        let bitrate_bps = plan.clamp_bitrate(bitrate_bps);
        if bitrate_bps != session.bitrate_bps {
            session.set_bitrate(bitrate_bps)?;
        }
        if !session.copy_bool(unsafe {
            ffi::kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder
        }) {
            return Err(VtError::NoHardware(format!(
                "{} session is not hardware accelerated",
                plan.codec.display_name()
            )));
        }
        info!(
            codec = plan.codec.display_name(),
            width = plan.width,
            height = plan.height,
            target_fps = plan.target_fps,
            encode_fps = plan.encode_fps,
            required_tier = ?plan.tier,
            required_level = plan.level.name,
            level_bitrate_bps = plan.level_bitrate_bps,
            bitrate_bps,
            ?tuning,
            "Opened VideoToolbox session"
        );
        Ok(session)
    }

    fn configure(&mut self, tuning: Tuning) -> Result<(), VtError> {
        let plan = self.plan;
        // VideoToolbox has no explicit HEVC levels: AutoLevel picks the
        // level from the size, ExpectedFrameRate and AverageBitRate when the
        // session is prepared, which `plan_session` predicts.
        let profile = match plan.codec {
            Codec::Hevc => unsafe { ffi::kVTProfileLevel_HEVC_Main_AutoLevel },
            Codec::H264 => h264_profile_level(plan)?,
        };
        let yes = CFBoolean::true_value().as_CFType();
        let no = CFBoolean::false_value().as_CFType();
        let flag = |value: bool| if value { yes.clone() } else { no.clone() };
        let int = |value: i64| CFNumber::from(value).as_CFType();
        // Each property the stream depends on must be accepted.
        let mut required: Vec<(&str, CFStringRef, CFType)> = unsafe {
            vec![
                (
                    "RealTime",
                    ffi::kVTCompressionPropertyKey_RealTime,
                    flag(tuning.real_time),
                ),
                (
                    "AllowFrameReordering",
                    ffi::kVTCompressionPropertyKey_AllowFrameReordering,
                    no.clone(),
                ),
                (
                    "ProfileLevel",
                    ffi::kVTCompressionPropertyKey_ProfileLevel,
                    cf_key(profile).as_CFType(),
                ),
                (
                    "AverageBitRate",
                    ffi::kVTCompressionPropertyKey_AverageBitRate,
                    int(i64::from(self.bitrate_bps.max(config::MIN_BITRATE_BPS))),
                ),
                (
                    "ExpectedFrameRate",
                    ffi::kVTCompressionPropertyKey_ExpectedFrameRate,
                    int(i64::from(plan.encode_fps)),
                ),
                (
                    "MaxKeyFrameInterval",
                    ffi::kVTCompressionPropertyKey_MaxKeyFrameInterval,
                    int(i64::from(config::max_keyframe_interval_frames(
                        plan.encode_fps,
                    ))),
                ),
                (
                    "MaxKeyFrameIntervalDuration",
                    ffi::kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                    int(i64::from(config::KEYFRAME_INTERVAL_SECONDS)),
                ),
                // BT.709 video range, matching the 420v input and what the
                // iPad's NV12 shader assumes by default.
                (
                    "ColorPrimaries",
                    ffi::kVTCompressionPropertyKey_ColorPrimaries,
                    cf_key(ffi::kCVImageBufferColorPrimaries_ITU_R_709_2).as_CFType(),
                ),
                (
                    "TransferFunction",
                    ffi::kVTCompressionPropertyKey_TransferFunction,
                    cf_key(ffi::kCVImageBufferTransferFunction_ITU_R_709_2).as_CFType(),
                ),
                (
                    "YCbCrMatrix",
                    ffi::kVTCompressionPropertyKey_YCbCrMatrix,
                    cf_key(ffi::kCVImageBufferYCbCrMatrix_ITU_R_709_2).as_CFType(),
                ),
            ]
        };
        if self.data_rate_limit {
            required.push((
                "DataRateLimits",
                unsafe { ffi::kVTCompressionPropertyKey_DataRateLimits },
                rate_limits(self.bitrate_bps).as_CFType(),
            ));
        }
        for (name, key, value) in &required {
            let status = self.set(*key, value);
            if status != 0 {
                warn!(
                    property = name,
                    status, "VideoToolbox rejected a required property"
                );
                return Err(VtError::from_status("VTSessionSetProperty", status));
            }
        }
        // Speed hints: worth having, not worth failing over.
        let mut optional: Vec<(&str, CFStringRef, CFType)> = unsafe {
            vec![
                (
                    "PrioritizeEncodingSpeedOverQuality",
                    ffi::kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                    flag(tuning.prioritize_speed),
                ),
                (
                    "MaximizePowerEfficiency",
                    ffi::kVTCompressionPropertyKey_MaximizePowerEfficiency,
                    no.clone(),
                ),
            ]
        };
        if let Some(delay) = tuning.max_frame_delay {
            optional.push((
                "MaxFrameDelayCount",
                unsafe { ffi::kVTCompressionPropertyKey_MaxFrameDelayCount },
                int(i64::from(delay)),
            ));
        }
        if plan.codec == Codec::H264 {
            optional.push((
                "H264EntropyMode",
                unsafe { ffi::kVTCompressionPropertyKey_H264EntropyMode },
                cf_key(unsafe { ffi::kVTH264EntropyMode_CABAC }).as_CFType(),
            ));
        }
        for (name, key, value) in &optional {
            let status = self.set(*key, value);
            if status != 0 {
                warn!(
                    property = name,
                    status, "VideoToolbox ignored an optional property"
                );
            }
        }
        // SAFETY: self.raw is a live session.
        let status = unsafe { ffi::VTCompressionSessionPrepareToEncodeFrames(self.raw) };
        if status != 0 {
            return Err(VtError::from_status(
                "VTCompressionSessionPrepareToEncodeFrames",
                status,
            ));
        }
        Ok(())
    }

    fn set(&self, key: CFStringRef, value: &CFType) -> i32 {
        // SAFETY: live session; key and value are valid CF objects.
        unsafe { ffi::VTSessionSetProperty(self.raw, key, value.as_CFTypeRef()) }
    }

    fn copy_property(&self, key: CFStringRef) -> Option<CFType> {
        let mut value: CFTypeRef = std::ptr::null();
        // SAFETY: live session; a non-null result is +1 (copy rule).
        let status =
            unsafe { ffi::VTSessionCopyProperty(self.raw, key, std::ptr::null(), &mut value) };
        (status == 0 && !value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
    }

    fn copy_bool(&self, key: CFStringRef) -> bool {
        self.copy_property(key)
            .and_then(|value| value.downcast::<CFBoolean>())
            .is_some_and(bool::from)
    }

    /// The properties as VideoToolbox reports them, for the log.
    pub fn describe(&self) -> String {
        let keys: [(&str, CFStringRef); 6] = unsafe {
            [
                ("ProfileLevel", ffi::kVTCompressionPropertyKey_ProfileLevel),
                (
                    "ExpectedFrameRate",
                    ffi::kVTCompressionPropertyKey_ExpectedFrameRate,
                ),
                (
                    "AverageBitRate",
                    ffi::kVTCompressionPropertyKey_AverageBitRate,
                ),
                ("RealTime", ffi::kVTCompressionPropertyKey_RealTime),
                (
                    "PrioritizeEncodingSpeedOverQuality",
                    ffi::kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                ),
                (
                    "MaxFrameDelayCount",
                    ffi::kVTCompressionPropertyKey_MaxFrameDelayCount,
                ),
            ]
        };
        keys.iter()
            .map(|(name, key)| match self.copy_property(*key) {
                Some(value) => format!("{name}={value:?}"),
                None => format!("{name}=<unset>"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn plan(&self) -> &SessionPlan {
        &self.plan
    }

    pub fn live_fps(&self) -> u32 {
        self.live_fps
    }

    pub fn bitrate(&self) -> u32 {
        self.bitrate_bps
    }

    /// Changes `AverageBitRate` (and `DataRateLimits` when enabled) on the
    /// open session: no reopen, no IDR, no gap. Clamped to what the level
    /// carries: VideoToolbox accepts more live but keeps the level it
    /// prepared with, so the stream would no longer conform to it.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<(), VtError> {
        let requested = bitrate_bps;
        let bitrate_bps = self.plan.clamp_bitrate(bitrate_bps);
        if bitrate_bps < requested {
            warn!(
                requested,
                clamped = bitrate_bps,
                level = self.plan.level.name,
                "Bitrate clamped to what the stream's level carries"
            );
        }
        let status = self.set(
            unsafe { ffi::kVTCompressionPropertyKey_AverageBitRate },
            &CFNumber::from(i64::from(bitrate_bps)).as_CFType(),
        );
        if status != 0 {
            return Err(VtError::from_status("AverageBitRate", status));
        }
        if self.data_rate_limit {
            let status = self.set(
                unsafe { ffi::kVTCompressionPropertyKey_DataRateLimits },
                &rate_limits(bitrate_bps).as_CFType(),
            );
            if status != 0 {
                return Err(VtError::from_status("DataRateLimits", status));
            }
        }
        info!(
            from = self.bitrate_bps,
            to = bitrate_bps,
            "Changed VideoToolbox bitrate live"
        );
        self.bitrate_bps = bitrate_bps;
        Ok(())
    }

    /// Changes `ExpectedFrameRate` (and the keyframe interval in frames)
    /// within what the session's level carries; see `config::plan_change`.
    pub fn set_frame_rate(&mut self, fps: u32) -> Result<(), VtError> {
        let fps = fps.max(1);
        for (name, key, value) in unsafe {
            [
                (
                    "ExpectedFrameRate",
                    ffi::kVTCompressionPropertyKey_ExpectedFrameRate,
                    i64::from(fps),
                ),
                (
                    "MaxKeyFrameInterval",
                    ffi::kVTCompressionPropertyKey_MaxKeyFrameInterval,
                    i64::from(config::max_keyframe_interval_frames(fps)),
                ),
            ]
        } {
            let status = self.set(key, &CFNumber::from(value).as_CFType());
            if status != 0 {
                return Err(VtError::from_status(name, status));
            }
        }
        info!(
            from = self.live_fps,
            to = fps,
            "Changed VideoToolbox frame rate live"
        );
        self.live_fps = fps;
        Ok(())
    }

    fn state(&self) -> &CallbackState {
        &self.state
    }

    /// Frames submitted whose callback has not run yet.
    pub fn in_flight(&self) -> usize {
        self.state().pending.lock().len()
    }

    /// The session was invalidated; it must be replaced.
    pub fn is_lost(&self) -> bool {
        self.state().lost.load(Ordering::Relaxed)
    }

    /// True once after a frame failed or a requested keyframe was dropped.
    pub fn take_keyframe_retry(&self) -> bool {
        self.state().rekey.swap(false, Ordering::Relaxed)
    }

    pub fn counters(&self) -> SessionCounters {
        let state = self.state();
        SessionCounters {
            dropped: state.dropped.load(Ordering::Relaxed),
            failed: state.failed.load(Ordering::Relaxed),
            last_failure: state.last_failure.load(Ordering::Relaxed),
        }
    }

    /// Profile, tier and level from the first keyframe's VPS or SPS.
    pub fn emitted_level(&self) -> Option<ProfileTierLevel> {
        *self.state().emitted.lock()
    }

    /// Converts a packed BGRA frame (the capture format) into a buffer from
    /// the session's IOSurface pool and encodes it.
    pub fn encode_bgra(
        &mut self,
        bgra: &[u8],
        row_bytes: usize,
        info: FrameInfo,
        force_keyframe: bool,
    ) -> Result<(), VtError> {
        let started = Instant::now();
        let buffer = self.pool_buffer()?;
        self.fill_from_bgra(&buffer, bgra, row_bytes)?;
        self.submit(&buffer, info, force_keyframe, started)
    }

    /// Encodes a pixel buffer as is: the zero-copy path for capture that
    /// already produces IOSurface-backed `420v` buffers at the session size.
    pub fn encode_pixel_buffer(
        &mut self,
        buffer: &PixelBuffer,
        info: FrameInfo,
        force_keyframe: bool,
    ) -> Result<(), VtError> {
        self.submit(buffer, info, force_keyframe, Instant::now())
    }

    fn pool_buffer(&self) -> Result<PixelBuffer, VtError> {
        // SAFETY: live session. The pool follows the get rule: the session
        // owns it, so it is fetched per frame rather than cached.
        let pool = unsafe { ffi::VTCompressionSessionGetPixelBufferPool(self.raw) };
        if pool.is_null() {
            // An invalidated session (sleep, media server reset) has no pool
            // any more; this is often the first sign of it.
            self.state().lost.store(true, Ordering::Relaxed);
            return Err(VtError::SessionLost(ffi::VT_INVALID_SESSION));
        }
        let mut raw: ffi::CVPixelBufferRef = std::ptr::null_mut();
        // SAFETY: valid pool; a non-null result is +1.
        let status =
            unsafe { ffi::CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut raw) };
        if status != 0 || raw.is_null() {
            return Err(VtError::Failed {
                call: "CVPixelBufferPoolCreatePixelBuffer",
                status,
            });
        }
        // SAFETY: non-null, and we own the +1 reference.
        Ok(unsafe { PixelBuffer::from_owned(raw) })
    }

    fn fill_from_bgra(
        &self,
        buffer: &PixelBuffer,
        bgra: &[u8],
        row_bytes: usize,
    ) -> Result<(), VtError> {
        let (width, height) = (self.plan.width as usize, self.plan.height as usize);
        // SAFETY: the buffer is a valid two-plane 420v buffer from our pool.
        // Its planes are written only between lock and unlock, and the
        // converter checks every size against what the buffer reports.
        unsafe {
            let status =
                ffi::CVPixelBufferLockBaseAddress(buffer.0, ffi::PIXEL_BUFFER_LOCK_READ_WRITE);
            if status != 0 {
                return Err(VtError::Failed {
                    call: "CVPixelBufferLockBaseAddress",
                    status,
                });
            }
            let plane = |index| Plane {
                data: ffi::CVPixelBufferGetBaseAddressOfPlane(buffer.0, index),
                row_bytes: ffi::CVPixelBufferGetBytesPerRowOfPlane(buffer.0, index),
                rows: ffi::CVPixelBufferGetHeightOfPlane(buffer.0, index),
            };
            let (luma, chroma) = (plane(0), plane(1));
            let result = if luma.data.is_null() || chroma.data.is_null() {
                Err("pool buffer has no NV12 planes".to_string())
            } else {
                self.converter
                    .convert(bgra, width, height, row_bytes, &luma, &chroma)
            };
            ffi::CVPixelBufferUnlockBaseAddress(buffer.0, ffi::PIXEL_BUFFER_LOCK_READ_WRITE);
            result.map_err(VtError::Input)
        }
    }

    fn submit(
        &mut self,
        buffer: &PixelBuffer,
        info: FrameInfo,
        force_keyframe: bool,
        started: Instant,
    ) -> Result<(), VtError> {
        if self.is_lost() {
            return Err(VtError::SessionLost(ffi::VT_INVALID_SESSION));
        }
        if (buffer.width(), buffer.height())
            != (self.plan.width as usize, self.plan.height as usize)
        {
            return Err(VtError::Input(format!(
                "{}x{} frame for a {}x{} session",
                buffer.width(),
                buffer.height(),
                self.plan.width,
                self.plan.height
            )));
        }
        // Microseconds from the session's first frame, strictly increasing
        // (keepalive resends repeat a capture time).
        let epoch = *self.epoch.get_or_insert(info.captured);
        let pts = (info.captured.saturating_duration_since(epoch).as_micros() as i64)
            .max(self.last_pts_us + 1);
        self.last_pts_us = pts;
        let token = self.next_token;
        self.next_token += 1;
        // Registered before the call: the callback may run inside it.
        self.state().pending.lock().insert(
            token,
            Pending {
                info,
                started,
                keyframe_requested: force_keyframe,
            },
        );
        let options = if force_keyframe {
            self.force_keyframe.as_concrete_TypeRef().cast()
        } else {
            std::ptr::null()
        };
        let mut flags = 0u32;
        // SAFETY: live session and buffer; VideoToolbox retains the buffer
        // as long as it needs it. `token` is an integer, not a pointer.
        let status = unsafe {
            ffi::VTCompressionSessionEncodeFrame(
                self.raw,
                buffer.0,
                ffi::CMTime::micros(pts),
                ffi::CMTime::INVALID,
                options,
                token as *mut c_void,
                &mut flags,
            )
        };
        if status != 0 {
            self.state().pending.lock().remove(&token);
            let error = VtError::from_status("VTCompressionSessionEncodeFrame", status);
            if matches!(error, VtError::SessionLost(_)) {
                self.state().lost.store(true, Ordering::Relaxed);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Blocks until every submitted frame has come back.
    pub fn flush(&self) -> Result<(), VtError> {
        // SAFETY: live session.
        let status =
            unsafe { ffi::VTCompressionSessionCompleteFrames(self.raw, ffi::CMTime::INVALID) };
        if status == 0 {
            Ok(())
        } else {
            Err(VtError::from_status(
                "VTCompressionSessionCompleteFrames",
                status,
            ))
        }
    }

    /// Invalidates the session underneath us, as a sleep or media server
    /// reset would, to exercise recovery.
    #[cfg(test)]
    pub fn invalidate_for_test(&self) {
        // SAFETY: live session; Drop invalidates again, which is allowed.
        unsafe { ffi::VTCompressionSessionInvalidate(self.raw) };
    }

    #[cfg(test)]
    pub fn callback_probe(&self) -> CallbackProbe {
        CallbackProbe {
            id: self.id,
            state: Arc::downgrade(&self.state),
        }
    }
}

/// Watches a session's callback state after the session is gone.
#[cfg(test)]
pub struct CallbackProbe {
    id: u64,
    state: std::sync::Weak<CallbackState>,
}

#[cfg(test)]
impl CallbackProbe {
    pub fn is_registered(&self) -> bool {
        CALLBACKS.lock().contains_key(&self.id)
    }

    pub fn is_freed(&self) -> bool {
        self.state.strong_count() == 0
    }

    /// Runs the output callback as a frame from this session would.
    pub fn late_callback(&self, status: i32) {
        on_encoded(
            self.id as *mut c_void,
            std::ptr::null_mut(),
            status,
            0,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(test)]
pub fn registered_callbacks() -> usize {
    CALLBACKS.lock().len()
}

/// `DataRateLimits` as a CFArray of [bytes, seconds].
fn rate_limits(bitrate_bps: u32) -> CFArray<CFType> {
    let (bytes, seconds) = config::data_rate_limit(bitrate_bps);
    CFArray::from_CFTypes(&[
        CFNumber::from(bytes).as_CFType(),
        CFNumber::from(seconds).as_CFType(),
    ])
}

fn h264_profile_level(plan: SessionPlan) -> Result<CFStringRef, VtError> {
    let name = config::h264_profile_level_name(plan.level).ok_or_else(|| {
        VtError::Input(format!(
            "H.264 level {} has no VideoToolbox profile constant",
            plan.level.name
        ))
    })?;
    // SAFETY: framework constants.
    Ok(unsafe {
        match name {
            "H264_High_3_0" => ffi::kVTProfileLevel_H264_High_3_0,
            "H264_High_3_1" => ffi::kVTProfileLevel_H264_High_3_1,
            "H264_High_3_2" => ffi::kVTProfileLevel_H264_High_3_2,
            "H264_High_4_0" => ffi::kVTProfileLevel_H264_High_4_0,
            "H264_High_4_1" => ffi::kVTProfileLevel_H264_High_4_1,
            "H264_High_4_2" => ffi::kVTProfileLevel_H264_High_4_2,
            "H264_High_5_0" => ffi::kVTProfileLevel_H264_High_5_0,
            "H264_High_5_1" => ffi::kVTProfileLevel_H264_High_5_1,
            _ => ffi::kVTProfileLevel_H264_High_5_2,
        }
    })
}

impl Drop for VtSession {
    fn drop(&mut self) {
        // SAFETY: we own the session's only reference. CompleteFrames runs
        // every pending callback; Invalidate stops further ones.
        unsafe {
            if !self.is_lost() {
                ffi::VTCompressionSessionCompleteFrames(self.raw, ffi::CMTime::INVALID);
            }
            ffi::VTCompressionSessionInvalidate(self.raw);
            ffi::CFRelease(self.raw as CFTypeRef);
        }
        // A lost session's pending frames never complete, and no API
        // promises their callbacks won't still arrive. Once the entry is
        // gone such a callback finds nothing; one already running holds
        // its own clone of the state.
        CALLBACKS.lock().remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register(state: &Arc<CallbackState>) -> u64 {
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        CALLBACKS.lock().insert(id, state.clone());
        id
    }

    fn callback_state() -> Arc<CallbackState> {
        Arc::new(CallbackState {
            codec: Codec::Hevc,
            output: OutputQueue::new(),
            pending: Mutex::new(HashMap::new()),
            lost: AtomicBool::new(false),
            rekey: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            last_failure: AtomicI32::new(0),
            emitted: Mutex::new(None),
        })
    }

    fn pending() -> Pending {
        Pending {
            info: FrameInfo {
                captured: Instant::now(),
                frame_number: 1,
            },
            started: Instant::now(),
            keyframe_requested: false,
        }
    }

    fn callback(id: u64, token: u64, status: i32) {
        on_encoded(
            id as *mut c_void,
            token as *mut c_void,
            status,
            0,
            std::ptr::null_mut(),
        );
    }

    #[test]
    fn callbacks_reach_a_registered_session_and_late_ones_are_no_ops() {
        let state = callback_state();
        let id = register(&state);
        state.pending.lock().insert(1, pending());
        callback(id, 1, ffi::VT_INVALID_SESSION);
        assert!(state.lost.load(Ordering::Relaxed));
        assert_eq!(state.failed.load(Ordering::Relaxed), 1);

        // Teardown of a lost session: frames still pending never completed.
        state.pending.lock().insert(2, pending());
        let freed = Arc::downgrade(&state);
        drop(state);
        CALLBACKS.lock().remove(&id);
        assert_eq!(freed.strong_count(), 0, "freed with its last clone");
        // The late callback for frame 2 finds no session and returns.
        callback(id, 2, 0);
        callback(id, 2, ffi::VT_INVALID_SESSION);
    }

    #[test]
    fn lost_sessions_leave_nothing_behind() {
        let mut freed = Vec::new();
        for _ in 0..10_000 {
            let state = callback_state();
            let id = register(&state);
            for token in 0..4 {
                state.pending.lock().insert(token, pending());
            }
            callback(id, 0, ffi::VT_INVALID_SESSION);
            freed.push((id, Arc::downgrade(&state)));
            drop(state);
            CALLBACKS.lock().remove(&id);
            callback(id, 1, 0);
        }
        assert!(freed.iter().all(|(_, state)| state.strong_count() == 0));
        let registry = CALLBACKS.lock();
        assert!(freed.iter().all(|(id, _)| !registry.contains_key(id)));
    }
}
