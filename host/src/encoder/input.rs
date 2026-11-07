use ffmpeg_next::format::Pixel;
use tracing::info;

use crate::capture::FRAME_PADDING;

/// Troubleshooting's encoder input. Saved through `settings::saved_input`,
/// which keeps it readable by older versions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EncoderInput {
    /// Nothing picked: BGRA for NVIDIA's encoders, which convert it on the
    /// graphics card, and YUV420 (NV12 on Quick Sync) for the others. AMD
    /// BGRA is unsupported in the pinned SDK; see docs/decisions.md.
    #[default]
    Automatic,
    /// BGRA whenever the encoder takes it and encodes it as 4:2:0.
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

/// Quick Sync HEVC, which encodes BGRA as a 4:4:4 (HEVC RExt) stream rather
/// than 4:2:0: FFmpeg 7.1's qsvenc.c sets the frame's chroma format from the
/// input's subsampling, none for BGRA, and with no profile set asks for
/// 4:4:4 output. Encoder input picks what the encoder reads, not the
/// stream's chroma; 4:4:4 is untested on the iPad and spends more of the
/// same bitrate on colour. NVENC converts BGRA to 4:2:0 unless asked to keep
/// 4:4:4.
fn encodes_bgra_as_444(codec_name: &str) -> bool {
    codec_name == "hevc_qsv"
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
        EncoderInput::Auto | EncoderInput::Bgra if encodes_bgra_as_444(codec_name) => {
            info!(
                encoder = codec_name,
                input = ?mode,
                "This encoder would stream BGRA as 4:4:4; using YUV420"
            );
            false
        }
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
    if mode == EncoderInput::Bgra && !encodes_bgra_as_444(codec_name) {
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
) -> Result<Option<Scaler>, ffmpeg_next::Error> {
    if !matches!(format, Pixel::YUV420P | Pixel::NV12) {
        return Ok(None);
    }
    Scaler::new(format, width, height, scaler_threads()).map(Some)
}

/// Threads one conversion is split across: half the logical processors, at
/// most four. A 2732x2048 frame takes about 14 ms on one thread of a desktop
/// CPU, most of a 60 fps frame's 16.7 ms.
fn scaler_threads() -> usize {
    threads_for(std::thread::available_parallelism().map_or(1, |count| count.get()))
}

fn threads_for(processors: usize) -> usize {
    (processors / 2).clamp(1, 4)
}

/// swscale converting BGRA to `output` at one size, FAST_BILINEAR, on
/// `threads` threads. FFmpeg 7.1 splits a conversion across threads only in
/// `sws_scale_frame`; `sws_scale`, which ffmpeg-next's `Context::run` calls,
/// always runs on one.
pub(super) struct Scaler {
    context: *mut ffmpeg_next::ffi::SwsContext,
    width: u32,
    height: u32,
    output: Pixel,
    threads: usize,
}

impl Scaler {
    fn new(
        output: Pixel,
        width: u32,
        height: u32,
        threads: usize,
    ) -> Result<Self, ffmpeg_next::Error> {
        use ffmpeg_next::ffi::{
            av_opt_set_int, sws_alloc_context, sws_init_context, AVPixelFormat, SWS_FAST_BILINEAR,
        };
        let context = unsafe { sws_alloc_context() };
        if context.is_null() {
            return Err(ffmpeg_next::Error::Other {
                errno: libc::ENOMEM,
            });
        }
        // Freed by Drop from here on, including on the errors below.
        let scaler = Self {
            context,
            width,
            height,
            output,
            threads,
        };
        let options = [
            (c"srcw", i64::from(width)),
            (c"srch", i64::from(height)),
            (c"src_format", AVPixelFormat::from(Pixel::BGRA) as i64),
            (c"dstw", i64::from(width)),
            (c"dsth", i64::from(height)),
            (c"dst_format", AVPixelFormat::from(output) as i64),
            (c"sws_flags", i64::from(SWS_FAST_BILINEAR)),
            (c"threads", threads as i64),
        ];
        for (name, value) in options {
            let result = unsafe { av_opt_set_int(context.cast(), name.as_ptr(), value, 0) };
            if result < 0 {
                return Err(ffmpeg_next::Error::from(result));
            }
        }
        let result =
            unsafe { sws_init_context(context, std::ptr::null_mut(), std::ptr::null_mut()) };
        if result < 0 {
            return Err(ffmpeg_next::Error::from(result));
        }
        Ok(scaler)
    }

    pub(super) fn threads(&self) -> usize {
        self.threads
    }

    /// Converts `input`, a BGRA frame of the scaler's size, into `output`.
    pub(super) fn run(
        &mut self,
        input: &ffmpeg_next::frame::Video,
        output: &mut ffmpeg_next::frame::Video,
    ) -> Result<(), ffmpeg_next::Error> {
        if input.format() != Pixel::BGRA
            || input.width() != self.width
            || input.height() != self.height
        {
            return Err(ffmpeg_next::Error::InputChanged);
        }
        self.check_output(output)?;
        // SAFETY: both frames own their buffers and have the sizes checked.
        let result = unsafe {
            ffmpeg_next::ffi::sws_scale_frame(self.context, output.as_mut_ptr(), input.as_ptr())
        };
        if result < 0 {
            return Err(ffmpeg_next::Error::from(result));
        }
        Ok(())
    }

    fn check_output(&self, output: &ffmpeg_next::frame::Video) -> Result<(), ffmpeg_next::Error> {
        if output.format() != self.output
            || output.width() != self.width
            || output.height() != self.height
        {
            return Err(ffmpeg_next::Error::OutputChanged);
        }
        Ok(())
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        unsafe { ffmpeg_next::ffi::sws_freeContext(self.context) }
    }
}

/// Converts capture's BGRA rows, `row_bytes` apart, into `output` where they
/// lie: swscale reads the capture buffer itself, with no copy into a BGRA
/// frame first. Rows wider than the scaler's input, or more of them, are a
/// capture with an odd last column or row, which is left out. The call
/// returns when the conversion is done, so the buffer can go back to
/// capture straight after.
///
/// swscale can read up to [`FRAME_PADDING`] bytes past the last row, so
/// `rows` without that many after the picture, as capture's
/// [`frame_buffer`](crate::capture::frame_buffer) has, are copied into a
/// frame FFmpeg allocated first.
pub(super) fn convert_packed_bgra(
    scaler: &mut Scaler,
    rows: &[u8],
    row_bytes: usize,
    output: &mut ffmpeg_next::frame::Video,
) -> Result<(), ffmpeg_next::Error> {
    use ffmpeg_next::ffi::{
        av_buffer_create, av_frame_alloc, av_frame_free, sws_scale_frame, AVPixelFormat,
        AV_BUFFER_FLAG_READONLY,
    };
    let (width, height) = (scaler.width, scaler.height);
    let picture_bytes = width as usize * 4;
    // The last row ends after its picture's pixels.
    let frame_bytes = row_bytes * (height as usize).saturating_sub(1) + picture_bytes;
    if row_bytes < picture_bytes || rows.len() < frame_bytes {
        return Err(ffmpeg_next::Error::InputChanged);
    }
    scaler.check_output(output)?;
    if rows.len() < frame_bytes + FRAME_PADDING {
        let copied = copied_frame(&rows[..frame_bytes], row_bytes, width, height);
        return scaler.run(&copied, output);
    }
    // sws_scale_frame takes reference-counted frames, so `rows` is wrapped
    // in a buffer that never frees it. sws_scale_frame drops its references
    // before it returns, and the frame is freed below, so nothing refers to
    // `rows` after this call.
    unsafe extern "C" fn borrowed(_: *mut libc::c_void, _: *mut u8) {}
    let mut source = unsafe { av_frame_alloc() };
    if source.is_null() {
        return Err(ffmpeg_next::Error::Other {
            errno: libc::ENOMEM,
        });
    }
    // SAFETY: swscale reads `height` rows of `picture_bytes`, `row_bytes`
    // apart, from `rows`, and at most FRAME_PADDING more, checked above, and
    // writes the planes `output` allocated at the output size.
    let result = unsafe {
        let data = rows.as_ptr().cast_mut();
        (*source).format = AVPixelFormat::from(Pixel::BGRA) as libc::c_int;
        (*source).width = width as libc::c_int;
        (*source).height = height as libc::c_int;
        (*source).data[0] = data;
        (*source).linesize[0] = row_bytes as libc::c_int;
        (*source).buf[0] = av_buffer_create(
            data,
            rows.len(),
            Some(borrowed),
            std::ptr::null_mut(),
            AV_BUFFER_FLAG_READONLY,
        );
        let result = if (*source).buf[0].is_null() {
            -libc::ENOMEM
        } else {
            sws_scale_frame(scaler.context, output.as_mut_ptr(), source)
        };
        av_frame_free(&mut source);
        result
    };
    if result < 0 {
        return Err(ffmpeg_next::Error::from(result));
    }
    Ok(())
}

/// The top-left `width`x`height` of `rows`, `row_bytes` apart, copied into a
/// BGRA frame FFmpeg allocated. At odd widths swscale reads one pixel past
/// each row for the last chroma sample, which lands in the frame's unset
/// stride padding, so each row's last pixel is repeated there.
fn copied_frame(
    rows: &[u8],
    row_bytes: usize,
    width: u32,
    height: u32,
) -> ffmpeg_next::frame::Video {
    let picture_bytes = width as usize * 4;
    let mut copied = ffmpeg_next::frame::Video::new(Pixel::BGRA, width, height);
    let stride = copied.stride(0);
    for (y, row) in rows.chunks(row_bytes).take(height as usize).enumerate() {
        let row = &row[..picture_bytes];
        let line = &mut copied.data_mut(0)[y * stride..(y + 1) * stride];
        line[..picture_bytes].copy_from_slice(row);
        if let Some(next) = line.get_mut(picture_bytes..picture_bytes + 4) {
            next.copy_from_slice(&row[picture_bytes - 4..]);
        }
    }
    copied
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
        }
        assert!(select_input(EncoderInput::Bgra, "h264_qsv", &[Pixel::NV12]).is_err());
        assert_eq!(
            select_input(EncoderInput::Bgra, "hevc_qsv", &[Pixel::NV12]),
            Ok(Pixel::NV12)
        );
        assert!(select_input(EncoderInput::Yuv420, "unknown", &[Pixel::QSV]).is_err());
    }

    /// Quick Sync HEVC streams BGRA as 4:4:4, so it gets NV12 whatever was
    /// picked, while NVENC keeps a picked BGRA.
    #[test]
    fn quick_sync_hevc_never_gets_bgra() {
        // What FFmpeg 7.1.1's hevc_qsv advertises.
        let hevc_qsv = [
            Pixel::NV12,
            Pixel::P010LE,
            Pixel::P012LE,
            Pixel::YUYV422,
            Pixel::Y210LE,
            Pixel::QSV,
            Pixel::BGRA,
            Pixel::X2RGB10LE,
            Pixel::VUYX,
            Pixel::XV30LE,
        ];
        for mode in [
            EncoderInput::Automatic,
            EncoderInput::Auto,
            EncoderInput::Bgra,
            EncoderInput::Yuv420,
        ] {
            assert_eq!(
                select_input(mode, "hevc_qsv", &hevc_qsv),
                Ok(Pixel::NV12),
                "{mode:?}"
            );
            assert_eq!(
                select_input(mode, "hevc_qsv", &[Pixel::NV12, Pixel::BGRZ]),
                Ok(Pixel::NV12),
                "{mode:?}"
            );
        }
        let nvenc = [Pixel::YUV420P, Pixel::NV12, Pixel::BGRZ, Pixel::BGRA];
        for name in ["h264_nvenc", "hevc_nvenc"] {
            for mode in [EncoderInput::Auto, EncoderInput::Bgra] {
                assert_eq!(select_input(mode, name, &nvenc), Ok(Pixel::BGRA));
            }
        }
        let h264_qsv = [Pixel::NV12, Pixel::QSV];
        for mode in [
            EncoderInput::Automatic,
            EncoderInput::Auto,
            EncoderInput::Yuv420,
        ] {
            assert_eq!(select_input(mode, "h264_qsv", &h264_qsv), Ok(Pixel::NV12));
        }
    }

    fn fill_pattern(rows: &mut [u8], row_bytes: usize) {
        for (i, byte) in rows.iter_mut().enumerate() {
            *byte = (i * 7 + i / row_bytes * 13) as u8;
        }
    }

    /// What the copy into an aligned BGRA frame gives, converted by
    /// ffmpeg-next's single-threaded `sws_scale`.
    fn converted_from_copy(
        format: Pixel,
        rows: &[u8],
        width: u32,
        height: u32,
    ) -> ffmpeg_next::frame::Video {
        let frame_bytes = width as usize * 4 * height as usize;
        let copied = copied_frame(&rows[..frame_bytes], width as usize * 4, width, height);
        let mut converted = ffmpeg_next::frame::Video::new(format, width, height);
        ffmpeg_next::software::scaling::Context::get(
            Pixel::BGRA,
            width,
            height,
            format,
            width,
            height,
            ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
        )
        .unwrap()
        .run(&copied, &mut converted)
        .unwrap();
        converted
    }

    fn assert_same_picture(
        before: &ffmpeg_next::frame::Video,
        after: &ffmpeg_next::frame::Video,
        label: &str,
    ) {
        for plane in 0..before.planes() {
            let visible = before.plane_width(plane) as usize
                * if before.format() == Pixel::NV12 && plane == 1 {
                    2
                } else {
                    1
                };
            for y in 0..before.plane_height(plane) as usize {
                let at = |frame: &ffmpeg_next::frame::Video| {
                    let start = y * frame.stride(plane);
                    frame.data(plane)[start..start + visible].to_vec()
                };
                assert_eq!(at(before), at(after), "{label} plane {plane} row {y}");
            }
        }
    }

    /// swscale reading a padded capture buffer, on one thread or several,
    /// gives the same picture as single-threaded swscale reading the copy into
    /// an aligned BGRA frame it replaced, at widths whose rows are (1680) and
    /// aren't (2732, 1366) 64-byte multiples, and whole iPad screens.
    #[test]
    fn converting_capture_rows_in_place_matches_the_copied_frame() {
        ffmpeg_next::init().unwrap();
        for (width, height) in [
            (2732u32, 2048u32),
            (2732, 64),
            (1680, 1050),
            (96, 64),
            (1366, 32),
            (102, 18),
        ] {
            let row_bytes = width as usize * 4;
            let frame_bytes = row_bytes * height as usize;
            let mut rows = crate::capture::frame_buffer(frame_bytes);
            fill_pattern(&mut rows[..frame_bytes], row_bytes);
            for format in [Pixel::YUV420P, Pixel::NV12] {
                let before = converted_from_copy(format, &rows, width, height);
                for threads in [1, 4] {
                    let mut scaler = Scaler::new(format, width, height, threads).unwrap();
                    let mut after = ffmpeg_next::frame::Video::new(format, width, height);
                    convert_packed_bgra(&mut scaler, &rows, row_bytes, &mut after).unwrap();
                    let label = format!("{width}x{height} {format:?} threads={threads}");
                    assert_same_picture(&before, &after, &label);
                }
                let mut scaler = conversion_context(format, width, height).unwrap().unwrap();
                let mut after = ffmpeg_next::frame::Video::new(format, width, height);
                // A short buffer or another size is refused, never read past.
                assert_eq!(
                    convert_packed_bgra(
                        &mut scaler,
                        &rows[..frame_bytes - 1],
                        row_bytes,
                        &mut after
                    ),
                    Err(ffmpeg_next::Error::InputChanged)
                );
                assert_eq!(
                    convert_packed_bgra(&mut scaler, &rows, row_bytes - 4, &mut after),
                    Err(ffmpeg_next::Error::InputChanged)
                );
                let mut other = ffmpeg_next::frame::Video::new(format, width + 2, height);
                assert_eq!(
                    convert_packed_bgra(&mut scaler, &rows, row_bytes, &mut other),
                    Err(ffmpeg_next::Error::OutputChanged)
                );
            }
        }
    }

    #[test]
    fn conversions_use_half_the_processors_up_to_four_threads() {
        ffmpeg_next::init().unwrap();
        for (processors, threads) in [(1, 1), (2, 1), (4, 2), (6, 3), (8, 4), (16, 4), (64, 4)] {
            assert_eq!(threads_for(processors), threads, "{processors} processors");
        }
        let scaler = conversion_context(Pixel::YUV420P, 2732, 2048)
            .unwrap()
            .unwrap();
        assert_eq!(scaler.threads(), scaler_threads());
        let mut set = 0;
        let result = unsafe {
            ffmpeg_next::ffi::av_opt_get_int(
                scaler.context.cast(),
                c"threads".as_ptr(),
                0,
                &mut set,
            )
        };
        assert!(result >= 0);
        assert_eq!(set, scaler_threads() as i64);
    }

    #[test]
    fn capture_padding_is_ffmpegs() {
        assert_eq!(
            FRAME_PADDING,
            ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize
        );
    }

    /// `len` bytes that end where a page that faults on any access starts.
    #[cfg(unix)]
    struct Guarded {
        base: *mut libc::c_void,
        mapped: usize,
        len: usize,
    }

    #[cfg(unix)]
    impl Guarded {
        fn new(len: usize) -> Self {
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            let readable = len.div_ceil(page) * page;
            // SAFETY: a fresh private mapping, `readable` bytes then one
            // page that faults on any access; unmapped on drop.
            unsafe {
                let base = libc::mmap(
                    std::ptr::null_mut(),
                    readable + page,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                );
                assert_ne!(base, libc::MAP_FAILED);
                let guard = base.cast::<u8>().add(readable);
                assert_eq!(libc::mprotect(guard.cast(), page, libc::PROT_NONE), 0);
                Self {
                    base,
                    mapped: readable + page,
                    len,
                }
            }
        }

        fn bytes(&mut self) -> &mut [u8] {
            let start = self.mapped - unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            // SAFETY: the last `len` readable bytes of the mapping.
            unsafe {
                std::slice::from_raw_parts_mut(
                    self.base.cast::<u8>().add(start - self.len),
                    self.len,
                )
            }
        }
    }

    #[cfg(unix)]
    impl Drop for Guarded {
        fn drop(&mut self) {
            assert_eq!(unsafe { libc::munmap(self.base, self.mapped) }, 0);
        }
    }

    /// Rows with no readable bytes after them, ending where an unreadable
    /// page starts, convert to the same picture without a crash: swscale
    /// can read past the last row, so they take the copy.
    #[cfg(unix)]
    #[test]
    fn rows_without_padding_are_never_read_past() {
        ffmpeg_next::init().unwrap();
        for (width, height) in [(2733u32, 2048u32), (1365, 32), (17, 3)] {
            let row_bytes = width as usize * 4;
            let frame_bytes = row_bytes * height as usize;
            let mut mapping = Guarded::new(frame_bytes);
            let rows = mapping.bytes();
            fill_pattern(rows, row_bytes);
            // The pixel swscale reads after each odd-width row is set.
            let copied = copied_frame(rows, row_bytes, width, height);
            let stride = copied.stride(0);
            for y in 0..height as usize {
                let line = &copied.data(0)[y * stride..y * stride + row_bytes + 4];
                assert_eq!(line[row_bytes..], line[row_bytes - 4..row_bytes]);
            }
            for format in [Pixel::YUV420P, Pixel::NV12] {
                let before = converted_from_copy(format, rows, width, height);
                for threads in [1, 4] {
                    let mut scaler = Scaler::new(format, width, height, threads).unwrap();
                    let mut after = ffmpeg_next::frame::Video::new(format, width, height);
                    convert_packed_bgra(&mut scaler, rows, row_bytes, &mut after).unwrap();
                    let label = format!("{width}x{height} {format:?} threads={threads}");
                    assert_same_picture(&before, &after, &label);
                }
            }
        }
    }

    /// Capture's padded rows are converted where they lie, on one thread or
    /// several, without reading past the padding: an unreadable page starts
    /// right after it. Even widths give the copy's picture; at odd widths the
    /// last chroma column takes the next row's first pixel instead.
    #[cfg(unix)]
    #[test]
    fn padded_rows_are_read_in_place_no_further_than_the_padding() {
        ffmpeg_next::init().unwrap();
        for (width, height) in [(2732u32, 2048u32), (2733, 2048), (1680, 1050), (17, 3)] {
            let row_bytes = width as usize * 4;
            let frame_bytes = row_bytes * height as usize;
            let mut mapping = Guarded::new(frame_bytes + FRAME_PADDING);
            let rows = mapping.bytes();
            fill_pattern(&mut rows[..frame_bytes], row_bytes);
            for format in [Pixel::YUV420P, Pixel::NV12] {
                let before = converted_from_copy(format, rows, width, height);
                for threads in [1, 4] {
                    let mut scaler = Scaler::new(format, width, height, threads).unwrap();
                    let mut after = ffmpeg_next::frame::Video::new(format, width, height);
                    convert_packed_bgra(&mut scaler, rows, row_bytes, &mut after).unwrap();
                    if width % 2 == 0 {
                        let label = format!("{width}x{height} {format:?} threads={threads}");
                        assert_same_picture(&before, &after, &label);
                    }
                }
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
