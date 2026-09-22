//! Accelerate's BGRA conversion on macOS. The Windows encoder path is unchanged.
//! ABI declarations follow the macOS SDK's vImage_Types.h and Conversion.h.
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
}

impl Converter {
    pub(super) fn new() -> Result<Self, String> {
        let mut info = Self { opaque: [0; 128] };
        // BT.601 limited range, matching swscale's existing BGRA -> YUV420P path.
        let range = PixelRange {
            values: [16, 128, 235, 240, 235, 16, 240, 16],
        };
        let error = unsafe {
            vImageConvert_ARGBToYpCbCr_GenerateConversion(
                kvImage_ARGBToYpCbCrMatrix_ITU_R_601_4,
                &range,
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

    pub(super) fn convert(&self, source: &Video, output: &mut Video) -> Result<(), String> {
        let (width, height) = (source.width() as usize, source.height() as usize);
        if source.format() != Pixel::BGRA
            || output.format() != Pixel::YUV420P
            || source.width() != output.width()
            || source.height() != output.height()
            || width < 2
            || height < 2
            || width % 2 != 0
            || height % 2 != 0
        {
            return Err("vImage requires equal, even BGRA/YUV420P dimensions".into());
        }
        let src = Buffer {
            data: source.data(0).as_ptr() as *mut c_void,
            width,
            height,
            row_bytes: source.stride(0),
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
            converter.convert(&src, &mut output).unwrap();
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
        assert!(converter.convert(&src, &mut output).is_err());
    }
}
