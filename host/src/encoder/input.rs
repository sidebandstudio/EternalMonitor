use ffmpeg_next::format::Pixel;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderInput {
    Auto,
    Bgra,
    // AMD BGRA is unsupported in the pinned SDK; see docs/decisions.md.
    #[default]
    Yuv420,
}

impl EncoderInput {
    pub fn from_env(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "bgra" => Some(Self::Bgra),
            "yuv420" | "yuv420p" => Some(Self::Yuv420),
            _ => None,
        }
    }
}

pub(super) fn select_input(
    mode: EncoderInput,
    codec_name: &str,
    supported: &[Pixel],
) -> Result<Pixel, &'static str> {
    if codec_name == "libx264" {
        return Ok(Pixel::YUV420P);
    }
    if mode != EncoderInput::Yuv420 {
        for format in [Pixel::BGRA, Pixel::BGRZ] {
            if supported.contains(&format) {
                return Ok(format);
            }
        }
    }
    if mode == EncoderInput::Bgra {
        return Err("The selected encoder does not accept BGRA/BGR0 input");
    }
    // YUV420P and NV12 are both 8-bit 4:2:0. Quick Sync accepts NV12,
    // but not planar YUV420P, in the pinned FFmpeg 7.1 SDK.
    for format in [Pixel::YUV420P, Pixel::NV12] {
        if supported.contains(&format) {
            return Ok(format);
        }
    }
    if supported.is_empty() {
        return Ok(Pixel::YUV420P);
    }
    Err("The selected encoder does not accept 8-bit YUV420P/NV12 input")
}

/// The swscale context that turns a captured BGRA frame of `source` size into
/// the encoder's `format` at `output` size, or `None` when the capture can be
/// copied in as it is.
pub(super) fn conversion_context(
    format: Pixel,
    source: (u32, u32),
    output: (u32, u32),
) -> Result<Option<ffmpeg_next::software::scaling::Context>, ffmpeg_next::Error> {
    if !matches!(format, Pixel::YUV420P | Pixel::NV12) && source == output {
        return Ok(None);
    }
    // Bilinear rather than the fast variant once scaling: text on a scaled
    // desktop stays readable.
    let flags = if source == output {
        ffmpeg_next::software::scaling::Flags::FAST_BILINEAR
    } else {
        ffmpeg_next::software::scaling::Flags::BILINEAR
    };
    ffmpeg_next::software::scaling::Context::get(
        Pixel::BGRA,
        source.0,
        source.1,
        format,
        output.0,
        output.1,
        flags,
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertised_formats_control_native_input_and_software_stays_yuv() {
        let formats = [Pixel::YUV420P, Pixel::BGRZ, Pixel::BGRA];
        assert_eq!(
            select_input(EncoderInput::Auto, "h264_nvenc", &formats),
            Ok(Pixel::BGRA)
        );
        assert_eq!(
            select_input(EncoderInput::Auto, "h264_amf", &[Pixel::BGRZ]),
            Ok(Pixel::BGRZ)
        );
        assert_eq!(
            select_input(EncoderInput::Auto, "h264_qsv", &[Pixel::NV12]),
            Ok(Pixel::NV12)
        );
        assert_eq!(
            select_input(EncoderInput::Auto, "unknown", &[]),
            Ok(Pixel::YUV420P)
        );
        assert!(select_input(EncoderInput::Bgra, "hevc_amf", &[Pixel::NV12]).is_err());
        assert_eq!(
            select_input(EncoderInput::Bgra, "libx264", &formats),
            Ok(Pixel::YUV420P)
        );
        assert_eq!(
            select_input(EncoderInput::default(), "h264_nvenc", &formats),
            Ok(Pixel::YUV420P)
        );
        assert_eq!(EncoderInput::from_env("BGRA"), Some(EncoderInput::Bgra));
        assert_eq!(EncoderInput::from_env("mistyped"), None);
    }

    #[test]
    fn quick_sync_uses_nv12_for_default_and_auto_input() {
        for name in ["h264_qsv", "hevc_qsv"] {
            for mode in [EncoderInput::default(), EncoderInput::Auto] {
                assert_eq!(
                    select_input(mode, name, &[Pixel::NV12, Pixel::QSV]),
                    Ok(Pixel::NV12)
                );
            }
            assert!(select_input(EncoderInput::Bgra, name, &[Pixel::NV12]).is_err());
        }
        assert!(select_input(EncoderInput::Yuv420, "unknown", &[Pixel::QSV]).is_err());
    }

    #[test]
    fn nv12_conversion_preserves_luma_and_interleaves_chroma() {
        ffmpeg_next::init().unwrap();
        let (width, height) = (96, 64);
        let mut bgra = ffmpeg_next::frame::Video::new(Pixel::BGRA, width, height);
        let stride = bgra.stride(0);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let offset = y * stride + x * 4;
                bgra.data_mut(0)[offset..offset + 4].copy_from_slice(&[
                    (x * 2) as u8,
                    (y * 3) as u8,
                    (x + y) as u8,
                    255,
                ]);
            }
        }
        let mut planar = ffmpeg_next::frame::Video::new(Pixel::YUV420P, width, height);
        let mut nv12 = ffmpeg_next::frame::Video::new(Pixel::NV12, width, height);
        for output in [&mut planar, &mut nv12] {
            conversion_context(output.format(), (width, height), (width, height))
                .unwrap()
                .unwrap()
                .run(&bgra, output)
                .unwrap();
        }
        for y in 0..height as usize {
            for x in 0..width as usize {
                assert_eq!(
                    nv12.data(0)[y * nv12.stride(0) + x],
                    planar.data(0)[y * planar.stride(0) + x]
                );
            }
        }
        for y in 0..height as usize / 2 {
            for x in 0..width as usize / 2 {
                // swscale's planar and interleaved SIMD paths can round a
                // chroma sample differently by one on x86.
                assert!(
                    nv12.data(1)[y * nv12.stride(1) + 2 * x]
                        .abs_diff(planar.data(1)[y * planar.stride(1) + x])
                        <= 1
                );
                assert!(
                    nv12.data(1)[y * nv12.stride(1) + 2 * x + 1]
                        .abs_diff(planar.data(2)[y * planar.stride(2) + x])
                        <= 1
                );
            }
        }
        assert!(
            conversion_context(Pixel::BGRA, (width, height), (width, height))
                .unwrap()
                .is_none()
        );
        assert!(
            conversion_context(Pixel::BGRZ, (width, height), (width, height))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn native_bgra_input_still_scales_a_desktop_that_is_too_large() {
        let _ = ffmpeg_next::init();
        let mut scaler = conversion_context(Pixel::BGRA, (64, 16), (32, 8))
            .unwrap()
            .expect("a smaller output needs swscale even for BGRA");
        let mut bgra = ffmpeg_next::frame::Video::new(Pixel::BGRA, 64, 16);
        bgra.data_mut(0).fill(200);
        let mut output = ffmpeg_next::frame::Video::new(Pixel::BGRA, 32, 8);
        scaler.run(&bgra, &mut output).unwrap();
        assert_eq!((output.width(), output.height()), (32, 8));
        assert_eq!(output.data(0)[0], 200);
    }
}
