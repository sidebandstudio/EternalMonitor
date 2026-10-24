//! Hand-written VideoToolbox, CoreMedia and CoreVideo declarations: only
//! what the encoder (and its tests' decoder) call. Signatures follow the
//! macOS 15 SDK headers (VTCompressionSession.h, VTCompressionProperties.h,
//! VTErrors.h, CMSampleBuffer.h, CMFormatDescription.h, CVPixelBuffer.h).
//! Every function here is `unsafe` to call; the safe wrappers live in
//! `session.rs`, which owns the references they return.

use core_foundation::base::CFTypeRef;
use core_foundation::string::CFStringRef;
use std::ffi::c_void;

pub type OSStatus = i32;
pub type VTCompressionSessionRef = *mut c_void;
pub type CMSampleBufferRef = *mut c_void;
pub type CMBlockBufferRef = *mut c_void;
pub type CMFormatDescriptionRef = *const c_void;
pub type CVPixelBufferRef = *mut c_void;
pub type CVPixelBufferPoolRef = *mut c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFArrayRef = *const c_void;

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

    /// A valid time in microseconds (`kCMTimeFlags_Valid`).
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
/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`.
pub const PIXEL_FORMAT_NV12_VIDEO_RANGE: u32 = u32::from_be_bytes(*b"420v");

/// `kVTEncodeInfo_FrameDropped`.
pub const ENCODE_INFO_FRAME_DROPPED: u32 = 1 << 1;
/// Lock flags without `kCVPixelBufferLock_ReadOnly`: the encoder writes the planes.
pub const PIXEL_BUFFER_LOCK_READ_WRITE: u64 = 0;

pub const VT_INVALID_SESSION: OSStatus = -12903;
pub const VT_COULD_NOT_CREATE_INSTANCE: OSStatus = -12907;
pub const VT_COULD_NOT_FIND_VIDEO_ENCODER: OSStatus = -12908;
pub const VT_VIDEO_ENCODER_MALFUNCTION: OSStatus = -12912;
pub const VT_VIDEO_ENCODER_NOT_AVAILABLE_NOW: OSStatus = -12915;

pub type VTCompressionOutputCallback = extern "C" fn(
    output_callback_refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample_buffer: CMSampleBufferRef,
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
        output_callback: Option<VTCompressionOutputCallback>,
        output_callback_refcon: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;
    pub fn VTSessionSetProperty(
        session: *mut c_void,
        property_key: CFStringRef,
        property_value: CFTypeRef,
    ) -> OSStatus;
    pub fn VTSessionCopyProperty(
        session: *mut c_void,
        property_key: CFStringRef,
        allocator: *const c_void,
        property_value_out: *mut CFTypeRef,
    ) -> OSStatus;
    pub fn VTCompressionSessionPrepareToEncodeFrames(session: VTCompressionSessionRef) -> OSStatus;
    pub fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        presentation_time_stamp: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    pub fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_presentation_time_stamp: CMTime,
    ) -> OSStatus;
    pub fn VTCompressionSessionGetPixelBufferPool(
        session: VTCompressionSessionRef,
    ) -> CVPixelBufferPoolRef;
    pub fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);

    pub static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    pub static kVTCompressionPropertyKey_DataRateLimits: CFStringRef;
    pub static kVTCompressionPropertyKey_RealTime: CFStringRef;
    pub static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    pub static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: CFStringRef;
    pub static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    pub static kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality: CFStringRef;
    pub static kVTCompressionPropertyKey_MaximizePowerEfficiency: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxFrameDelayCount: CFStringRef;
    pub static kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder: CFStringRef;
    pub static kVTCompressionPropertyKey_H264EntropyMode: CFStringRef;
    pub static kVTCompressionPropertyKey_ColorPrimaries: CFStringRef;
    pub static kVTCompressionPropertyKey_TransferFunction: CFStringRef;
    pub static kVTCompressionPropertyKey_YCbCrMatrix: CFStringRef;
    pub static kVTH264EntropyMode_CABAC: CFStringRef;
    pub static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFStringRef;
    pub static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;

    pub static kVTProfileLevel_HEVC_Main_AutoLevel: CFStringRef;
    pub static kVTProfileLevel_H264_High_3_0: CFStringRef;
    pub static kVTProfileLevel_H264_High_3_1: CFStringRef;
    pub static kVTProfileLevel_H264_High_3_2: CFStringRef;
    pub static kVTProfileLevel_H264_High_4_0: CFStringRef;
    pub static kVTProfileLevel_H264_High_4_1: CFStringRef;
    pub static kVTProfileLevel_H264_High_4_2: CFStringRef;
    pub static kVTProfileLevel_H264_High_5_0: CFStringRef;
    pub static kVTProfileLevel_H264_High_5_1: CFStringRef;
    pub static kVTProfileLevel_H264_High_5_2: CFStringRef;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    pub fn CMSampleBufferGetDataBuffer(sample: CMSampleBufferRef) -> CMBlockBufferRef;
    pub fn CMBlockBufferGetDataLength(block: CMBlockBufferRef) -> usize;
    pub fn CMBlockBufferCopyDataBytes(
        block: CMBlockBufferRef,
        offset_to_data: usize,
        data_length: usize,
        destination: *mut c_void,
    ) -> OSStatus;
    pub fn CMSampleBufferGetSampleAttachmentsArray(
        sample: CMSampleBufferRef,
        create_if_necessary: u8,
    ) -> CFArrayRef;
    pub fn CMSampleBufferGetFormatDescription(sample: CMSampleBufferRef) -> CMFormatDescriptionRef;
    pub fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        description: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;
    pub fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        description: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;
    pub static kCMSampleAttachmentKey_NotSync: CFStringRef;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    pub fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: *const c_void,
        pool: CVPixelBufferPoolRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> i32;
    pub fn CVPixelBufferLockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    pub fn CVPixelBufferUnlockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    pub fn CVPixelBufferGetWidth(buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetHeight(buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetBaseAddressOfPlane(buffer: CVPixelBufferRef, plane: usize) -> *mut u8;
    pub fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CVPixelBufferRef, plane: usize) -> usize;
    pub fn CVPixelBufferGetHeightOfPlane(buffer: CVPixelBufferRef, plane: usize) -> usize;

    pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    pub static kCVPixelBufferWidthKey: CFStringRef;
    pub static kCVPixelBufferHeightKey: CFStringRef;
    pub static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    pub static kCVImageBufferColorPrimaries_ITU_R_709_2: CFStringRef;
    pub static kCVImageBufferTransferFunction_ITU_R_709_2: CFStringRef;
    pub static kCVImageBufferYCbCrMatrix_ITU_R_709_2: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
}
