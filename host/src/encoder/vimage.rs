//! Accelerate's BGRA conversion on macOS. The Windows encoder path is unchanged.
//! ABI declarations follow the macOS SDK's vImage_Types.h and Conversion.h.
//! [`Converter`] feeds FFmpeg (BT.601 planar); [`Nv12Converter`] feeds the
//! direct VideoToolbox encoder (BT.709 biplanar).
use ffmpeg_next::{format::Pixel, frame::Video};
use std::ffi::c_void;

#[repr(C)]
struct Buffer {
    data: *mut c_void,
    height: usize,
    width: usize,
    row_bytes: usize,
}

#[repr(C, align(16))]
pub(super) struct Converter {
    opaque: [u8; 128],
}

#[repr(C)]
struct Matrix {
    coefficients: [f32; 8],
}
#[repr(C)]
struct PixelRange {
    values: [i32; 8],
}

#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    static kvImage_ARGBToYpCbCrMatrix_ITU_R_601_4: *const Matrix;
    static kvImage_ARGBToYpCbCrMatrix_ITU_R_709_2: *const Matrix;
    fn vImageConvert_ARGBToYpCbCr_GenerateConversion(
        matrix: *const Matrix,
        range: *const PixelRange,
        info: *mut Converter,
        input: u32,
        output: u32,
        flags: u32,
    ) -> isize;
    fn vImageConvert_ARGB8888To420Yp8_Cb8_Cr8(
        source: *const Buffer,
        y: *const Buffer,
        cb: *const Buffer,
        cr: *const Buffer,
        info: *const Converter,
        permute: *const u8,
        flags: u32,
    ) -> isize;
    fn vImageConvert_ARGB8888To420Yp8_CbCr8(
        source: *const Buffer,
        y: *const Buffer,
        cbcr: *const Buffer,
        info: *const Converter,
        permute: *const u8,
        flags: u32,
    ) -> isize;
}

/// Video-range limits: Y 16..235, Cb/Cr 16..240 around 128.
const VIDEO_RANGE: PixelRange = PixelRange {
    values: [16, 128, 235, 240, 235, 16, 240, 16],
};
/// `kvImage420Yp8_CbCr8`: CoreVideo's `420v`/`420f`.
const YPCBCR_420_BIPLANAR: u32 = 4;

impl Converter {
    pub(super) fn new() -> Result<Self, String> {
        let mut info = Self { opaque: [0; 128] };
        // BT.601 limited range, matching swscale's existing BGRA -> YUV420P path.
        let error = unsafe {
            vImageConvert_ARGBToYpCbCr_GenerateConversion(
                kvImage_ARGBToYpCbCrMatrix_ITU_R_601_4,
                &VIDEO_RANGE,
                &mut info,
                0,
                3,
                0,
            )
        };
        if error == 0 {
            Ok(info)
        } else {
            Err(format!("vImage conversion setup failed: {error}"))
        }
    }

    pub(super) fn convert(
        &self,
        source: &[u8],
        width: u32,
        height: u32,
        row_bytes: usize,
        output: &mut Video,
    ) -> Result<(), String> {
        let (width, height) = (width as usize, height as usize);
        if output.format() != Pixel::YUV420P
            || width != output.width() as usize
            || height != output.height() as usize
            || width < 2
            || height < 2
            || width % 2 != 0
            || height % 2 != 0
            || row_bytes < width * 4
            || row_bytes
                .checked_mul(height)
                .is_none_or(|size| source.len() < size)
        {
            return Err("vImage requires equal, even BGRA/YUV420P dimensions".into());
        }
        let src = Buffer {
            data: source.as_ptr() as *mut c_void,
            width,
            height,
            row_bytes,
        };
        let planes = std::array::from_fn::<_, 3, _>(|plane| {
            let row_bytes = output.stride(plane);
            let shift = usize::from(plane != 0);
            Buffer {
                data: output.data_mut(plane).as_mut_ptr().cast(),
                width: width >> shift,
                height: height >> shift,
                row_bytes,
            }
        });
        // DoNotTile avoids a worker pool competing with the decoder on small VMs.
        // Buffers remain borrowed through this synchronous call; planes do not overlap.
        let error = unsafe {
            vImageConvert_ARGB8888To420Yp8_Cb8_Cr8(
                &src,
                &planes[0],
                &planes[1],
                &planes[2],
                self,
                [3, 2, 1, 0].as_ptr(),
                16,
            )
        };
        if error == 0 {
            Ok(())
        } else {
            Err(format!("vImage BGRA conversion failed: {error}"))
        }
    }
}

/// BGRA to BT.709 video-range NV12 (`420v`). The direct VideoToolbox encoder
/// tags its stream BT.709, and the iPad decodes to video-range NV12 and
/// defaults its shader to BT.709 (`MetalView.swift`).
pub(super) struct Nv12Converter(Converter);

/// One writable plane of a locked pixel buffer.
pub(super) struct Plane {
    pub data: *mut u8,
    pub row_bytes: usize,
    pub rows: usize,
}

impl Nv12Converter {
    pub(super) fn new() -> Result<Self, String> {
        let mut info = Converter { opaque: [0; 128] };
        // SAFETY: the matrix is a static the framework exports; `info` is the
        // 128-byte, 16-aligned vImage_ARGBToYpCbCr the call fills in.
        let error = unsafe {
            vImageConvert_ARGBToYpCbCr_GenerateConversion(
                kvImage_ARGBToYpCbCrMatrix_ITU_R_709_2,
                &VIDEO_RANGE,
                &mut info,
                0,
                YPCBCR_420_BIPLANAR,
                0,
            )
        };
        if error == 0 {
            Ok(Self(info))
        } else {
            Err(format!("vImage NV12 conversion setup failed: {error}"))
        }
    }

    /// Converts a packed BGRA image into an NV12 luma and CbCr plane pair.
    ///
    /// # Safety
    /// Each plane's `data` must be writable for `rows * row_bytes` bytes for
    /// the duration of the call (a locked CVPixelBuffer plane), and neither
    /// may overlap `source`.
    pub(super) unsafe fn convert(
        &self,
        source: &[u8],
        width: usize,
        height: usize,
        row_bytes: usize,
        luma: &Plane,
        chroma: &Plane,
    ) -> Result<(), String> {
        if width < 2
            || height < 2
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
            || row_bytes < width * 4
            || row_bytes
                .checked_mul(height)
                .is_none_or(|size| source.len() < size)
            || luma.rows < height
            || luma.row_bytes < width
            || chroma.rows < height / 2
            || chroma.row_bytes < width
        {
            return Err(format!(
                "vImage NV12 conversion needs even dimensions and planes that fit {width}x{height}"
            ));
        }
        let src = Buffer {
            data: source.as_ptr() as *mut c_void,
            width,
            height,
            row_bytes,
        };
        let y = Buffer {
            data: luma.data.cast(),
            width,
            height,
            row_bytes: luma.row_bytes,
        };
        let cbcr = Buffer {
            data: chroma.data.cast(),
            width: width / 2,
            height: height / 2,
            row_bytes: chroma.row_bytes,
        };
        // Flags 0 lets vImage split the frame across cores: at 2670x1878 a
        // single core costs several milliseconds a frame.
        let error = vImageConvert_ARGB8888To420Yp8_CbCr8(
            &src,
            &y,
            &cbcr,
            &self.0,
            [3, 2, 1, 0].as_ptr(),
            0,
        );
        if error == 0 {
            Ok(())
        } else {
            Err(format!("vImage NV12 conversion failed: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_conversion_matches_ffmpeg_with_padded_rows() {
        let converter = Converter::new().unwrap();
        for (width, height) in [(32, 32), (642, 362), (2560, 1440)] {
            let mut src = Video::new(Pixel::BGRA, width, height);
            let stride = src.stride(0);
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let offset = y * stride + x * 4;
                    src.data_mut(0)[offset..offset + 4].copy_from_slice(&[
                        x as u8,
                        y as u8,
                        (x + y) as u8,
                        255,
                    ]);
                }
            }
            let mut reference = Video::new(Pixel::YUV420P, width, height);
            let mut output = Video::new(Pixel::YUV420P, width, height);
            let mut scaler = ffmpeg_next::software::scaling::Context::get(
                Pixel::BGRA,
                width,
                height,
                Pixel::YUV420P,
                width,
                height,
                ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
            )
            .unwrap();
            scaler.run(&src, &mut reference).unwrap();
            converter
                .convert(src.data(0), width, height, stride, &mut output)
                .unwrap();
            for plane in 0..3 {
                let shift = u32::from(plane != 0);
                for y in 0..(height >> shift) as usize {
                    for x in 0..(width >> shift) as usize {
                        let actual = output.data(plane)[y * output.stride(plane) + x];
                        let expected = reference.data(plane)[y * reference.stride(plane) + x];
                        assert!(
                            actual.abs_diff(expected) <= 1,
                            "{width}x{height} plane {plane} at {x},{y}: {actual} vs {expected}"
                        );
                    }
                }
            }
        }
        let src = Video::new(Pixel::BGRA, 3, 3);
        let mut output = Video::new(Pixel::YUV420P, 3, 3);
        assert!(converter
            .convert(src.data(0), 3, 3, src.stride(0), &mut output)
            .is_err());
    }

    #[test]
    fn packed_capture_pixels_match_padded_input_and_remain_unchanged() {
        let converter = Converter::new().unwrap();
        let (width, height) = (642, 362);
        let mut source = vec![0; width as usize * height as usize * 4];
        crate::capture::synthetic::render_synthetic_frame(&mut source, width, height, 123);
        let before = source.clone();
        let mut packed = Video::new(Pixel::YUV420P, width, height);
        let mut reference = Video::new(Pixel::YUV420P, width, height);
        let mut padded = Video::new(Pixel::BGRA, width, height);
        let row_bytes = width as usize * 4;
        let stride = padded.stride(0);
        for y in 0..height as usize {
            padded.data_mut(0)[y * stride..y * stride + row_bytes]
                .copy_from_slice(&source[y * row_bytes..(y + 1) * row_bytes]);
        }
        converter
            .convert(&source, width, height, row_bytes, &mut packed)
            .unwrap();
        converter
            .convert(padded.data(0), width, height, stride, &mut reference)
            .unwrap();
        assert_eq!(source, before);
        for plane in 0..3 {
            let shift = usize::from(plane != 0);
            for y in 0..height as usize >> shift {
                let count = width as usize >> shift;
                let got = &packed.data(plane)[y * packed.stride(plane)..][..count];
                let want = &reference.data(plane)[y * reference.stride(plane)..][..count];
                assert_eq!(got, want);
            }
        }
        assert!(converter
            .convert(
                &source[..source.len() - 1],
                width,
                height,
                row_bytes,
                &mut packed
            )
            .is_err());
        assert!(converter
            .convert(&source, width, height, row_bytes - 1, &mut packed)
            .is_err());
        assert!(converter
            .convert(&source, width, height, usize::MAX, &mut packed)
            .is_err());
    }
}
