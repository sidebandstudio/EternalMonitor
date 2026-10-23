use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::slice;
use std::sync::atomic::Ordering;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use eternal_wire::h264::{
    contains_nal_type, describe_extradata_format, hex_prefix, is_amf_encoder,
    normalize_h264_payload_with_info, parameter_sets_from_extradata, H264BitstreamState,
};

use crate::capture::FrameSlot;
use crate::control::SharedControl;
use crate::gpu::GpuInfo;
use crate::stats::PIPELINE_STATS;

pub mod input;
use input::{select_input, EncoderInput};
#[cfg(target_os = "macos")]
mod vimage;

const CHANNEL_CAPACITY: usize = 4;
const AMF_CAPTURE_PACKET_LIMIT: u64 = 120;
const AMF_IDR_WARNING_PACKET: u64 = 60;
const AMF_FORCED_INTRA_PERIOD: u64 = 30;

/// Encoded H.264 NAL unit ready for transport.
#[derive(Debug, Clone)]
pub struct NALUnit {
    pub data: Vec<u8>,
    pub sequence: u64,
    pub timestamp: Instant,
    pub encode_duration_us: u128,
    pub is_keyframe: bool,
}

/// Runs the H.264 encode loop on the CURRENT thread until the capture slot
/// closes or stop is requested. The supervisor spawns and monitors this.
pub fn run_encode_stage(
    frames: FrameSlot,
    tx: mpsc::Sender<NALUnit>,
    shared: SharedControl,
    gpu: GpuInfo,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(target_os = "macos")]
    crate::capture::timing::set_stream_qos();
    let result = run_encode_loop(frames, tx, shared, gpu, generation);
    if let Err(ref e) = result {
        error!(error = %e, "Encode loop exited with error");
    }
    result
}

/// Capacity of the encoded-output channel (lossless: encoded frames are never
/// dropped — a lost P-frame corrupts until the next IDR).
pub const NAL_CHANNEL_CAPACITY: usize = CHANNEL_CAPACITY;

fn run_encode_loop(
    frames: FrameSlot,
    tx: mpsc::Sender<NALUnit>,
    shared: SharedControl,
    gpu: GpuInfo,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Apply GUI encoder override at pipeline start. The override is consulted only here;
    // mid-stream changes don't take effect until the user requests a Restart.
    let (mut encoder_name, codec_display_name) = {
        let override_name = shared.encoder_override.lock().clone();
        match override_name {
            Some(name) if ffmpeg_next::encoder::find_by_name(&name).is_some() => {
                let display = match name.as_str() {
                    "h264_nvenc" => "H.264 (NVENC)",
                    "h264_amf" => "H.264 (AMF)",
                    "h264_qsv" => "H.264 (QSV)",
                    "libx264" => "H.264 (x264)",
                    other => {
                        warn!(encoder = %other, "Unknown encoder override label");
                        other
                    }
                };
                let display = display.to_string();
                info!(encoder = %name, "Encoder override honoured");
                (name, display)
            }
            Some(name) => {
                warn!(
                    encoder = %name,
                    "Encoder override not available in FFmpeg — falling back to auto-detected encoder"
                );
                (gpu.encoder_name.clone(), gpu.codec_display_name.clone())
            }
            None => (gpu.encoder_name.clone(), gpu.codec_display_name.clone()),
        }
    };

    if ffmpeg_next::encoder::find_by_name(&encoder_name).is_none() {
        return Err(format!("{} codec not found in FFmpeg", encoder_name).into());
    }
    info!(encoder = %encoder_name, "Found encoder codec");
    {
        let mut stats = PIPELINE_STATS.lock();
        stats.set_codec_name(&codec_display_name);
        // Fresh pipeline start: clear any stale software-fallback flag. The encode loop
        // re-sets it below only if a hardware encoder actually fails to open. A deliberate
        // libx264 override is not a fallback, so it intentionally leaves this false.
        stats.set_software_fallback(false);
    }

    let mut encoder_state: Option<EncoderState> = None;
    let input_mode = *shared.encoder_input.lock();
    let mut frames_since_last_idr: u64 = 0;
    let test_idr_period = std::env::var("ETERNAL_FORCE_IDR_PERIOD")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|&period| period > 0);
    // Bounded retries for the AMF startup case where a keyframe is emitted before any SPS/PPS are
    // available (empty extradata + no inline parameter sets). We nudge another IDR a few times so
    // a subsequent keyframe carrying inline parameter sets can recover, instead of leaving the
    // iPad unable to build a format description.
    let mut amf_startup_idr_retries: u8 = 0;
    // Suppresses reopen retry storms after a failed bitrate change.
    let mut failed_reopen: Option<(u32, u32)> = None;
    // HEVC gating memory: warn only once when no HEVC sibling exists, and
    // don't retry an HEVC open that failed until the preference or the
    // session changes (hevc_wanted going false resets it).
    let mut hevc_unavailable_warned = false;
    let mut hevc_open_failed = false;
    let mut last_keyframe_reopen: Option<Instant> = None;
    let mut pending_keyframe = false;

    // Guarantee the very first frame of every pipeline is an IDR for all encoders, so the
    // iPad never sits in `waitingForSyncSample` after a connect or pipeline restart.
    shared.force_next_idr.store(true, Ordering::SeqCst);

    // Test-harness fault injection: die after N encoded frames so the E2E can
    // prove the supervisor's auto-restart. Consumed once (self-clearing) so
    // the restarted generation streams normally.
    let fault_after: Option<u64> = std::env::var("ETERNAL_FAULT_ENCODER_AFTER")
        .ok()
        .and_then(|v| v.trim().parse().ok());

    while let Some(raw_frame) = frames.blocking_take() {
        if !shared.running.load(Ordering::SeqCst)
            || !crate::supervisor::generation_is_current(generation)
        {
            info!("Encoder loop stopping (stop requested or generation superseded)");
            break;
        }

        if let Some(after) = fault_after {
            if raw_frame.frame_number >= after {
                std::env::remove_var("ETERNAL_FAULT_ENCODER_AFTER");
                return Err("injected encoder fault (ETERNAL_FAULT_ENCODER_AFTER)".into());
            }
        }

        if raw_frame.data.is_empty() {
            warn!(
                frame = raw_frame.frame_number,
                "Skipping frame with empty data"
            );
            continue;
        }

        let desired_bitrate = shared.abr_current_bps.load(Ordering::SeqCst).max(500_000);
        let target_fps = shared.effective_fps();

        // HEVC negotiation, re-checked every frame so a settings toggle or a
        // new session takes effect at the next frame boundary: the user's
        // preference AND the connected client's advertised decode capability.
        let hevc_wanted = shared.hevc_enabled.load(Ordering::SeqCst)
            && shared.session.lock().client_supports_hevc();
        if !hevc_wanted {
            hevc_open_failed = false;
        }
        let desired_encoder: String = if hevc_wanted && !hevc_open_failed {
            match hevc_variant_of(&encoder_name) {
                Some(hevc) if ffmpeg_next::encoder::find_by_name(hevc).is_some() => {
                    hevc.to_string()
                }
                _ => {
                    if !hevc_unavailable_warned {
                        hevc_unavailable_warned = true;
                        warn!(
                            encoder = %encoder_name,
                            "HEVC requested but no H.265 encoder is available — staying on H.264"
                        );
                    }
                    encoder_name.clone()
                }
            }
        } else {
            encoder_name.clone()
        };

        if let Some(state) = encoder_state.as_mut() {
            if state.update_bitrate_without_reopening(&desired_encoder, desired_bitrate, target_fps)
            {
                failed_reopen = None;
            }
        }

        // Capture rebuilds at the new size after a display mode change (Radeon
        // Super Resolution, VSR, GPU scaling, a resized virtual display). The
        // open session's frame buffers no longer fit.
        let size_changed = encoder_state
            .as_ref()
            .is_some_and(|state| !state.accepts_frame(raw_frame.width, raw_frame.height));
        // Latch requests before deciding whether to reopen. Requests received
        // during the rate limit must survive until AMF can emit an IDR.
        pending_keyframe |= shared.force_next_idr.swap(false, Ordering::SeqCst);
        let keyframe_reopen = pending_keyframe
            && encoder_state.as_ref().is_some_and(|state| {
                reopen_for_keyframe(
                    &state.opened_encoder,
                    state.frames_submitted,
                    last_keyframe_reopen.map(|at| at.elapsed()),
                )
            });

        if encoder_state.is_none() {
            match EncoderState::new(
                &desired_encoder,
                raw_frame.width,
                raw_frame.height,
                desired_bitrate,
                target_fps,
                input_mode,
            ) {
                Ok(state) => {
                    encoder_state = Some(state);
                    note_encoder_opened(&shared, &desired_encoder);
                }
                Err(e) if is_hevc_encoder(&desired_encoder) => {
                    warn!(
                        encoder = %desired_encoder,
                        error = %e,
                        "HEVC encoder failed to open — staying on H.264"
                    );
                    hevc_open_failed = true;
                    continue;
                }
                Err(e) if encoder_name != "libx264" => {
                    warn!(
                        encoder = %encoder_name,
                        error = %e,
                        "Preferred encoder failed to open; trying remaining encoders"
                    );
                    let (name, state) = open_remaining_encoders(&encoder_name, |name| {
                        EncoderState::new(
                            name,
                            raw_frame.width,
                            raw_frame.height,
                            desired_bitrate,
                            target_fps,
                            input_mode,
                        )
                    })?;
                    encoder_name = name.to_string();
                    encoder_state = Some(state);
                    PIPELINE_STATS
                        .lock()
                        .set_software_fallback(encoder_name == "libx264");
                    note_encoder_opened(&shared, &encoder_name);
                }
                Err(e) => return Err(e),
            }
        } else if size_changed
            || keyframe_reopen
            || encoder_state.as_ref().is_some_and(|state| {
                state.opened_encoder != desired_encoder
                    || ((state.opened_bitrate != desired_bitrate || state.opened_fps != target_fps)
                        && failed_reopen != Some((desired_bitrate, target_fps)))
            })
        {
            // Bitrate changed (ABR rung or the user's slider), the
            // negotiated codec changed (HEVC toggle / client capability), the
            // capture size changed (display mode change), or AMF owes a
            // keyframe: hardware encoders ignore parameter pokes on an open
            // context, so each means a session reopen (~50-200ms hiccup, no
            // transport teardown, same stream epoch) followed by a forced IDR.
            if keyframe_reopen {
                last_keyframe_reopen = Some(Instant::now());
            }
            let previous = encoder_state
                .as_ref()
                .map(|s| s.opened_bitrate)
                .unwrap_or(0);
            match EncoderState::new(
                &desired_encoder,
                raw_frame.width,
                raw_frame.height,
                desired_bitrate,
                target_fps,
                input_mode,
            ) {
                Ok(state) => {
                    info!(
                        from = previous,
                        to = desired_bitrate,
                        encoder = %desired_encoder,
                        width = raw_frame.width,
                        height = raw_frame.height,
                        size_changed,
                        keyframe = keyframe_reopen,
                        "Reopened encoder session"
                    );
                    encoder_state = Some(state);
                    failed_reopen = None;
                    pending_keyframe = true;
                    note_encoder_opened(&shared, &desired_encoder);
                }
                // The old session cannot take a frame of the new size. A
                // pipeline restart opens a fresh encoder, with the software
                // fallback if the hardware one rejects this size.
                Err(e) if size_changed => {
                    return Err(format!(
                        "{desired_encoder} could not reopen at {}x{}: {e}",
                        raw_frame.width, raw_frame.height
                    )
                    .into());
                }
                Err(e) if is_hevc_encoder(&desired_encoder) => {
                    warn!(
                        error = %e,
                        encoder = %desired_encoder,
                        "HEVC reopen failed — keeping the current session"
                    );
                    hevc_open_failed = true;
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        bitrate = desired_bitrate,
                        "Encoder reopen failed — keeping the current session"
                    );
                    failed_reopen = Some((desired_bitrate, target_fps));
                }
            }
        }

        let encoder = encoder_state
            .as_mut()
            .expect("Encoder state must be initialized");
        let current_bitrate = encoder.opened_bitrate;
        let opened_is_hevc = is_hevc_encoder(&encoder.opened_encoder);

        let src_row_bytes = (raw_frame.width * 4) as usize;
        #[cfg(target_os = "macos")]
        let converted = match encoder.native_converter.as_ref().map(|converter| {
            converter.convert(
                &raw_frame.data,
                raw_frame.width,
                raw_frame.height,
                src_row_bytes,
                &mut encoder.frame,
            )
        }) {
            Some(Ok(())) => true,
            Some(Err(error)) => {
                warn!(%error, "Using swscale after native color conversion failed");
                encoder.native_converter = None;
                false
            }
            None => false,
        };
        #[cfg(not(target_os = "macos"))]
        let converted = false;
        if !converted {
            let input_frame = encoder.bgra_frame.as_mut().unwrap_or(&mut encoder.frame);
            let stride = input_frame.stride(0);
            let bgra_plane = input_frame.data_mut(0);
            for y in 0..raw_frame.height as usize {
                let src_offset = y * src_row_bytes;
                let dst_offset = y * stride;
                bgra_plane[dst_offset..dst_offset + src_row_bytes]
                    .copy_from_slice(&raw_frame.data[src_offset..src_offset + src_row_bytes]);
            }
            if let (Some(scaler), Some(bgra)) = (&mut encoder.scaler, &encoder.bgra_frame) {
                scaler.run(bgra, &mut encoder.frame)?;
            }
        }
        // Conversion finished synchronously. Recycle capture's buffer before
        // the codec or sender can block, including on the direct macOS path.
        drop(raw_frame.data);
        let pts = encoder.next_pts(raw_frame.timestamp, raw_frame.frame_number);
        encoder.frame.set_pts(Some(pts));
        let force_idr = take_keyframe_request(
            &mut pending_keyframe,
            &encoder.opened_encoder,
            encoder.frames_submitted,
        ) || test_idr_period
            .is_some_and(|period| raw_frame.frame_number.is_multiple_of(period));
        let _forced_intra = prepare_frame_for_encode(
            &mut encoder.frame,
            raw_frame.frame_number,
            &encoder.opened_encoder,
            &mut frames_since_last_idr,
            force_idr,
        );

        let encode_start = Instant::now();
        encoder.send_frame(pts, raw_frame.timestamp, raw_frame.frame_number)?;

        loop {
            match encoder.encoder.receive_packet(&mut encoder.packet) {
                Ok(()) => {
                    let encode_us = encode_start.elapsed().as_micros();
                    let packet_bytes = encoder.packet.data().unwrap_or_default();
                    let packet_is_key = encoder.packet.is_key();
                    let (nal_data, is_keyframe) = if opened_is_hevc {
                        // HEVC path: no parameter-set surgery — the encoders
                        // are configured to repeat VPS/SPS/PPS in-band. If a
                        // keyframe still lacks them and the extradata is
                        // Annex B, prepend it as a safety net.
                        let mut data = packet_bytes.to_vec();
                        let key = packet_is_key || eternal_wire::hevc::contains_keyframe(&data);
                        if key && !eternal_wire::hevc::contains_parameter_sets(&data) {
                            if let Some(extra) = encoder_extradata(&encoder.encoder) {
                                if extra.starts_with(&[0, 0, 0, 1]) || extra.starts_with(&[0, 0, 1])
                                {
                                    let mut with_headers =
                                        Vec::with_capacity(extra.len() + data.len());
                                    with_headers.extend_from_slice(&extra);
                                    with_headers.extend_from_slice(&data);
                                    data = with_headers;
                                }
                            }
                        }
                        (data, key)
                    } else {
                        if encoder.h264.needs_parameter_sets() {
                            encoder
                                .h264
                                .refresh_parameter_sets_from_extradata(encoder_extradata(
                                    &encoder.encoder,
                                ));
                        }
                        let (nal_data, au_info) = normalize_h264_payload_with_info(
                            packet_bytes,
                            packet_is_key,
                            &mut encoder.h264,
                            &encoder.opened_encoder,
                        );
                        let is_keyframe =
                            au_info.is_keyframe(packet_is_key, &encoder.opened_encoder);
                        (nal_data, is_keyframe)
                    };
                    if !opened_is_hevc {
                        encoder.observe_amf_diagnostics(
                            &nal_data,
                            raw_frame.frame_number,
                            packet_is_key,
                            is_keyframe,
                        );
                    }
                    if let Some(capture) = encoder.hevc_capture.as_mut() {
                        if let Err(error) = capture.observe(&nal_data, is_keyframe) {
                            warn!(%error, "Stopping opt-in HEVC diagnostic capture");
                            encoder.hevc_capture = None;
                        }
                    }

                    // AMF startup safety net: a keyframe went out but we still have no cached
                    // SPS/PPS (empty extradata and none inline), so the iPad can't build a format
                    // description. Nudge another IDR a few times — a later keyframe may carry the
                    // parameter sets inline and recover the stream.
                    if is_keyframe
                        && !opened_is_hevc
                        && is_amf_encoder(&encoder.opened_encoder)
                        && encoder.h264.needs_parameter_sets()
                        && amf_startup_idr_retries < 3
                    {
                        amf_startup_idr_retries += 1;
                        warn!(
                            retry = amf_startup_idr_retries,
                            "AMF keyframe still lacks SPS/PPS — forcing another IDR to recover"
                        );
                        shared.force_next_idr.store(true, Ordering::SeqCst);
                    }

                    PIPELINE_STATS
                        .lock()
                        .record_encode(encode_us, nal_data.len(), current_bitrate);
                    crate::capture::heartbeat(&shared.hb_encode_frame_ms);

                    debug!(
                        frame = raw_frame.frame_number,
                        encode_us,
                        nal_bytes = nal_data.len(),
                        bitrate_bps = current_bitrate,
                        "Frame encoded"
                    );

                    let (timestamp, sequence) =
                        encoder.packet_origin((raw_frame.timestamp, raw_frame.frame_number));
                    let nal = NALUnit {
                        data: nal_data,
                        sequence,
                        timestamp,
                        encode_duration_us: encode_us,
                        is_keyframe,
                    };

                    if tx.blocking_send(nal).is_err() {
                        info!("NAL channel closed, stopping encoder");
                        return Ok(());
                    }
                }
                Err(e) if is_eagain(&e) => break,
                Err(e) => return Err(e.into()),
            }
        }
    }

    info!("Capture channel closed, encoder shutting down");
    Ok(())
}

struct EncoderState {
    #[cfg(target_os = "macos")]
    native_converter: Option<vimage::Converter>,
    encoder: ffmpeg_next::codec::encoder::video::Encoder,
    scaler: Option<ffmpeg_next::software::scaling::Context>,
    bgra_frame: Option<ffmpeg_next::frame::Video>,
    frame: ffmpeg_next::frame::Video,
    packet: ffmpeg_next::Packet,
    h264: H264BitstreamState,
    amf_diagnostics: Option<AmfBitstreamDiagnostics>,
    hevc_capture: Option<HevcBitstreamCapture>,
    /// The FFmpeg encoder this session was opened with; a negotiated codec
    /// change (H.264 ↔ HEVC) requests a reopen.
    opened_encoder: String,
    /// Last applied bitrate. Hardware encoders reopen when this changes.
    opened_bitrate: u32,
    opened_fps: u32,
    /// Frames sent to this session. Its first frame is always an IDR.
    frames_submitted: u64,
    /// First frame's capture instant — the PTS epoch for this session.
    pts_epoch: Option<std::time::Instant>,
    last_pts: i64,
    /// Frames sent to this session that have not come back as packets. It
    /// belongs to the session because its PTS restart at 0: a frame the old
    /// session still held could otherwise label a new packet with the same
    /// PTS, seconds stale.
    in_flight: InFlightFrames,
    /// ETERNAL_LEGACY_PTS=1 restores the old dense frame-counter PTS at
    /// time_base 1/60 (escape hatch until the real-PTS path is verified on
    /// NVENC/AMF hardware).
    legacy_pts: bool,
}

impl EncoderState {
    fn update_bitrate_without_reopening(
        &mut self,
        desired_encoder: &str,
        bitrate_bps: u32,
        target_fps: u32,
    ) -> bool {
        if self.opened_encoder != "libx264"
            || desired_encoder != self.opened_encoder
            || target_fps != self.opened_fps
            || bitrate_bps == self.opened_bitrate
        {
            return false;
        }
        // FFmpeg 7.1's libx264 wrapper applies bitrate and VBV changes through
        // x264_encoder_reconfig before the next frame. Preserve its reference
        // frames, PTS epoch and buffers instead of allocating a second encoder
        // for every ABR step. Other encoders retain the existing reopen path.
        self.encoder.set_bit_rate(bitrate_bps.max(500_000) as usize);
        self.encoder
            .set_max_bit_rate(bitrate_bps.max(500_000) as usize);
        // A half-second VBV is enabled at open, as required for x264's live
        // bitrate reconfiguration. The context belongs to this encode thread.
        unsafe {
            (*self.encoder.as_mut_ptr()).rc_buffer_size = (bitrate_bps.max(500_000) / 2) as i32
        };
        info!(
            from = self.opened_bitrate,
            to = bitrate_bps,
            "Updated software encoder bitrate"
        );
        self.opened_bitrate = bitrate_bps;
        true
    }

    fn new(
        encoder_name: &str,
        width: u32,
        height: u32,
        bitrate_bps: u32,
        target_fps: u32,
        input_mode: EncoderInput,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        info!(encoder = encoder_name, width, height, bitrate_bps, target_fps,
            input = ?input_mode, "Opening encoder session");
        let legacy_pts = std::env::var("ETERNAL_LEGACY_PTS").is_ok_and(|v| v.trim() == "1");
        let codec = ffmpeg_next::encoder::find_by_name(encoder_name)
            .ok_or_else(|| format!("{} codec not found in FFmpeg", encoder_name))?;
        // AVCodec owns this terminated static list. A null list means that
        // native BGRA support is unknown, so Auto retains the YUV path.
        let supported = unsafe {
            let mut ptr = (*codec.as_ptr()).pix_fmts;
            let mut formats = Vec::new();
            while !ptr.is_null() && *ptr != ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                formats.push(ffmpeg_next::format::Pixel::from(*ptr));
                ptr = ptr.add(1);
            }
            formats
        };
        info!(encoder = encoder_name, advertised_formats = ?supported, input = ?input_mode, "Encoder advertised input formats");
        let pixel_format = select_input(input_mode, encoder_name, &supported)?;
        let context = ffmpeg_next::codec::Context::new_with_codec(codec);
        let mut encoder = context.encoder().video()?;

        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(pixel_format);
        if legacy_pts {
            encoder.set_time_base(ffmpeg_next::Rational(1, 60));
        } else {
            // Microsecond PTS from real capture times: rate control finally
            // sees the true cadence (idle keepalives, 30 fps modes) instead of
            // being told every frame is 1/60s apart.
            encoder.set_time_base(ffmpeg_next::Rational(1, 1_000_000));
            encoder.set_frame_rate(Some(ffmpeg_next::Rational(target_fps.max(1) as i32, 1)));
        }
        encoder.set_max_b_frames(0);
        encoder.set_bit_rate(bitrate_bps.max(500_000) as usize);
        if encoder_name == "libx264" {
            // x264 can only reconfigure bitrate when VBV was enabled at open.
            // Limit bursts to half a second at the current target bitrate.
            encoder.set_max_bit_rate(bitrate_bps.max(500_000) as usize);
            unsafe {
                (*encoder.as_mut_ptr()).rc_buffer_size = (bitrate_bps.max(500_000) / 2) as i32
            };
        }
        encoder.set_gop(30);
        configure_encoder_flags(&mut encoder, encoder_name);

        let opts = encoder_options(encoder_name);

        let encoder = encoder.open_with(opts).map_err(|error| {
            let code: i32 = error.into();
            format!("{encoder_name} open failed: FFmpeg code={code} error={error}; {width}x{height} input={pixel_format:?} bitrate={bitrate_bps} fps={target_fps}")
        })?;
        log_encoder_configuration(encoder_name, &encoder);
        info!(
            width,
            height,
            bitrate = bitrate_bps,
            fps = target_fps,
            encoder = encoder_name,
            input = ?pixel_format,
            "Encoder opened"
        );
        let mut h264 = H264BitstreamState::default();
        h264.refresh_parameter_sets_from_extradata(encoder_extradata(&encoder));

        let scaler = input::conversion_context(pixel_format, width, height)?;
        let needs_conversion = scaler.is_some();
        if scaler.is_some() {
            info!(output = ?pixel_format, "swscale BGRA conversion context created");
        }

        Ok(Self {
            #[cfg(target_os = "macos")]
            native_converter: if pixel_format == ffmpeg_next::format::Pixel::YUV420P {
                match vimage::Converter::new() {
                    Ok(converter) => {
                        info!("Accelerate BGRA->YUV420P converter ready");
                        Some(converter)
                    }
                    Err(error) => {
                        warn!(%error, "Using swscale color conversion");
                        None
                    }
                }
            } else {
                None
            },
            encoder,
            scaler,
            bgra_frame: needs_conversion.then(|| {
                ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::BGRA, width, height)
            }),
            frame: ffmpeg_next::frame::Video::new(pixel_format, width, height),
            packet: ffmpeg_next::Packet::empty(),
            h264,
            amf_diagnostics: AmfBitstreamDiagnostics::new(encoder_name),
            hevc_capture: HevcBitstreamCapture::for_encoder(encoder_name),
            opened_encoder: encoder_name.to_string(),
            opened_bitrate: bitrate_bps,
            opened_fps: target_fps,
            frames_submitted: 0,
            pts_epoch: None,
            last_pts: -1,
            in_flight: InFlightFrames::default(),
            legacy_pts,
        })
    }

    fn next_pts(&mut self, captured: Instant, frame_number: u64) -> i64 {
        if self.legacy_pts {
            return frame_number as i64;
        }
        let epoch = *self.pts_epoch.get_or_insert(captured);
        let real = captured.saturating_duration_since(epoch).as_micros() as i64;
        // Strict monotonicity (keepalive resends and clock quirks).
        let pts = real.max(self.last_pts + 1);
        self.last_pts = pts;
        pts
    }

    fn send_frame(
        &mut self,
        pts: i64,
        captured: Instant,
        frame_number: u64,
    ) -> Result<(), ffmpeg_next::Error> {
        self.encoder.send_frame(&self.frame)?;
        self.frames_submitted += 1;
        self.in_flight.submitted(pts, captured, frame_number);
        Ok(())
    }

    /// Capture time and number of the frame in `self.packet`. An encoder
    /// with output delay (NVENC holds one frame) returns an earlier frame
    /// than the one just submitted.
    fn packet_origin(&mut self, just_submitted: (Instant, u64)) -> (Instant, u64) {
        self.in_flight
            .take(self.packet.pts())
            .unwrap_or(just_submitted)
    }

    fn accepts_frame(&self, width: u32, height: u32) -> bool {
        self.frame.width() == width && self.frame.height() == height
    }

    fn observe_amf_diagnostics(
        &mut self,
        normalized_packet: &[u8],
        sequence: u64,
        packet_is_key: bool,
        is_keyframe: bool,
    ) {
        if let Some(diagnostics) = self.amf_diagnostics.as_mut() {
            diagnostics.observe_packet(normalized_packet, sequence, packet_is_key, is_keyframe);
        }
    }
}

/// A compiled-in encoder may have no usable device or driver. Try each other
/// Windows backend once before using the CPU. Explicit software selection
/// never enters this path, and non-Windows backends keep their own fallback.
/// Capture time and number of each frame still inside the encoder, by pts. A
/// packet must carry the capture time of the frame it encodes: stamping it
/// with the frame just submitted let NVENC's held frame (the startup frame,
/// or a previous viewer's last frame) pass as new, so a new viewer's first
/// keyframe showed an old desktop. It also skewed the latency reported to
/// the iPad by one frame.
#[derive(Default)]
struct InFlightFrames(std::collections::VecDeque<(i64, Instant, u64)>);

impl InFlightFrames {
    const LIMIT: usize = 64;

    fn submitted(&mut self, pts: i64, captured: Instant, number: u64) {
        if self.0.len() >= Self::LIMIT {
            self.0.pop_front();
        }
        self.0.push_back((pts, captured, number));
    }

    /// Frames are emitted in pts order (no B-frames), so older entries
    /// were dropped by the encoder and can go.
    fn take(&mut self, pts: Option<i64>) -> Option<(Instant, u64)> {
        let pts = pts?;
        while let Some(&(front, captured, number)) = self.0.front() {
            if front > pts {
                return None;
            }
            self.0.pop_front();
            if front == pts {
                return Some((captured, number));
            }
        }
        None
    }
}

fn open_remaining_encoders<T>(
    failed: &str,
    mut open: impl FnMut(&str) -> Result<T, Box<dyn std::error::Error + Send + Sync>>,
) -> Result<(&'static str, T), Box<dyn std::error::Error + Send + Sync>> {
    let hardware: &[&str] = if matches!(failed, "h264_nvenc" | "h264_amf" | "h264_qsv") {
        &["h264_nvenc", "h264_amf", "h264_qsv"]
    } else {
        &[]
    };
    for &name in hardware {
        if name == failed {
            continue;
        }
        match open(name) {
            Ok(state) => return Ok((name, state)),
            Err(error) => warn!(encoder = name, %error, "Hardware encoder did not open"),
        }
    }
    warn!("No hardware encoder opened; falling back to libx264 (software)");
    open("libx264").map(|state| ("libx264", state))
}

/// The H.265 sibling of an H.264 encoder name, if one exists.
pub fn hevc_variant_of(h264_name: &str) -> Option<&'static str> {
    match h264_name {
        "h264_nvenc" => Some("hevc_nvenc"),
        "h264_amf" => Some("hevc_amf"),
        "h264_qsv" => Some("hevc_qsv"),
        "h264_videotoolbox" => Some("hevc_videotoolbox"),
        "libx264" => Some("libx265"),
        _ => None,
    }
}

pub fn is_hevc_encoder(encoder_name: &str) -> bool {
    encoder_name.starts_with("hevc_") || encoder_name == "libx265"
}

/// True for encoders that cannot be asked for a keyframe on a given frame.
/// FFmpeg 7.1's AMF wrapper never reads `AVFrame::pict_type` (8.0 added that,
/// behind its `forced_idr` option), so AMF keeps encoding P-frames until its
/// own IDR period. Every new session starts with an IDR carrying its
/// parameter sets, so the encode loop reopens AMF to honour a request.
pub fn ignores_forced_keyframes(encoder_name: &str) -> bool {
    matches!(encoder_name, "h264_amf" | "hevc_amf")
}

/// Whether a pending keyframe request needs a session reopen: the encoder
/// ignores per-frame requests, the session has already sent its opening IDR,
/// and the last such reopen is older than the client's own request limit.
fn reopen_for_keyframe(
    encoder_name: &str,
    frames_submitted: u64,
    since_last_reopen: Option<std::time::Duration>,
) -> bool {
    ignores_forced_keyframes(encoder_name)
        && frames_submitted > 0
        && since_last_reopen.is_none_or(|elapsed| {
            elapsed >= crate::transport::session::KEYFRAME_REQUEST_MIN_INTERVAL
        })
}

/// AMF can satisfy a request only on a fresh session's first frame. Keep it
/// pending while a reopen is rate limited or has failed.
fn take_keyframe_request(pending: &mut bool, encoder_name: &str, frames_submitted: u64) -> bool {
    if ignores_forced_keyframes(encoder_name) && frames_submitted > 0 {
        return false;
    }
    std::mem::take(pending)
}

fn codec_display_name_for(encoder_name: &str) -> &str {
    match encoder_name {
        "h264_nvenc" => "H.264 (NVENC)",
        "h264_amf" => "H.264 (AMF)",
        "h264_qsv" => "H.264 (QSV)",
        "h264_videotoolbox" => "H.264 (VideoToolbox)",
        "libx264" => "H.264 (x264)",
        "hevc_nvenc" => "HEVC (NVENC)",
        "hevc_amf" => "HEVC (AMF)",
        "hevc_qsv" => "HEVC (QSV)",
        "hevc_videotoolbox" => "HEVC (VideoToolbox)",
        "libx265" => "HEVC (x265)",
        other => other,
    }
}

/// After every successful encoder open: report the live codec on the wire
/// (HELLO_ACK/heartbeat `StreamConfig.codec`) and in the GUI.
fn note_encoder_opened(shared: &SharedControl, encoder_name: &str) {
    let codec = if is_hevc_encoder(encoder_name) {
        eternal_wire::v2::control::CODEC_HEVC
    } else {
        eternal_wire::v2::control::CODEC_H264
    };
    shared.active_codec.store(codec, Ordering::SeqCst);
    PIPELINE_STATS
        .lock()
        .set_codec_name(codec_display_name_for(encoder_name));
}

fn encoder_options(encoder_name: &str) -> ffmpeg_next::Dictionary<'_> {
    let mut opts = ffmpeg_next::Dictionary::new();
    match encoder_name {
        "h264_nvenc" => {
            opts.set("preset", "p1");
            opts.set("tune", "ll");
            opts.set("profile", "baseline");
            opts.set("zerolatency", "1");
            opts.set("rc", "cbr");
        }
        "h264_amf" => {
            opts.set("usage", "lowlatency");
            opts.set("latency", "1");
            opts.set("quality", "speed");
            opts.set("profile", "constrained_baseline");
            // Level must cover the actual frame complexity or strict decoders (Apple
            // VideoToolbox) reject the stream even though lenient ones (ffmpeg) accept it.
            // 4.1 only covers ~1080p30; 1080p60 needs 4.2. Declare 5.1 for headroom across
            // resolutions — over-declaring a level is harmless, under-declaring is fatal.
            // NVENC works on iPad precisely because it auto-selects an adequate level.
            opts.set("level", "5.1");
            opts.set("coder", "cavlc");
            // No AUD (NAL 9): the iPad decoder strips AUDs before submitting to VideoToolbox,
            // so emitting them only wastes bytes. One FramePacket == one access unit, so the
            // receiver doesn't need AUDs for access-unit boundary detection.
            opts.set("aud", "0");
            // Emit SPS/PPS inline alongside each IDR; the host also prepends cached parameter
            // sets on every IDR in normalize_h264_payload as a belt-and-suspenders measure.
            opts.set("header_spacing", "1");
            opts.set("rc", "cbr");
            // IDR cadence is driven by encoder.set_gop(30) (mapped to AMF's IDR period). The
            // AMF_FORCED_INTRA_PERIOD safety net forces an I-frame every 30 frames in case the
            // wrapper doesn't honour gop in low-latency mode. Verify on-device via the AMF
            // packet logs that forced/periodic keyframes carry NAL 5 + SPS/PPS, not bare I.
        }
        "h264_qsv" => {
            opts.set("preset", "veryfast");
            opts.set("profile", "baseline");
            opts.set("look_ahead", "0");
        }
        "libx264" => {
            opts.set("preset", "ultrafast");
            opts.set("tune", "zerolatency");
            opts.set("profile", "baseline");
        }
        // HEVC siblings. Every keyframe must be decodable by a mid-stream
        // joiner, so ask each encoder to repeat VPS/SPS/PPS in-band; the
        // encode loop also prepends Annex B extradata as a safety net.
        "hevc_nvenc" => {
            opts.set("preset", "p1");
            opts.set("tune", "ll");
            opts.set("zerolatency", "1");
            opts.set("rc", "cbr");
        }
        "hevc_amf" => {
            opts.set("usage", "lowlatency");
            opts.set("latency", "1");
            opts.set("quality", "speed");
            opts.set("rc", "cbr");
            opts.set("aud", "0");
            // AMF HEVC's equivalent of h264_amf's header_spacing.
            opts.set("header_insertion_mode", "idr");
        }
        "hevc_qsv" => {
            opts.set("preset", "veryfast");
        }
        "hevc_videotoolbox" => {
            opts.set("realtime", "1");
        }
        "libx265" => {
            opts.set("preset", "ultrafast");
            opts.set("tune", "zerolatency");
            opts.set("x265-params", "repeat-headers=1:log-level=error");
        }
        _ => {
            opts.set("profile", "baseline");
        }
    }
    opts
}

fn prepare_frame_for_encode(
    frame: &mut ffmpeg_next::frame::Video,
    frame_number: u64,
    encoder_name: &str,
    frames_since_last_idr: &mut u64,
    force_next_idr: bool,
) -> bool {
    let is_amf = is_amf_encoder(encoder_name);
    // Force the next encoded frame to be an IDR whenever the transport flagged a
    // re-handshake. Applies to all encoders so any reconnect recovers within 1 frame.
    let force_hit = force_next_idr;
    // AMF needs an explicit periodic IDR to keep VideoToolbox in sync; other encoders
    // already produce a healthy keyframe cadence on their own.
    let period_hit = is_amf && *frames_since_last_idr >= AMF_FORCED_INTRA_PERIOD;
    let request_intra = period_hit || force_hit;

    frame.set_kind(if request_intra {
        ffmpeg_next::util::picture::Type::I
    } else {
        ffmpeg_next::util::picture::Type::None
    });

    unsafe {
        (*frame.as_mut_ptr()).key_frame = if request_intra { 1 } else { 0 };
    }

    if request_intra {
        *frames_since_last_idr = 0;
        let reason = if force_hit {
            "force_next_idr"
        } else {
            "period"
        };
        if is_amf {
            info!(
                encoder = encoder_name,
                frame = frame_number,
                period = AMF_FORCED_INTRA_PERIOD,
                reason,
                "[AMF] Forced IDR"
            );
        } else {
            info!(
                encoder = encoder_name,
                frame = frame_number,
                reason,
                "[encoder] Forced IDR"
            );
        }
    } else if is_amf {
        *frames_since_last_idr += 1;
    }

    request_intra
}

/// Check if an ffmpeg error is EAGAIN (no output available yet).
fn is_eagain(e: &ffmpeg_next::Error) -> bool {
    matches!(e, ffmpeg_next::Error::Other { errno } if *errno == libc::EAGAIN)
}

// The existing H.264 diagnostics remain unchanged. HEVC uses its own bounded,
// opt-in file so the hardware campaign can validate the actual AMF bitstream.
struct HevcBitstreamCapture {
    file: Option<File>,
    packets: u64,
}

impl HevcBitstreamCapture {
    fn for_encoder(name: &str) -> Option<Self> {
        Self::open_for_encoder(
            name,
            std::env::var("ETERNAL_AMF_DIAG").is_ok_and(|v| v.trim() == "1"),
            diagnostic_dir().join("amf-first-120-packets.hevc"),
        )
    }

    fn open_for_encoder(name: &str, enabled: bool, path: PathBuf) -> Option<Self> {
        if name != "hevc_amf" || !enabled {
            return None;
        }
        let opened = fs::create_dir_all(path.parent().unwrap()).and_then(|()| File::create(&path));
        match opened {
            Ok(file) => {
                info!(path = %path.display(), "Capturing AMF HEVC packets for diagnostics");
                Some(Self {
                    file: Some(file),
                    packets: 0,
                })
            }
            Err(error) => {
                warn!(%error, "Could not create opt-in HEVC diagnostic capture");
                None
            }
        }
    }

    fn observe(&mut self, bytes: &[u8], keyframe: bool) -> std::io::Result<()> {
        if self.packets == 0 && !keyframe {
            return Ok(());
        }
        if let Some(file) = self.file.as_mut() {
            file.write_all(bytes)?;
            self.packets += 1;
            if self.packets == AMF_CAPTURE_PACKET_LIMIT {
                file.flush()?;
                self.file = None;
                info!(
                    packets = self.packets,
                    "AMF HEVC diagnostic capture complete"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod hevc_capture_tests {
    use super::*;

    #[test]
    fn opted_in_hevc_amf_creates_capture_and_other_encoders_do_not() {
        let path = std::env::temp_dir().join(format!("em-hevc-select-{}.bin", std::process::id()));
        for (name, enabled) in [
            ("hevc_amf", false),
            ("h264_amf", true),
            ("hevc_nvenc", true),
        ] {
            assert!(HevcBitstreamCapture::open_for_encoder(name, enabled, path.clone()).is_none());
            assert!(!path.exists());
        }
        let mut capture = HevcBitstreamCapture::open_for_encoder("hevc_amf", true, path.clone())
            .expect("opted-in AMD HEVC must create its diagnostic capture");
        capture.observe(&[1, 2, 3], true).unwrap();
        drop(capture);
        assert_eq!(fs::read(&path).unwrap(), [1, 2, 3]);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn capture_starts_at_a_keyframe_and_closes_at_the_packet_limit() {
        let path = std::env::temp_dir().join(format!("em-hevc-capture-{}.bin", std::process::id()));
        let mut capture = HevcBitstreamCapture {
            file: Some(File::create(&path).unwrap()),
            packets: 0,
        };
        capture.observe(&[99], false).unwrap();
        capture.observe(&[1], true).unwrap();
        for _ in 1..AMF_CAPTURE_PACKET_LIMIT {
            capture.observe(&[2], false).unwrap();
        }
        capture.observe(&[3], true).unwrap();
        assert!(capture.file.is_none());
        let bytes = fs::read(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(bytes.len(), AMF_CAPTURE_PACKET_LIMIT as usize);
        assert_eq!(bytes[0], 1);
        assert!(bytes[1..].iter().all(|&byte| byte == 2));
    }
}

struct AmfBitstreamDiagnostics {
    capture_path: PathBuf,
    capture_file: Option<File>,
    total_packets: u64,
    captured_packets: u64,
    seen_random_access: bool,
    seen_idr: bool,
    warned_missing_random_access: bool,
    validation_complete: bool,
}

impl AmfBitstreamDiagnostics {
    fn new(encoder_name: &str) -> Option<Self> {
        if !is_amf_encoder(encoder_name) {
            return None;
        }
        // Opt-in only: the capture writes every packet to disk and used to
        // stall the encode thread for seconds on the ffmpeg.exe validation.
        if !std::env::var("ETERNAL_AMF_DIAG").is_ok_and(|v| v.trim() == "1") {
            return None;
        }

        let capture_path = diagnostic_capture_path();
        if let Some(parent) = capture_path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                warn!(
                    encoder = encoder_name,
                    path = %capture_path.display(),
                    error = %error,
                    "Failed to create AMF diagnostic directory"
                );
            }
        }

        let capture_file = match File::create(&capture_path) {
            Ok(file) => {
                info!(
                    encoder = encoder_name,
                    path = %capture_path.display(),
                    packet_limit = AMF_CAPTURE_PACKET_LIMIT,
                    "Capturing normalized AMF packets for diagnostics"
                );
                Some(file)
            }
            Err(error) => {
                warn!(
                    encoder = encoder_name,
                    path = %capture_path.display(),
                    error = %error,
                    "Failed to create AMF diagnostic capture file"
                );
                None
            }
        };

        Some(Self {
            capture_path,
            capture_file,
            total_packets: 0,
            captured_packets: 0,
            seen_random_access: false,
            seen_idr: false,
            warned_missing_random_access: false,
            validation_complete: false,
        })
    }

    fn observe_packet(
        &mut self,
        normalized_packet: &[u8],
        sequence: u64,
        packet_is_key: bool,
        is_keyframe: bool,
    ) {
        self.total_packets += 1;
        let contains_idr = contains_nal_type(normalized_packet, 5);
        if is_keyframe && !self.seen_random_access {
            self.seen_random_access = true;
            info!(
                sequence,
                packet_index = self.total_packets,
                packet_is_key,
                contains_idr,
                "Observed first AMF random-access access unit after normalization"
            );
        }
        if contains_idr && !self.seen_idr {
            self.seen_idr = true;
            info!(
                sequence,
                packet_index = self.total_packets,
                packet_is_key,
                is_keyframe,
                "Observed first AMF IDR after normalization"
            );
        }

        if !self.seen_random_access
            && !self.warned_missing_random_access
            && self.total_packets >= AMF_IDR_WARNING_PACKET
        {
            warn!(
                packet_index = self.total_packets,
                sequence,
                capture_path = %self.capture_path.display(),
                "AMF has not emitted any random-access access unit by packet 60; decoder startup may still fail"
            );
            self.warned_missing_random_access = true;
        }

        if self.captured_packets < AMF_CAPTURE_PACKET_LIMIT {
            if let Some(file) = self.capture_file.as_mut() {
                if let Err(error) = file.write_all(normalized_packet) {
                    warn!(
                        path = %self.capture_path.display(),
                        error = %error,
                        "Failed to append normalized AMF packet to diagnostic capture"
                    );
                    self.capture_file = None;
                }
            }

            self.captured_packets += 1;
            if self.captured_packets == AMF_CAPTURE_PACKET_LIMIT {
                if let Some(file) = self.capture_file.as_mut() {
                    if let Err(error) = file.flush() {
                        warn!(
                            path = %self.capture_path.display(),
                            error = %error,
                            "Failed to flush AMF diagnostic capture file"
                        );
                    }
                }
                self.capture_file = None;
                // Never on the encode thread: the ffmpeg.exe round trip takes
                // seconds and used to freeze the stream mid-session.
                let capture_path = self.capture_path.clone();
                std::thread::spawn(move || validate_capture_with_ffmpeg(&capture_path));
                self.validation_complete = true;
            }
        }
    }
}

fn validate_capture_with_ffmpeg(capture_path: &std::path::Path) {
    {
        let Some(ffmpeg_path) = find_ffmpeg_exe() else {
            warn!(
                path = %capture_path.display(),
                "AMF diagnostic capture completed but ffmpeg.exe was not found for software decode validation"
            );
            return;
        };

        match Command::new(&ffmpeg_path)
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-i",
                capture_path.to_string_lossy().as_ref(),
                "-f",
                "null",
                "-",
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                info!(
                    capture = %capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    "Local FFmpeg software decode of captured AMF bitstream succeeded"
                );
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(
                    capture = %capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    status = ?output.status.code(),
                    stderr = %stderr.trim(),
                    "Local FFmpeg software decode of captured AMF bitstream failed"
                );
            }
            Err(error) => {
                warn!(
                    capture = %capture_path.display(),
                    ffmpeg = %ffmpeg_path.display(),
                    error = %error,
                    "Failed to launch ffmpeg.exe for AMF software decode validation"
                );
            }
        }
    }
}

fn encoder_extradata(encoder: &ffmpeg_next::codec::encoder::video::Encoder) -> Option<Vec<u8>> {
    unsafe {
        let context = encoder.as_ptr();
        let extradata = (*context).extradata;
        let extradata_size = (*context).extradata_size;
        if extradata.is_null() || extradata_size <= 0 {
            None
        } else {
            Some(slice::from_raw_parts(extradata, extradata_size as usize).to_vec())
        }
    }
}

fn configure_encoder_flags(
    encoder: &mut ffmpeg_next::codec::encoder::video::Video,
    encoder_name: &str,
) {
    if !is_amf_encoder(encoder_name) {
        return;
    }

    let mut flags = codec_flags(encoder);
    flags.remove(ffmpeg_next::codec::Flags::GLOBAL_HEADER);
    flags.insert(ffmpeg_next::codec::Flags::CLOSED_GOP);
    encoder.set_flags(flags);

    info!(
        encoder = encoder_name,
        global_header = flags.contains(ffmpeg_next::codec::Flags::GLOBAL_HEADER),
        closed_gop = flags.contains(ffmpeg_next::codec::Flags::CLOSED_GOP),
        "Applied AMD-specific encoder flags"
    );
}

fn log_encoder_configuration(
    encoder_name: &str,
    encoder: &ffmpeg_next::codec::encoder::video::Encoder,
) {
    let flags = codec_flags(encoder);
    let extradata = encoder_extradata(encoder);
    let extradata_bytes = extradata.as_ref().map_or(0, Vec::len);
    let extradata_format = extradata
        .as_deref()
        .map(describe_extradata_format)
        .unwrap_or("None");
    let (sps_bytes, pps_bytes) = extradata
        .as_deref()
        .and_then(parameter_sets_from_extradata)
        .map(|(sps, pps)| (sps.len(), pps.len()))
        .unwrap_or((0, 0));
    let extradata_prefix = extradata
        .as_deref()
        .map(|bytes| hex_prefix(bytes, 16))
        .unwrap_or_else(|| "none".to_string());

    info!(
        encoder = encoder_name,
        global_header = flags.contains(ffmpeg_next::codec::Flags::GLOBAL_HEADER),
        closed_gop = flags.contains(ffmpeg_next::codec::Flags::CLOSED_GOP),
        extradata_bytes,
        extradata_format,
        sps_bytes,
        pps_bytes,
        extradata_prefix = %extradata_prefix,
        "Encoder H.264 configuration"
    );
}

fn codec_flags<T: AsRef<ffmpeg_next::codec::Context>>(context: &T) -> ffmpeg_next::codec::Flags {
    unsafe {
        let raw_flags = (*context.as_ref().as_ptr()).flags;
        ffmpeg_next::codec::Flags::from_bits_truncate(raw_flags as u32)
    }
}

fn diagnostic_capture_path() -> PathBuf {
    diagnostic_dir().join("amf-first-120-packets.h264")
}

fn diagnostic_dir() -> PathBuf {
    // Write under %APPDATA% so the capture works even when the app is installed read-only under
    // Program Files (the exe directory is not writable by the non-elevated host there).
    if let Some(dir) = crate::settings::app_data_dir() {
        return dir.join("diagnostics");
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("diagnostics")))
        .unwrap_or_else(|| PathBuf::from("diagnostics"))
}

fn find_ffmpeg_exe() -> Option<PathBuf> {
    // ffmpeg.exe is bundled next to the host exe; reading it is fine even under Program Files.
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ffmpeg.exe")))
        .filter(|path| path.is_file());
    if bundled.is_some() {
        return bundled;
    }

    std::env::var_os("FFMPEG_DIR")
        .map(PathBuf::from)
        .map(|dir| dir.join("bin").join("ffmpeg.exe"))
        .filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delayed_packet_keeps_the_capture_time_of_its_own_frame() {
        let start = Instant::now();
        let at = |ms| start + std::time::Duration::from_millis(ms);
        let mut in_flight = InFlightFrames::default();
        // NVENC holds the startup keyframe until the next frame arrives,
        // which can be many seconds later when a viewer connects.
        in_flight.submitted(0, at(0), 1);
        in_flight.submitted(16_666, at(9_000), 2);
        assert_eq!(in_flight.take(Some(0)), Some((at(0), 1)));
        in_flight.submitted(33_333, at(9_016), 3);
        in_flight.submitted(50_000, at(9_033), 4);
        // A frame the encoder dropped is skipped, not reused.
        assert_eq!(in_flight.take(Some(33_333)), Some((at(9_016), 3)));
        assert_eq!(in_flight.take(Some(50_000)), Some((at(9_033), 4)));
        assert_eq!(in_flight.take(Some(66_666)), None);
        assert_eq!(in_flight.take(None), None);
    }

    #[test]
    fn a_reopened_encoder_labels_packets_with_their_own_capture_time() {
        ffmpeg_next::init().unwrap();
        let start = Instant::now();
        // A fixed cadence, like the synthetic source, so both sessions use
        // the same PTS values.
        let at = |n: u64| start + std::time::Duration::from_micros(n * 1_000_000 / 60);
        let open =
            || EncoderState::new("libx264", 640, 360, 8_000_000, 60, EncoderInput::Yuv420).unwrap();
        // Sends frames through the encode loop's calls. With `hold_last`,
        // the last frame's packet is never collected, as when NVENC still
        // holds a viewer's last frame at a reopen.
        let run = |state: &mut EncoderState, frames: std::ops::Range<u64>, hold_last: bool| {
            let mut labels = Vec::new();
            for n in frames.clone() {
                let pts = state.next_pts(at(n), n);
                state.frame.set_pts(Some(pts));
                state.send_frame(pts, at(n), n).unwrap();
                if hold_last && n + 1 == frames.end {
                    break;
                }
                while state.encoder.receive_packet(&mut state.packet).is_ok() {
                    labels.push(state.packet_origin((at(n), n)));
                }
            }
            labels
        };
        // A viewer watches and leaves with frame 5 inside the encoder, the
        // host idles for 8 s, and a new viewer's arrival reopens the encoder.
        let mut first = open();
        let labels = run(&mut first, 0..6, true);
        assert_eq!(labels, (0..5).map(|n| (at(n), n)).collect::<Vec<_>>());
        drop(first);
        let mut second = open();
        let labels = run(&mut second, 480..490, false);
        assert_eq!(labels, (480..490).map(|n| (at(n), n)).collect::<Vec<_>>());
    }

    #[test]
    fn startup_tries_other_hardware_before_software() {
        let mut attempts = Vec::new();
        let (selected, ()) = open_remaining_encoders("h264_nvenc", |name| {
            attempts.push(name.to_string());
            match name {
                "h264_qsv" => Ok(()),
                _ => Err("device unavailable".into()),
            }
        })
        .unwrap();
        assert_eq!(attempts, ["h264_amf", "h264_qsv"]);
        assert_eq!(selected, "h264_qsv");
    }

    #[test]
    fn startup_uses_software_only_after_all_other_hardware_fails() {
        for failed in ["h264_nvenc", "h264_amf", "h264_qsv"] {
            let mut attempts = Vec::new();
            let (selected, ()) = open_remaining_encoders(failed, |name| {
                attempts.push(name.to_string());
                if name == "libx264" {
                    Ok(())
                } else {
                    Err("device unavailable".into())
                }
            })
            .unwrap();
            assert_eq!(selected, "libx264");
            assert_eq!(attempts.len(), 3);
            assert!(!attempts.iter().any(|name| name == failed));
            assert_eq!(attempts.last().unwrap(), "libx264");
        }
        assert!(open_remaining_encoders::<()>("h264_amf", |_| {
            Err("no encoder can open".into())
        })
        .is_err());
    }

    #[test]
    fn non_windows_backends_keep_their_software_fallback() {
        let mut attempts = Vec::new();
        let (selected, ()) = open_remaining_encoders("h264_videotoolbox", |name| {
            attempts.push(name.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(selected, "libx264");
        assert_eq!(attempts, ["libx264"]);
    }

    #[test]
    fn software_bitrate_changes_preserve_decoding_and_reduce_packet_size() {
        ffmpeg_next::init().unwrap();
        let mut state =
            EncoderState::new("libx264", 640, 360, 8_000_000, 60, EncoderInput::Yuv420).unwrap();
        let context = unsafe { state.encoder.as_ptr() };
        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264).unwrap();
        let mut decoder = ffmpeg_next::codec::Context::new_with_codec(codec)
            .decoder()
            .video()
            .unwrap();
        let mut decoded = ffmpeg_next::frame::Video::empty();
        let mut decoded_frames = 0;
        let mut sizes = [0usize; 3];
        let mut random = 1u32;
        for number in 0..360 {
            if number == 120 || number == 240 {
                let bitrate = if number == 120 { 1_000_000 } else { 8_000_000 };
                assert!(state.update_bitrate_without_reopening("libx264", bitrate, 60));
            }
            // Changing noise forces rate control to work at both targets.
            for value in state.frame.data_mut(0) {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *value = (random >> 24) as u8;
            }
            state.frame.data_mut(1).fill(128);
            state.frame.data_mut(2).fill(128);
            state.frame.set_pts(Some(number as i64 * 1_000_000 / 60));
            state.encoder.send_frame(&state.frame).unwrap();
            loop {
                match state.encoder.receive_packet(&mut state.packet) {
                    Ok(()) => {
                        if number % 120 >= 60 {
                            sizes[number / 120] += state.packet.size();
                        }
                        decoder.send_packet(&state.packet).unwrap();
                        loop {
                            match decoder.receive_frame(&mut decoded) {
                                Ok(()) => {
                                    assert_eq!(
                                        decoded.pts(),
                                        Some(decoded_frames * 1_000_000 / 60)
                                    );
                                    assert_eq!((decoded.width(), decoded.height()), (640, 360));
                                    decoded_frames += 1;
                                }
                                Err(error) if is_eagain(&error) => break,
                                Err(error) => panic!("decode failed: {error}"),
                            }
                        }
                    }
                    Err(error) if is_eagain(&error) => break,
                    Err(error) => panic!("encode failed: {error}"),
                }
            }
        }
        assert_eq!(decoded_frames, 360);
        assert_eq!(unsafe { state.encoder.as_ptr() }, context);
        assert!(
            sizes[1] * 2 < sizes[0],
            "lower bitrate was not applied: {sizes:?}"
        );
        assert!(
            sizes[2] > sizes[1] * 2,
            "higher bitrate was not applied: {sizes:?}"
        );
        // FPS and codec changes must still use the existing reopen path.
        assert!(!state.update_bitrate_without_reopening("libx264", 4_000_000, 120));
        assert!(!state.update_bitrate_without_reopening("libx265", 4_000_000, 60));
        for name in ["h264_nvenc", "h264_amf", "h264_qsv", "libx265"] {
            state.opened_encoder = name.to_string();
            assert!(!state.update_bitrate_without_reopening(name, 4_000_000, 60));
        }
    }

    #[test]
    fn hevc_variants_cover_every_supported_encoder() {
        assert_eq!(hevc_variant_of("h264_nvenc"), Some("hevc_nvenc"));
        assert_eq!(hevc_variant_of("h264_amf"), Some("hevc_amf"));
        assert_eq!(hevc_variant_of("h264_qsv"), Some("hevc_qsv"));
        assert_eq!(
            hevc_variant_of("h264_videotoolbox"),
            Some("hevc_videotoolbox")
        );
        assert_eq!(hevc_variant_of("libx264"), Some("libx265"));
        assert_eq!(hevc_variant_of("mystery_codec"), None);
    }

    #[test]
    fn amf_keyframe_requests_reopen_the_session_at_most_twice_a_second() {
        let long_ago = Some(std::time::Duration::from_secs(5));
        let just_now = Some(std::time::Duration::from_millis(100));
        for name in ["h264_amf", "hevc_amf"] {
            assert!(reopen_for_keyframe(name, 1, None), "{name}");
            assert!(reopen_for_keyframe(name, 300, long_ago), "{name}");
            // A fresh session's first frame is already an IDR.
            assert!(!reopen_for_keyframe(name, 0, None), "{name}");
            assert!(!reopen_for_keyframe(name, 300, just_now), "{name}");
        }
        // These encoders turn AVFrame::pict_type into a keyframe themselves.
        for name in ["h264_nvenc", "hevc_nvenc", "h264_qsv", "libx264", "libx265"] {
            assert!(!reopen_for_keyframe(name, 300, None), "{name}");
        }
    }

    #[test]
    fn amf_keeps_keyframe_requests_pending_until_a_fresh_session() {
        for name in ["h264_amf", "hevc_amf"] {
            let mut pending = true;
            for millis in [0, 100, 499] {
                assert!(!reopen_for_keyframe(
                    name,
                    10,
                    Some(std::time::Duration::from_millis(millis))
                ));
                assert!(!take_keyframe_request(&mut pending, name, 10));
                assert!(pending, "{name} lost the deferred request");
            }
            assert!(reopen_for_keyframe(
                name,
                10,
                Some(std::time::Duration::from_millis(500))
            ));
            // A failed reopen leaves the old session in place.
            assert!(!take_keyframe_request(&mut pending, name, 10));
            assert!(pending);
            // A successful reopen starts its frame counter at zero.
            assert!(take_keyframe_request(&mut pending, name, 0));
            assert!(!pending);
            assert!(!take_keyframe_request(&mut pending, name, 0));
        }
        let mut pending = true;
        assert!(take_keyframe_request(&mut pending, "libx264", 10));
        assert!(!pending);
    }

    #[test]
    fn amf_options_keep_the_settings_the_ipad_decoder_needs() {
        let h264 = encoder_options("h264_amf");
        for (key, value) in [
            ("usage", "lowlatency"),
            ("profile", "constrained_baseline"),
            ("level", "5.1"),
            ("coder", "cavlc"),
            ("aud", "0"),
            ("header_spacing", "1"),
            ("rc", "cbr"),
        ] {
            assert_eq!(h264.get(key), Some(value), "h264_amf {key}");
        }
        let hevc = encoder_options("hevc_amf");
        for (key, value) in [
            ("usage", "lowlatency"),
            ("aud", "0"),
            ("header_insertion_mode", "idr"),
            ("rc", "cbr"),
        ] {
            assert_eq!(hevc.get(key), Some(value), "hevc_amf {key}");
        }
    }

    #[test]
    fn hevc_detection_matches_the_variant_table() {
        for h264 in ["h264_nvenc", "h264_amf", "h264_qsv", "libx264"] {
            assert!(!is_hevc_encoder(h264));
            let hevc = hevc_variant_of(h264).unwrap();
            assert!(is_hevc_encoder(hevc), "{hevc} must register as HEVC");
        }
    }
}
