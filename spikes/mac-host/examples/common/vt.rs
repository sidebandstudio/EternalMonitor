//! Hand-written VideoToolbox, CoreMedia and CoreVideo declarations: just what
//! the spikes call. Phase 2 would use `objc2-video-toolbox` instead.

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::string::CFStringRef;
use std::ffi::c_void;

pub type OSStatus = i32;
pub type VTCompressionSessionRef = *mut c_void;
pub type CMSampleBufferRef = *mut c_void;
pub type CVPixelBufferRef = *mut c_void;
pub type CVPixelBufferPoolRef = *mut c_void;
pub type CFDictionaryRef = *const c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

impl CMTime {
    pub const INVALID: Self = Self {
        value: 0,
        timescale: 0,
        flags: 0,
        epoch: 0,
    };
    pub fn micros(us: i64) -> Self {
        Self {
            value: us,
            timescale: 1_000_000,
            flags: 1,
            epoch: 0,
        }
    }
}

pub const CODEC_HEVC: u32 = u32::from_be_bytes(*b"hvc1");
pub const CODEC_H264: u32 = u32::from_be_bytes(*b"avc1");
pub const PIXEL_NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
pub const PIXEL_BGRA: u32 = u32::from_be_bytes(*b"BGRA");

pub type OutputCallback = extern "C" fn(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample: CMSampleBufferRef,
);

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    pub fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: *const c_void,
        output_callback: Option<OutputCallback>,
        output_callback_refcon: *mut c_void,
        session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;
    pub fn VTSessionSetProperty(
        session: *mut c_void,
        key: CFStringRef,
        value: CFTypeRef,
    ) -> OSStatus;
    pub fn VTSessionCopyProperty(
        session: *mut c_void,
        key: CFStringRef,
        allocator: *const c_void,
        value_out: *mut CFTypeRef,
    ) -> OSStatus;
    pub fn VTCompressionSessionPrepareToEncodeFrames(session: VTCompressionSessionRef) -> OSStatus;
    pub fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        pts: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    pub fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until: CMTime,
    ) -> OSStatus;
    pub fn VTCompressionSessionGetPixelBufferPool(
        session: VTCompressionSessionRef,
    ) -> CVPixelBufferPoolRef;
    pub fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);
    pub fn VTCompressionSessionGetTypeID() -> usize;

    pub static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    pub static kVTCompressionPropertyKey_DataRateLimits: CFStringRef;
    pub static kVTCompressionPropertyKey_RealTime: CFStringRef;
    pub static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    pub static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    pub static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    pub static kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality: CFStringRef;
    pub static kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder: CFStringRef;
    pub static kVTCompressionPropertyKey_ConstantBitRate: CFStringRef;
    pub static kVTCompressionPropertyKey_MaximizePowerEfficiency: CFStringRef;
    pub static kVTCompressionPropertyKey_H264EntropyMode: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxFrameDelayCount: CFStringRef;
    pub static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFStringRef;
    pub static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFStringRef;
    pub static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    pub static kVTProfileLevel_HEVC_Main_AutoLevel: CFStringRef;
    pub static kVTProfileLevel_H264_Baseline_AutoLevel: CFStringRef;
    pub static kVTProfileLevel_H264_High_AutoLevel: CFStringRef;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    pub fn CMSampleBufferGetDataBuffer(sample: CMSampleBufferRef) -> *mut c_void;
    pub fn CMBlockBufferGetDataLength(block: *mut c_void) -> usize;
    pub fn CMBlockBufferCopyDataBytes(
        block: *mut c_void,
        offset: usize,
        length: usize,
        destination: *mut c_void,
    ) -> OSStatus;
    pub fn CMSampleBufferGetSampleAttachmentsArray(
        sample: CMSampleBufferRef,
        create_if_necessary: u8,
    ) -> *const c_void;
    pub fn CMSampleBufferGetFormatDescription(sample: CMSampleBufferRef) -> *const c_void;
    pub fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        description: *const c_void,
        index: usize,
        parameter_set: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_header_length: *mut i32,
    ) -> OSStatus;
    pub fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        description: *const c_void,
        index: usize,
        parameter_set: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_header_length: *mut i32,
    ) -> OSStatus;
    pub static kCMSampleAttachmentKey_NotSync: CFStringRef;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    pub fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: *const c_void,
        pool: CVPixelBufferPoolRef,
        buffer_out: *mut CVPixelBufferRef,
    ) -> i32;
    pub fn CVPixelBufferLockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    pub fn CVPixelBufferUnlockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    pub fn CVPixelBufferGetBaseAddressOfPlane(buffer: CVPixelBufferRef, plane: usize) -> *mut u8;
    pub fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CVPixelBufferRef, plane: usize) -> usize;
    pub fn CVPixelBufferGetHeightOfPlane(buffer: CVPixelBufferRef, plane: usize) -> usize;
    pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    pub static kCVPixelBufferWidthKey: CFStringRef;
    pub static kCVPixelBufferHeightKey: CFStringRef;
    pub static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
}

/// The session properties the spikes report, by name.
pub fn reported_properties() -> Vec<(&'static str, CFStringRef)> {
    unsafe {
        vec![
            (
                "UsingHardwareAcceleratedVideoEncoder",
                kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
            ),
            ("RealTime", kVTCompressionPropertyKey_RealTime),
            (
                "PrioritizeEncodingSpeedOverQuality",
                kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
            ),
            ("ProfileLevel", kVTCompressionPropertyKey_ProfileLevel),
            ("AverageBitRate", kVTCompressionPropertyKey_AverageBitRate),
            ("DataRateLimits", kVTCompressionPropertyKey_DataRateLimits),
            ("ConstantBitRate", kVTCompressionPropertyKey_ConstantBitRate),
            (
                "ExpectedFrameRate",
                kVTCompressionPropertyKey_ExpectedFrameRate,
            ),
            (
                "AllowFrameReordering",
                kVTCompressionPropertyKey_AllowFrameReordering,
            ),
            (
                "MaxKeyFrameInterval",
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
            ),
            (
                "MaximizePowerEfficiency",
                kVTCompressionPropertyKey_MaximizePowerEfficiency,
            ),
            ("H264EntropyMode", kVTCompressionPropertyKey_H264EntropyMode),
            (
                "MaxFrameDelayCount",
                kVTCompressionPropertyKey_MaxFrameDelayCount,
            ),
        ]
    }
}

/// Reads one property back, as CoreFoundation's description of it.
pub fn copy_property(session: *mut c_void, key: CFStringRef) -> String {
    let mut value: CFTypeRef = std::ptr::null();
    let status = unsafe { VTSessionCopyProperty(session, key, std::ptr::null(), &mut value) };
    if status != 0 {
        return format!("<status {status}>");
    }
    if value.is_null() {
        return "<null>".into();
    }
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    format!("{value:?}").replace('\n', " ")
}

/// Every reported property, one `name=value` per line.
pub fn describe_session(session: *mut c_void) -> Vec<String> {
    reported_properties()
        .into_iter()
        .map(|(name, key)| format!("{name}={}", copy_property(session, key)))
        .collect()
}

/// True for a sample with no `NotSync` attachment, which is a sync frame.
pub fn sample_is_keyframe(sample: CMSampleBufferRef) -> bool {
    use core_foundation::array::CFArray;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    let array = unsafe { CMSampleBufferGetSampleAttachmentsArray(sample, 0) };
    if array.is_null() {
        return true;
    }
    let array: CFArray<CFDictionary<CFString, CFType>> =
        unsafe { CFArray::wrap_under_get_rule(array.cast()) };
    let Some(first) = array.get(0) else {
        return true;
    };
    let key = unsafe { CFString::wrap_under_get_rule(kCMSampleAttachmentKey_NotSync) };
    match first.find(&key) {
        Some(value) => value
            .downcast::<CFBoolean>()
            .is_none_or(|not_sync| !bool::from(not_sync)),
        None => true,
    }
}

/// The parameter sets (VPS/SPS/PPS or SPS/PPS) of a sample's format.
pub fn parameter_sets(sample: CMSampleBufferRef, hevc: bool) -> Vec<Vec<u8>> {
    let description = unsafe { CMSampleBufferGetFormatDescription(sample) };
    let mut sets = Vec::new();
    if description.is_null() {
        return sets;
    }
    let mut index = 0;
    loop {
        let mut data: *const u8 = std::ptr::null();
        let (mut size, mut count, mut header) = (0usize, 0usize, 0i32);
        let status = unsafe {
            if hevc {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    description,
                    index,
                    &mut data,
                    &mut size,
                    &mut count,
                    &mut header,
                )
            } else {
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    description,
                    index,
                    &mut data,
                    &mut size,
                    &mut count,
                    &mut header,
                )
            }
        };
        if status != 0 || data.is_null() {
            break;
        }
        sets.push(unsafe { std::slice::from_raw_parts(data, size) }.to_vec());
        index += 1;
        if index >= count {
            break;
        }
    }
    sets
}
