use ffmpeg_next::format::Pixel;

/// Troubleshooting's encoder input. Saved through `settings::saved_input`,
/// which keeps it readable by older versions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EncoderInput {
    /// Nothing picked: BGRA for NVIDIA's encoders, which convert it on the
    /// graphics card, and YUV420 (NV12 on Quick Sync) for the others. AMD
    /// BGRA is unsupported in the pinned SDK; see docs/decisions.md.
    #[default]
    Automatic,
    /// BGRA whenever the encoder takes it.
    Auto,
    Bgra,
    Yuv420,
}

impl EncoderInput {
    pub fn from_env(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "automatic" => Some(Self::Automatic),
            "auto" => Some(Self::Auto),
            "bgra" => Some(Self::Bgra),
            "yuv420" | "yuv420p" => Some(Self::Yuv420),
            _ => None,
        }
    }
}

fn is_nvenc(codec_name: &str) -> bool {
    matches!(codec_name, "h264_nvenc" | "hevc_nvenc")
}

pub(super) fn select_input(
    mode: EncoderInput,
    codec_name: &str,
    supported: &[Pixel],
) -> Result<Pixel, &'static str> {
    if codec_name == "libx264" {
        return Ok(Pixel::YUV420P);
    }
    let native = match mode {
        EncoderInput::Automatic => is_nvenc(codec_name),
        EncoderInput::Auto | EncoderInput::Bgra => true,
        EncoderInput::Yuv420 => false,
    };
    if native {
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

/// Whether an open that failed at `format` is tried again as YUV420: only
/// when nothing was picked, so Automatic chose BGRA (an NVIDIA encoder).
/// A picked input is kept and fails as before.
pub(super) fn retries_as_yuv(mode: EncoderInput, format: Pixel) -> bool {
    mode == EncoderInput::Automatic && matches!(format, Pixel::BGRA | Pixel::BGRZ)
}

pub(super) fn conversion_context(
    format: Pixel,
    width: u32,
    height: u32,
) -> Result<Option<ffmpeg_next::software::scaling::Context>, ffmpeg_next::Error> {
    if !matches!(format, Pixel::YUV420P | Pixel::NV12) {
        return Ok(None);
    }
    ffmpeg_next::software::scaling::Context::get(
        Pixel::BGRA,
        width,
        height,
        format,
        width,
        height,
        ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
    )
    .map(Some)
}

/// Converts capture's tightly packed BGRA rows into `output` where they lie:
/// swscale reads the capture buffer itself, with no copy into a BGRA frame
/// first. The call returns when the conversion is done, so the buffer can
/// go back to capture straight after.
pub(super) fn convert_packed_bgra(
    scaler: &mut ffmpeg_next::software::scaling::Context,
    rows: &[u8],
    output: &mut ffmpeg_next::frame::Video,
) -> Result<(), ffmpeg_next::Error> {
    let input = *scaler.input();
    let row_bytes = input.width as usize * 4;
    if input.format != Pixel::BGRA || rows.len() < row_bytes * input.height as usize {
        return Err(ffmpeg_next::Error::InputChanged);
    }
    let wanted = scaler.output();
    if output.format() != wanted.format
        || output.width() != wanted.width
        || output.height() != wanted.height
    {
        return Err(ffmpeg_next::Error::OutputChanged);
    }
    let planes = [
        rows.as_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        std::ptr::null(),
    ];
    let strides = [row_bytes as libc::c_int, 0, 0, 0];
    // swscale reads `input.height` rows of `row_bytes` from `rows`, checked
    // above, and writes the planes `output` allocated at the output size.
    let result = unsafe {
        ffmpeg_next::ffi::sws_scale(
            scaler.as_mut_ptr(),
            planes.as_ptr(),
            strides.as_ptr(),
            0,
            input.height as libc::c_int,
            (*output.as_mut_ptr()).data.as_ptr(),
            (*output.as_mut_ptr()).linesize.as_ptr(),
        )
    };
    if result < 0 {
        return Err(ffmpeg_next::Error::from(result));
    }
    Ok(())
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
            select_input(EncoderInput::Yuv420, "h264_nvenc", &formats),
            Ok(Pixel::YUV420P)
        );
        assert_eq!(EncoderInput::from_env("BGRA"), Some(EncoderInput::Bgra));
        assert_eq!(
            EncoderInput::from_env("automatic"),
            Some(EncoderInput::Automatic)
        );
        assert_eq!(EncoderInput::from_env("mistyped"), None);
    }

    /// Nothing picked gives NVIDIA's encoders BGRA and every other encoder
    /// the input it had before; a picked input decides for every encoder.
    #[test]
    fn automatic_gives_only_nvenc_bgra_and_a_picked_input_wins() {
        // What each FFmpeg 7.1 encoder advertises.
        let nvenc = [
            Pixel::YUV420P,
            Pixel::NV12,
            Pixel::P010LE,
            Pixel::YUV444P,
            Pixel::BGRZ,
            Pixel::BGRA,
            Pixel::RGBA,
            Pixel::GBRP,
        ];
        let amf = [Pixel::NV12, Pixel::YUV420P, Pixel::D3D11, Pixel::DXVA2_VLD];
        let h264_qsv = [Pixel::NV12, Pixel::P010LE, Pixel::QSV];
        let hevc_qsv = [Pixel::NV12, Pixel::P010LE, Pixel::BGRA, Pixel::QSV];
        let x264 = [Pixel::YUV420P, Pixel::YUV444P, Pixel::NV12];
        let automatic = EncoderInput::default();
        assert_eq!(automatic, EncoderInput::Automatic);
        for name in ["h264_nvenc", "hevc_nvenc"] {
            assert_eq!(select_input(automatic, name, &nvenc), Ok(Pixel::BGRA));
            // A build whose NVENC takes no BGRA stays on YUV420P.
            assert_eq!(
                select_input(automatic, name, &[Pixel::YUV420P]),
                Ok(Pixel::YUV420P)
            );
            assert_eq!(
                select_input(EncoderInput::Yuv420, name, &nvenc),
                Ok(Pixel::YUV420P)
            );
            assert_eq!(
                select_input(EncoderInput::Bgra, name, &nvenc),
                Ok(Pixel::BGRA)
            );
            assert_eq!(
                select_input(EncoderInput::Auto, name, &nvenc),
                Ok(Pixel::BGRA)
            );
        }
        // Automatic is what YUV420, the old default, gave them.
        for (name, formats) in [
            ("h264_amf", &amf[..]),
            ("hevc_amf", &amf[..]),
            ("h264_amf", &[Pixel::NV12, Pixel::BGRZ][..]),
            ("h264_qsv", &h264_qsv[..]),
            ("hevc_qsv", &hevc_qsv[..]),
            ("libx264", &x264[..]),
            ("libx265", &x264[..]),
            ("h264_videotoolbox", &[Pixel::NV12, Pixel::BGRA][..]),
            ("unknown", &[][..]),
        ] {
            assert_eq!(
                select_input(automatic, name, formats),
                select_input(EncoderInput::Yuv420, name, formats),
                "{name}"
            );
            assert!(!matches!(
                select_input(automatic, name, formats),
                Ok(Pixel::BGRA | Pixel::BGRZ)
            ));
        }
        assert_eq!(
            select_input(EncoderInput::Bgra, "hevc_qsv", &hevc_qsv),
            Ok(Pixel::BGRA)
        );
        assert_eq!(
            select_input(automatic, "libx264", &[Pixel::BGRA]),
            Ok(Pixel::YUV420P)
        );
    }

    #[test]
    fn only_automatic_bgra_is_tried_again_as_yuv() {
        assert!(retries_as_yuv(EncoderInput::Automatic, Pixel::BGRA));
        assert!(retries_as_yuv(EncoderInput::Automatic, Pixel::BGRZ));
        assert!(!retries_as_yuv(EncoderInput::Automatic, Pixel::YUV420P));
        assert!(!retries_as_yuv(EncoderInput::Automatic, Pixel::NV12));
        for picked in [EncoderInput::Auto, EncoderInput::Bgra, EncoderInput::Yuv420] {
            assert!(!retries_as_yuv(picked, Pixel::BGRA));
        }
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

    /// swscale reading capture's packed rows gives the same picture as the
    /// copy into an aligned BGRA frame it replaced, at widths whose rows are
    /// and aren't 64-byte multiples.
    #[test]
    fn converting_capture_rows_in_place_matches_the_copied_frame() {
        ffmpeg_next::init().unwrap();
        for (width, height) in [(2732u32, 64u32), (96, 64), (1366, 32), (102, 18)] {
            let row_bytes = width as usize * 4;
            let rows: Vec<u8> = (0..row_bytes * height as usize)
                .map(|i| (i * 7 + i / row_bytes * 13) as u8)
                .collect();
            for format in [Pixel::YUV420P, Pixel::NV12] {
                let mut scaler = conversion_context(format, width, height).unwrap().unwrap();
                let mut copied = ffmpeg_next::frame::Video::new(Pixel::BGRA, width, height);
                let stride = copied.stride(0);
                for y in 0..height as usize {
                    copied.data_mut(0)[y * stride..y * stride + row_bytes]
                        .copy_from_slice(&rows[y * row_bytes..(y + 1) * row_bytes]);
                }
                let mut before = ffmpeg_next::frame::Video::new(format, width, height);
                scaler.run(&copied, &mut before).unwrap();
                let mut after = ffmpeg_next::frame::Video::new(format, width, height);
                convert_packed_bgra(&mut scaler, &rows, &mut after).unwrap();
                for plane in 0..before.planes() {
                    let visible = before.plane_width(plane) as usize
                        * if format == Pixel::NV12 && plane == 1 {
                            2
                        } else {
                            1
                        };
                    for y in 0..before.plane_height(plane) as usize {
                        let at = |frame: &ffmpeg_next::frame::Video| {
                            let start = y * frame.stride(plane);
                            frame.data(plane)[start..start + visible].to_vec()
                        };
                        assert_eq!(
                            at(&before),
                            at(&after),
                            "{width}x{height} {format:?} {plane} {y}"
                        );
                    }
                }
                // A short buffer or another size is refused, never read past.
                assert_eq!(
                    convert_packed_bgra(&mut scaler, &rows[..rows.len() - 1], &mut after),
                    Err(ffmpeg_next::Error::InputChanged)
                );
                let mut other = ffmpeg_next::frame::Video::new(format, width + 2, height);
                assert_eq!(
                    convert_packed_bgra(&mut scaler, &rows, &mut other),
                    Err(ffmpeg_next::Error::OutputChanged)
                );
            }
        }
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
            conversion_context(output.format(), width, height)
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
        assert!(conversion_context(Pixel::BGRA, width, height)
            .unwrap()
            .is_none());
        assert!(conversion_context(Pixel::BGRZ, width, height)
            .unwrap()
            .is_none());
    }
}
