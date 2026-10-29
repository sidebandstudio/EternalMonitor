use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::slice;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

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
    // Suppresses reopen retry storms: the encoder, bitrate and frame rate
    // of a reopen that failed are not tried again until one of them changes,
    // or the codec negotiation does (see `forget_failed_reopen`).
    let mut failed_reopen: Option<(String, u32, u32)> = None;
    let mut negotiated: Option<(bool, Option<u32>)> = None;
    // HEVC gating memory: warn only once when no HEVC sibling exists, and
    // don't retry an HEVC open that failed until the preference or the
    // session changes (hevc_wanted going false resets it).
    let mut hevc_unavailable_warned = false;
    let mut hevc_open_failed = false;
    let mut stall_recovery = StallRecovery::default();
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

    // While an encoder opens under its time limit, the limit decides, not
    // the watchdog.
    let mut opening = || {
        if crate::supervisor::generation_is_current(generation) {
            crate::capture::heartbeat(&shared.hb_encode_frame_ms);
        }
    };

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
        let (client_hevc, session_id) = {
            let session = shared.session.lock();
            (session.client_supports_hevc(), session.session_id())
        };
        let hevc_wanted = shared.hevc_enabled.load(Ordering::SeqCst) && client_hevc;
        if !hevc_wanted {
            hevc_open_failed = false;
        }
        let preferred: String = if hevc_wanted && !hevc_open_failed {
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
        // No open given up on still holds FFmpeg's lock. Until then, an open
        // in that lane would only be refused.
        let lane_clear = crate::stall::abandoned(crate::stall::Call::EncoderOpen) == 0;
        let desired_encoder =
            stall_recovery.encoder_for_frame(&preferred, Instant::now(), lane_clear);
        forget_failed_reopen(
            &mut failed_reopen,
            &mut negotiated,
            (hevc_wanted, session_id),
            &desired_encoder,
        );

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
                Opening::First,
                &mut opening,
            ) {
                Ok(state) => {
                    encoder_state = Some(state);
                    if stall_recovery.opened(&desired_encoder, &preferred)
                        && desired_encoder != "libx265"
                    {
                        PIPELINE_STATS.lock().set_software_fallback(false);
                    }
                    note_encoder_opened(&shared, &desired_encoder);
                }
                Err(e) if is_hevc_encoder(&desired_encoder) => {
                    warn!(
                        encoder = %desired_encoder,
                        error = %e,
                        "HEVC encoder failed to open — staying on H.264"
                    );
                    // After a stall HEVC is tried again later.
                    let stalled = is_open_stall(e.as_ref());
                    if !stall_recovery.failed(stalled, &preferred, &encoder_name, Instant::now()) {
                        hevc_open_failed = true;
                    }
                    continue;
                }
                Err(e) if desired_encoder != "libx264" => {
                    warn!(
                        encoder = %desired_encoder,
                        error = %e,
                        "Preferred encoder failed to open; trying remaining encoders"
                    );
                    let stalled = is_open_stall(e.as_ref());
                    let (name, state) = open_remaining_encoders(&desired_encoder, |name| {
                        EncoderState::new(
                            name,
                            raw_frame.width,
                            raw_frame.height,
                            desired_bitrate,
                            target_fps,
                            input_mode,
                            Opening::First,
                            &mut opening,
                        )
                    })?;
                    // After a stall the preferred encoder stays preferred,
                    // to be tried again; a real error leaves it for good.
                    if !stall_recovery.failed(stalled, &preferred, name, Instant::now()) {
                        encoder_name = name.to_string();
                    }
                    stall_recovery.opened(name, &preferred);
                    encoder_state = Some(state);
                    PIPELINE_STATS
                        .lock()
                        .set_software_fallback(name == "libx264");
                    note_encoder_opened(&shared, name);
                }
                Err(e) => return Err(e),
            }
        } else if size_changed
            || encoder_state.as_ref().is_some_and(|state| {
                reopen_needed(
                    (
                        &state.opened_encoder,
                        state.opened_bitrate,
                        state.opened_fps,
                    ),
                    (&desired_encoder, desired_bitrate, target_fps),
                    keyframe_reopen,
                    failed_reopen.as_ref(),
                    lane_clear,
                )
            })
        {
            // Bitrate changed (ABR rung or the user's slider), the
            // negotiated codec changed (HEVC toggle / client capability), the
            // capture size changed (display mode change), or AMF owes a
            // keyframe: hardware encoders ignore parameter pokes on an open
            // context, so each means a session reopen (~50-200ms hiccup, no
            // transport teardown, same stream epoch) followed by a forced IDR.
            // So does a try of the preferred encoder after a stalled open.
            if keyframe_reopen {
                last_keyframe_reopen = Some(Instant::now());
            }
            let (previous, current_encoder) = encoder_state
                .as_ref()
                .map(|s| (s.opened_bitrate, s.opened_encoder.clone()))
                .unwrap_or_default();
            stall_recovery.starting(&desired_encoder);
            match EncoderState::new(
                &desired_encoder,
                raw_frame.width,
                raw_frame.height,
                desired_bitrate,
                target_fps,
                input_mode,
                // The old session cannot take a frame of the new size.
                if size_changed {
                    Opening::First
                } else {
                    Opening::Reopen
                },
                &mut opening,
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
                    // Back from libx264 to hardware (libx265 only replaces a
                    // libx264 that was already preferred).
                    if stall_recovery.opened(&desired_encoder, &preferred)
                        && desired_encoder != "libx265"
                    {
                        PIPELINE_STATS.lock().set_software_fallback(false);
                    }
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
                Err(e) => {
                    let stalled = is_open_stall(e.as_ref());
                    if stalled {
                        warn!(
                            error = %e,
                            encoder = %desired_encoder,
                            "Encoder reopen stalled — keeping the current session"
                        );
                    } else if is_hevc_encoder(&desired_encoder) {
                        warn!(
                            error = %e,
                            encoder = %desired_encoder,
                            "HEVC reopen failed — keeping the current session"
                        );
                    } else {
                        warn!(
                            error = %e,
                            bitrate = desired_bitrate,
                            "Encoder reopen failed — keeping the current session"
                        );
                    }
                    // A stall is tried again once the lane is clear, while
                    // tries are left.
                    if !stall_recovery.failed(stalled, &preferred, &current_encoder, Instant::now())
                    {
                        give_up_reopen(
                            &desired_encoder,
                            &current_encoder,
                            stall_recovery.owed.is_some(),
                            &mut encoder_name,
                            &mut hevc_open_failed,
                        );
                        failed_reopen =
                            Some((desired_encoder.clone(), desired_bitrate, target_fps));
                    }
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

    #[allow(clippy::too_many_arguments)]
    fn new(
        encoder_name: &str,
        width: u32,
        height: u32,
        bitrate_bps: u32,
        target_fps: u32,
        input_mode: EncoderInput,
        opening: Opening,
        progress: &mut dyn FnMut(),
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

        let encoder = match open_encoder(encoder, encoder_name, opening, progress) {
            Ok(Ok(encoder)) => encoder,
            Ok(Err(error)) => {
                let code: i32 = error.into();
                return Err(format!("{encoder_name} open failed: FFmpeg code={code} error={error}; {width}x{height} input={pixel_format:?} bitrate={bitrate_bps} fps={target_fps}").into());
            }
            Err(stall) => {
                return Err(Box::new(OpenStalled(format!("{encoder_name} open failed: {stall}; {width}x{height} input={pixel_format:?} bitrate={bitrate_bps} fps={target_fps}"))));
            }
        };
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
    /// with output delay (a hardware encoder can hold frames) returns an
    /// earlier frame than the one just submitted.
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

type OpenResult = Result<
    Result<ffmpeg_next::codec::encoder::video::Encoder, ffmpeg_next::Error>,
    crate::stall::Stall,
>;

/// What streams while an encoder session opens, which sets its time limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// Nothing: the pipeline's first open and its fallbacks, or a reopen at
    /// a new capture size, which the old session cannot encode.
    First,
    /// The live session, which goes on if the reopen is given up on. The
    /// picture freezes until then, so the limit is shorter.
    Reopen,
}

/// Open `encoder` with its options. An encoder that opens under FFmpeg's
/// process-wide lock opens under its time limit (see [`crate::stall`]); one
/// that returns after it is closed on the call's thread.
fn open_encoder(
    encoder: ffmpeg_next::codec::encoder::video::Video,
    encoder_name: &str,
    opening: Opening,
    progress: &mut dyn FnMut(),
) -> OpenResult {
    open_encoder_on(
        crate::stall::lanes(),
        encoder,
        encoder_name,
        opening,
        progress,
    )
}

fn open_encoder_on(
    lanes: &'static crate::stall::Lanes,
    encoder: ffmpeg_next::codec::encoder::video::Video,
    encoder_name: &str,
    opening: Opening,
    progress: &mut dyn FnMut(),
) -> OpenResult {
    if !takes_codec_lock(encoder_name) {
        return Ok(encoder.open_with(encoder_options(encoder_name)));
    }
    let name = encoder_name.to_string();
    crate::stall::run_on(
        lanes,
        crate::stall::Call::EncoderOpen,
        open_limits(encoder_name, opening),
        move || encoder.open_with(encoder_options(&name)),
        progress,
    )
}

fn open_limits(encoder_name: &str, opening: Opening) -> crate::stall::Limits {
    match opening {
        Opening::First => crate::stall::encoder_open_limits(encoder_name),
        Opening::Reopen => crate::stall::encoder_reopen_limits(encoder_name),
    }
}

/// Encoders FFmpeg 7.1 opens under its one process-wide lock
/// (FF_CODEC_CAP_NOT_INIT_THREADSAFE): the NVENC, AMF and QSV hardware
/// encoders, which wait on a graphics driver, and libx265. While one of them
/// is stuck holding the lock, any other would block behind it, so they share
/// one time-limited lane. libx264 and VideoToolbox do not take the lock.
fn takes_codec_lock(encoder_name: &str) -> bool {
    encoder_name == "libx265"
        || ["_nvenc", "_amf", "_qsv"]
            .iter()
            .any(|suffix| encoder_name.ends_with(suffix))
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

/// An encoder open that its time limit gave up on, or refused while an
/// earlier one was stuck. The driver did not answer in time, which says
/// nothing about whether the encoder works.
#[derive(Debug)]
struct OpenStalled(String);

impl std::fmt::Display for OpenStalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OpenStalled {}

fn is_open_stall(error: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    error.downcast_ref::<OpenStalled>().is_some()
}

/// How long after a stalled open each new try of the preferred encoder
/// waits, at the earliest: at most three tries a pipeline generation.
const STALL_RETRY_WAITS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(8),
    Duration::from_secs(30),
];

/// The preferred encoder, owed after an open of it stalled. A stall is the
/// driver's, not the encoder's, so it must not cost a PC its hardware
/// encoder or HEVC for the rest of the pipeline: the stream goes on with
/// what opened instead (or the session already open), and once no stuck
/// open holds the lane (see [`crate::stall`]) the preferred encoder is tried
/// again through the reopen path, which keeps the current session if it
/// fails. A real open error keeps the fallback, as before.
#[derive(Default)]
struct StallRecovery {
    /// Set while the preferred encoder is owed.
    owed: Option<Owed>,
    /// A reopen in the lane started while one is owed, and has not
    /// returned. If it stalls, it counts as a try.
    trying: bool,
    /// Tries that stalled this generation.
    tries: usize,
}

struct Owed {
    /// The encoder preferred when the open stalled.
    preferred: String,
    /// The encoder streaming instead.
    using: String,
    /// When `preferred` may be tried again, once the lane is clear.
    next_try: Instant,
}

impl StallRecovery {
    /// The encoder this frame runs on: `preferred`, unless a stall left the
    /// stream on another one and the next try of `preferred` is not due yet.
    /// It is due once its wait is over and `lane_clear` (no stuck open
    /// left). A new preference (HEVC off, an H.264-only client) is not held.
    fn encoder_for_frame(&mut self, preferred: &str, now: Instant, lane_clear: bool) -> String {
        let Some(owed) = self.owed.as_ref() else {
            return preferred.to_string();
        };
        if owed.preferred != preferred {
            // Already there: nothing is owed any more.
            if owed.using == preferred {
                self.owed = None;
            }
            return preferred.to_string();
        }
        if now >= owed.next_try && lane_clear {
            return preferred.to_string();
        }
        owed.using.clone()
    }

    /// A reopen of `encoder` starts. While an encoder is owed, one in the
    /// lane counts as a try if it stalls, whether it is the try or another
    /// reopen (a bitrate change on the session streaming meanwhile), so the
    /// lane sees at most three more stalls a generation.
    fn starting(&mut self, encoder: &str) {
        let Some(owed) = self.owed.as_ref() else {
            return;
        };
        if !takes_codec_lock(encoder) {
            return;
        }
        self.trying = true;
        if encoder == owed.preferred && encoder != owed.using {
            info!(
                preferred = encoder,
                using = %owed.using,
                attempt = self.tries + 1,
                "Trying the preferred encoder again after a stalled open"
            );
        }
    }

    /// `encoder` opened, with `preferred` this frame's preference. True when
    /// that brought the stream back to the preferred encoder.
    fn opened(&mut self, encoder: &str, preferred: &str) -> bool {
        self.trying = false;
        let Some(owed) = self.owed.as_mut() else {
            return false;
        };
        if encoder != preferred {
            owed.using = encoder.to_string();
            return false;
        }
        info!(
            encoder,
            attempts = self.tries,
            "Back on the preferred encoder after a stalled open"
        );
        self.owed = None;
        true
    }

    /// An open failed while `preferred` was this frame's preference, and
    /// `using` streams meanwhile. True when `preferred` will be tried again:
    /// after a stall, while tries are left. A real error is not tried again,
    /// and one in a try ends the recovery, as does a stall in the third.
    /// Then nothing is owed any more, and the caller records the failure as
    /// for any other.
    fn failed(&mut self, stalled: bool, preferred: &str, using: &str, now: Instant) -> bool {
        let tried = std::mem::take(&mut self.trying);
        if !stalled {
            if tried {
                warn!(
                    using,
                    attempts = self.tries,
                    "The preferred encoder failed to open after a stall; staying on the fallback for this pipeline"
                );
                self.end();
            }
            return false;
        }
        self.tries += usize::from(tried);
        let Some(wait) = STALL_RETRY_WAITS.get(self.tries) else {
            if self.owed.is_some() {
                warn!(
                    using,
                    attempts = self.tries,
                    "An encoder open stalled again; staying on the fallback for this pipeline"
                );
            }
            self.end();
            return false;
        };
        warn!(
            using,
            retry_in_ms = wait.as_millis() as u64,
            attempts = self.tries,
            "An encoder open stalled; streaming on the fallback until the preferred encoder can be tried again"
        );
        self.owed = Some(Owed {
            preferred: preferred.to_string(),
            using: using.to_string(),
            next_try: now + *wait,
        });
        true
    }

    /// No more tries this generation.
    fn end(&mut self) {
        self.owed = None;
        self.tries = STALL_RETRY_WAITS.len();
    }
}

/// Whether the open session reopens for this frame. `opened` and `wanted`
/// are encoder, bitrate and frame rate. A reopen that failed is not tried
/// again until one of them changes; an AMF keyframe reopen has its own rate
/// limit. Until a stuck open leaves the lane (`lane_clear`), a reopen in it
/// would only be refused, so it waits. That is all it waits for: a bitrate,
/// frame rate or keyframe reopen goes ahead as soon as the lane is clear,
/// whenever the next try of an owed encoder is due.
fn reopen_needed(
    opened: (&str, u32, u32),
    wanted: (&str, u32, u32),
    keyframe_reopen: bool,
    failed_reopen: Option<&(String, u32, u32)>,
    lane_clear: bool,
) -> bool {
    if !lane_clear && takes_codec_lock(wanted.0) {
        return false;
    }
    keyframe_reopen
        || (opened != wanted
            && failed_reopen
                .is_none_or(|(encoder, bitrate, fps)| (encoder.as_str(), *bitrate, *fps) != wanted))
}

/// Forgets a failed reopen once what chose it changes: the codec negotiation
/// (`negotiated` is whether HEVC is wanted, and the client's session; a
/// toggle, or a new client, negotiates again) or the encoder this frame
/// wants (`desired`), as an encoder change always went ahead. Only a
/// bitrate or frame rate that failed stays suppressed for the same choice.
fn forget_failed_reopen(
    failed_reopen: &mut Option<(String, u32, u32)>,
    last_negotiated: &mut Option<(bool, Option<u32>)>,
    negotiated: (bool, Option<u32>),
    desired: &str,
) {
    let renegotiated = last_negotiated
        .replace(negotiated)
        .is_some_and(|last| last != negotiated);
    if renegotiated
        || failed_reopen
            .as_ref()
            .is_some_and(|(encoder, _, _)| encoder != desired)
    {
        *failed_reopen = None;
    }
}

/// A reopen to `desired` failed and is not tried again; `current` streams
/// on. A failed HEVC open turns HEVC off until the preference or the client
/// changes. Once nothing is owed (`recovering` is false), an H.264 session
/// left streaming becomes the pipeline's H.264 encoder, as after a real
/// error in the first open: that changes it only when a stall recovery
/// ended on a fallback.
fn give_up_reopen(
    desired: &str,
    current: &str,
    recovering: bool,
    encoder_name: &mut String,
    hevc_open_failed: &mut bool,
) {
    if is_hevc_encoder(desired) {
        *hevc_open_failed = true;
    }
    if !recovering && !is_hevc_encoder(current) && encoder_name != current {
        info!(
            from = %encoder_name,
            to = current,
            "Staying on the encoder that streams for this pipeline"
        );
        *encoder_name = current.to_string();
    }
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
            set_nvenc_keyframe_options(&mut opts);
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
            // Like NVENC, QSV makes a requested keyframe a plain I-frame.
            opts.set("forced_idr", "1");
            // Quick Sync queues 4 frames by default. On a still desktop the
            // keepalive sends a frame every 750 ms, so the last change would
            // reach the iPad 3 s late. Laptops moved onto their Intel iGPU
            // encode here when NVENC can't open.
            opts.set("async_depth", "1");
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
            set_nvenc_keyframe_options(&mut opts);
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
            opts.set("forced_idr", "1");
            opts.set("async_depth", "1");
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

/// A new viewer, or one back within the second screen's grace period, can
/// start only from an IDR. NVENC made a requested keyframe a plain I-frame,
/// which the transport never starts a session from, so the viewer waited
/// for the GOP's next IDR: up to 0.5 s at 60 fps, but up to 22 s on a static
/// second screen resent at 1.3 fps, past the iPad's 10 s. NVENC also held
/// each frame back two frames, another 1.5 s at that rate; without the delay
/// it returns each frame as it is encoded.
fn set_nvenc_keyframe_options(opts: &mut ffmpeg_next::Dictionary<'_>) {
    opts.set("forced-idr", "1");
    opts.set("delay", "0");
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

        let mut command = Command::new(&ffmpeg_path);
        command.args([
            "-hide_banner",
            "-v",
            "error",
            "-i",
            capture_path.to_string_lossy().as_ref(),
            "-f",
            "null",
            "-",
        ]);
        // ffmpeg.exe is a console program. The host has no console, so
        // without CREATE_NO_WINDOW Windows would open a console window for it.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
        match command.output() {
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
        let open = || {
            EncoderState::new(
                "libx264",
                640,
                360,
                8_000_000,
                60,
                EncoderInput::Yuv420,
                Opening::First,
                &mut || {},
            )
            .unwrap()
        };
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
    fn nvenc_and_qsv_make_a_requested_keyframe_an_idr() {
        for (name, option) in [
            ("h264_nvenc", "forced-idr"),
            ("hevc_nvenc", "forced-idr"),
            ("h264_qsv", "forced_idr"),
            ("hevc_qsv", "forced_idr"),
        ] {
            assert_eq!(encoder_options(name).get(option), Some("1"), "{name}");
        }
        for name in ["h264_nvenc", "hevc_nvenc"] {
            assert_eq!(encoder_options(name).get("delay"), Some("0"), "{name}");
        }
    }

    /// Encodes frame `n` as the encode loop does, and returns each packet
    /// that came out: whether the transport takes it as a keyframe, and the
    /// capture time and number of its frame.
    fn encode(
        state: &mut EncoderState,
        n: u64,
        captured: Instant,
        force_idr: bool,
    ) -> Vec<(bool, Instant, u64)> {
        let pts = state.next_pts(captured, n);
        state.frame.set_pts(Some(pts));
        prepare_frame_for_encode(
            &mut state.frame,
            n,
            &state.opened_encoder,
            &mut 0,
            force_idr,
        );
        state.send_frame(pts, captured, n).unwrap();
        let mut packets = Vec::new();
        while state.encoder.receive_packet(&mut state.packet).is_ok() {
            if state.h264.needs_parameter_sets() {
                state
                    .h264
                    .refresh_parameter_sets_from_extradata(encoder_extradata(&state.encoder));
            }
            let packet_is_key = state.packet.is_key();
            let (_, info) = normalize_h264_payload_with_info(
                state.packet.data().unwrap_or_default(),
                packet_is_key,
                &mut state.h264,
                &state.opened_encoder,
            );
            let is_keyframe = info.is_keyframe(packet_is_key, &state.opened_encoder);
            let (origin, sequence) = state.packet_origin((captured, n));
            packets.push((is_keyframe, origin, sequence));
        }
        packets
    }

    /// The value of an integer option on the opened encoder.
    fn encoder_option(state: &EncoderState, option: &str) -> u64 {
        let option = std::ffi::CString::new(option).unwrap();
        let mut value = 0;
        let result = unsafe {
            ffmpeg_next::ffi::av_opt_get_int(
                state.encoder.as_ptr().cast_mut().cast(),
                option.as_ptr(),
                ffmpeg_next::ffi::AV_OPT_SEARCH_CHILDREN,
                &mut value,
            )
        };
        assert_eq!(result, 0, "no option {option:?}");
        value.try_into().unwrap()
    }

    /// A viewer back within the second screen's grace period finds the
    /// encoder in the middle of a GOP, and a static second screen resends
    /// its picture every 750 ms. NVENC answered the session's keyframe
    /// request with an I-frame the transport cannot start a session from,
    /// so the first picture waited for the GOP's next IDR, 30 frames on:
    /// 22 s, and the iPad gave up after 10. Runs each encoder that opens
    /// here: NVENC on an NVIDIA PC, libx264 everywhere.
    #[test]
    fn a_viewer_back_mid_gop_starts_from_the_keyframe_it_asked_for() {
        use crate::transport::{first_frame_gate, FirstFrame};
        use std::time::Duration;
        ffmpeg_next::init().unwrap();
        let start = Instant::now();
        let mut tested = Vec::new();
        for name in ["h264_nvenc", "h264_qsv", "libx264"] {
            let Ok(mut state) = EncoderState::new(
                name,
                1280,
                720,
                8_000_000,
                60,
                EncoderInput::Yuv420,
                Opening::First,
                &mut || {},
            ) else {
                continue;
            };
            tested.push(name);
            // The last viewer watched 40 frames at 60 fps and left.
            for n in 0..40 {
                encode(
                    &mut state,
                    n,
                    start + Duration::from_micros(n * 16_667),
                    n == 0,
                );
            }
            // It comes back 5 s later; its session asks for a keyframe.
            let back = start + Duration::from_secs(5);
            let mut force_idr = true;
            let mut first = None;
            'frames: for n in 40..60 {
                let captured = back + Duration::from_millis(10 + (n - 40) * 750);
                for (is_keyframe, origin, sequence) in
                    encode(&mut state, n, captured, std::mem::take(&mut force_idr))
                {
                    match first_frame_gate(is_keyframe, origin, Some(back)) {
                        FirstFrame::Send => {
                            first = Some((n, sequence));
                            break 'frames;
                        }
                        FirstFrame::Skip => {}
                        FirstFrame::SkipAndRequestKeyframe => force_idr = true,
                    }
                }
            }
            // The requested frame itself opens the session, as soon as the
            // encoder's configured delay lets it out: at once on NVENC with
            // delay=0 and libx264 with zerolatency, and once QSV has its
            // async_depth frames queued (set to 1).
            let (submitted, sequence) =
                first.unwrap_or_else(|| panic!("{name}: no frame could open the session"));
            assert_eq!(sequence, 40, "{name}: the session opened on a later frame");
            let held = match name {
                "h264_nvenc" => encoder_option(&state, "delay"),
                "h264_qsv" => encoder_option(&state, "async_depth") - 1,
                _ => 0,
            };
            assert!(
                submitted - 40 <= held,
                "{name}: frame 40 came out after frame {submitted}, more than {held} frames late"
            );
        }
        eprintln!("encoders tested: {tested:?}");
        assert!(tested.contains(&"libx264"));
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
    fn a_stuck_hardware_open_falls_through_to_software_without_waiting_again() {
        use crate::stall::{run_in, Call, Lanes, Stall, Wait};
        use std::time::Duration;
        let lanes: &'static Lanes = Box::leak(Box::new(Lanes::new()));
        // A clock 100 ms on per read: NVENC's 15 s limit passes in 150 reads.
        let clock = || {
            let start = Instant::now();
            let reads = std::sync::atomic::AtomicU64::new(0);
            move || start + Duration::from_millis(100 * reads.fetch_add(1, Ordering::SeqCst))
        };
        let (release, released) = std::sync::mpsc::channel::<()>();
        let mut stuck = Some(move || {
            let _ = released.recv();
        });
        let mut open = |name: &str| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if !takes_codec_lock(name) {
                return Ok(());
            }
            let wait = Wait {
                limits: crate::stall::encoder_open_limits(name),
                slice: Duration::from_millis(1),
                now: clock(),
            };
            // Only the first open hangs; any other would return at once.
            let call = stuck.take();
            run_in(
                lanes,
                Call::EncoderOpen,
                wait,
                move || {
                    if let Some(call) = call {
                        call();
                    }
                },
                || {},
                crate::stall::no_hooks(),
            )
            .map_err(|stall: Stall| stall.to_string().into())
        };
        // NVENC hangs and is given up on at its limit.
        let error = open("h264_nvenc").unwrap_err().to_string();
        assert!(
            error.contains("encoder_open did not return within"),
            "{error}"
        );
        // AMF and QSV open under FFmpeg's lock that NVENC still holds: they
        // are refused at once, and x264 opens.
        let started = Instant::now();
        let mut attempts = Vec::new();
        let (selected, ()) = open_remaining_encoders("h264_nvenc", |name| {
            attempts.push(name.to_string());
            open(name)
        })
        .unwrap();
        assert_eq!(attempts, ["h264_amf", "h264_qsv", "libx264"]);
        assert_eq!(selected, "libx264");
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = release.send(());
    }

    /// Opens `name` as `EncoderState::new` does: in `lanes` under its own
    /// limit if it takes FFmpeg's lock, with a clock 100 ms on per read, so
    /// a limit passes however slowly the machine runs. `call` is the open.
    fn open_in(
        lanes: &'static crate::stall::Lanes,
        name: &str,
        opening: Opening,
        call: impl FnOnce() + Send + 'static,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        use crate::stall::{no_hooks, run_in, Call, Wait};
        if !takes_codec_lock(name) {
            return Ok(name.to_string());
        }
        let start = Instant::now();
        let reads = std::sync::atomic::AtomicU64::new(0);
        let wait = Wait {
            limits: open_limits(name, opening),
            slice: Duration::from_millis(1),
            now: move || start + Duration::from_millis(100 * reads.fetch_add(1, Ordering::SeqCst)),
        };
        match run_in(lanes, Call::EncoderOpen, wait, call, || {}, no_hooks()) {
            Ok(()) => Ok(name.to_string()),
            Err(stall) => Err(Box::new(OpenStalled(format!(
                "{name} open failed: {stall}"
            )))),
        }
    }

    /// An open that blocks until the sender sends or is dropped.
    fn stuck_open() -> (std::sync::mpsc::Sender<()>, impl FnOnce() + Send + 'static) {
        let (release, released) = std::sync::mpsc::channel::<()>();
        (release, move || {
            let _ = released.recv();
        })
    }

    fn lane_clear(lanes: &crate::stall::Lanes) -> bool {
        lanes.abandoned(crate::stall::Call::EncoderOpen) == 0
    }

    fn wait_for_clear_lane(lanes: &crate::stall::Lanes) {
        // Backstop only; the late open returns as soon as it is released.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !lane_clear(lanes) {
            assert!(
                Instant::now() < deadline,
                "the stuck open's lane did not free"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// How a reopen in [`Pipeline::frame`] ends.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Reopen {
        Opens,
        /// Given up on: the open holds the lane until the test clears it.
        Stalls,
        Fails,
    }

    /// The encode loop's choice of encoder for a session that is open, one
    /// frame at a time, making the loop's calls in its order. The test sets
    /// the inputs (HEVC wanted, the client's session, bitrate, frame rate,
    /// the lane, the clock) and says how each reopen ends.
    struct Pipeline {
        recovery: StallRecovery,
        encoder_name: String,
        hevc_open_failed: bool,
        failed_reopen: Option<(String, u32, u32)>,
        negotiated: Option<(bool, Option<u32>)>,
        session: u32,
        /// Encoder, bitrate and frame rate of the open session.
        opened: (String, u32, u32),
        hevc_wanted: bool,
        bitrate: u32,
        fps: u32,
        keyframe: bool,
        lane_clear: bool,
        now: Instant,
    }

    impl Pipeline {
        /// `opened` streams at 8 Mbps and 60 fps, and `encoder_name` is the
        /// H.264 encoder. HEVC is wanted if `opened` is HEVC.
        fn new(encoder_name: &str, opened: &str, now: Instant) -> Self {
            Self {
                recovery: StallRecovery::default(),
                encoder_name: encoder_name.to_string(),
                hevc_open_failed: false,
                failed_reopen: None,
                negotiated: None,
                session: 1,
                opened: (opened.to_string(), 8_000_000, 60),
                hevc_wanted: is_hevc_encoder(opened),
                bitrate: 8_000_000,
                fps: 60,
                keyframe: false,
                lane_clear: true,
                now,
            }
        }

        /// One frame. The encoder it reopened to, if it reopened.
        fn frame(&mut self, reopen: Reopen) -> Option<String> {
            if !self.hevc_wanted {
                self.hevc_open_failed = false;
            }
            let preferred = if self.hevc_wanted && !self.hevc_open_failed {
                hevc_variant_of(&self.encoder_name).unwrap().to_string()
            } else {
                self.encoder_name.clone()
            };
            let desired = self
                .recovery
                .encoder_for_frame(&preferred, self.now, self.lane_clear);
            forget_failed_reopen(
                &mut self.failed_reopen,
                &mut self.negotiated,
                (self.hevc_wanted, Some(self.session)),
                &desired,
            );
            let opened = (self.opened.0.as_str(), self.opened.1, self.opened.2);
            if !reopen_needed(
                opened,
                (&desired, self.bitrate, self.fps),
                self.keyframe,
                self.failed_reopen.as_ref(),
                self.lane_clear,
            ) {
                return None;
            }
            self.recovery.starting(&desired);
            if reopen == Reopen::Opens {
                self.opened = (desired.clone(), self.bitrate, self.fps);
                self.failed_reopen = None;
                self.keyframe = false;
                self.recovery.opened(&desired, &preferred);
                return Some(desired);
            }
            let stalled = reopen == Reopen::Stalls;
            if stalled && takes_codec_lock(&desired) {
                self.lane_clear = false;
            }
            let current = self.opened.0.clone();
            if !self
                .recovery
                .failed(stalled, &preferred, &current, self.now)
            {
                give_up_reopen(
                    &desired,
                    &current,
                    self.recovery.owed.is_some(),
                    &mut self.encoder_name,
                    &mut self.hevc_open_failed,
                );
                self.failed_reopen = Some((desired.clone(), self.bitrate, self.fps));
            }
            Some(desired)
        }

        fn later(&mut self, ms: u64) {
            self.now += Duration::from_millis(ms);
        }
    }

    #[test]
    fn a_stuck_qsv_open_streams_on_x264_then_goes_back_to_qsv_once_the_lane_frees() {
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        let mut recovery = StallRecovery::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // The pipeline's first frame: QSV's open hangs, and is given up on
        // at QSV's own limit.
        let preferred = "h264_qsv";
        assert_eq!(
            recovery.encoder_for_frame(preferred, at(0), lane_clear(lanes)),
            preferred
        );
        let (release, hang) = stuck_open();
        let error = open_in(lanes, preferred, Opening::First, hang).unwrap_err();
        assert!(is_open_stall(error.as_ref()), "{error}");
        assert!(
            error.to_string().contains("did not return within 15"),
            "{error}"
        );
        // NVENC and AMF are refused at once behind it; x264 gives a picture.
        let mut attempts = Vec::new();
        let (name, opened) = open_remaining_encoders(preferred, |name| {
            attempts.push(name.to_string());
            open_in(lanes, name, Opening::First, || {})
        })
        .unwrap();
        assert_eq!(attempts, ["h264_nvenc", "h264_amf", "libx264"]);
        assert_eq!((name, opened.as_str()), ("libx264", "libx264"));
        // The encode loop's calls for this fallback.
        assert!(recovery.failed(true, preferred, name, at(0)));
        assert!(!recovery.opened(name, preferred));
        // x264 streams while the QSV open is stuck, even after the first
        // wait is over.
        for ms in [0, 1_000, 2_000, 10_000] {
            assert_eq!(
                recovery.encoder_for_frame(preferred, at(ms), lane_clear(lanes)),
                "libx264",
                "at {ms} ms"
            );
        }
        // The stuck open returns. The next frame tries QSV again through
        // the reopen path, and it opens.
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        assert_eq!(
            recovery.encoder_for_frame(preferred, at(10_050), true),
            preferred
        );
        recovery.starting(preferred);
        assert_eq!(
            open_in(lanes, preferred, Opening::Reopen, || {}).unwrap(),
            preferred
        );
        assert!(
            recovery.opened(preferred, preferred),
            "the stream did not go back to QSV"
        );
        // QSV stays, and nothing is owed any more.
        assert_eq!(
            recovery.encoder_for_frame(preferred, at(99_000), true),
            preferred
        );
        assert!(recovery.owed.is_none());
        assert_eq!(recovery.tries, 0);
    }

    #[test]
    fn a_stalled_x265_open_is_tried_again_instead_of_dropping_hevc() {
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        let mut recovery = StallRecovery::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // libx264 is preferred and the iPad decodes HEVC, so libx265 opens
        // first. It hangs and is given up on at its own, longer limit.
        let (release, hang) = stuck_open();
        let error = open_in(lanes, "libx265", Opening::First, hang).unwrap_err();
        assert!(is_open_stall(error.as_ref()), "{error}");
        assert!(
            error.to_string().contains("did not return within 20"),
            "{error}"
        );
        // The loop's HEVC arm: H.264 meanwhile, and HEVC is not given up.
        assert!(recovery.failed(true, "libx265", "libx264", at(0)));
        let desired = recovery.encoder_for_frame("libx265", at(20), lane_clear(lanes));
        assert_eq!(desired, "libx264");
        let opened = open_in(lanes, &desired, Opening::First, || {}).unwrap();
        assert!(!recovery.opened(&opened, "libx265"));
        // The lane frees before the first wait is over: still x264.
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        assert_eq!(
            recovery.encoder_for_frame("libx265", at(1_999), true),
            "libx264"
        );
        // Then HEVC is tried again and opens.
        assert_eq!(
            recovery.encoder_for_frame("libx265", at(2_000), true),
            "libx265"
        );
        recovery.starting("libx265");
        assert_eq!(
            open_in(lanes, "libx265", Opening::Reopen, || {}).unwrap(),
            "libx265"
        );
        assert!(recovery.opened("libx265", "libx265"));
        assert_eq!(
            recovery.encoder_for_frame("libx265", at(2_050), true),
            "libx265"
        );
    }

    #[test]
    fn a_reopen_of_a_live_hardware_session_is_given_up_on_sooner() {
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        // The old session goes on, frozen, while a reopen waits: 9 s for the
        // hardware encoders, against 15 s when nothing streams yet.
        for (name, opening, seconds) in [
            ("h264_qsv", Opening::Reopen, 9),
            ("hevc_nvenc", Opening::Reopen, 9),
            ("h264_amf", Opening::First, 15),
            ("libx265", Opening::Reopen, 20),
        ] {
            let (release, hang) = stuck_open();
            let error = open_in(lanes, name, opening, hang).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("did not return within {seconds}0")),
                "{name} {opening:?}: {error}"
            );
            release.send(()).unwrap();
            wait_for_clear_lane(lanes);
        }
        assert_eq!(
            open_limits("libopus", Opening::Reopen),
            crate::stall::encoder_open_limits("libopus")
        );
    }

    #[test]
    fn the_preferred_encoder_is_tried_at_most_three_times_a_pipeline() {
        let t0 = Instant::now();
        // NVENC's first open stalled and x264 streams: the loop's calls.
        let mut p = Pipeline::new("h264_nvenc", "libx264", t0);
        assert!(p.recovery.failed(true, "h264_nvenc", "libx264", t0));
        p.lane_clear = false;
        // Each try waits longer, and each hangs again. The driver returns
        // each stuck open eventually, so the lane frees between tries.
        for wait in [2_000, 8_000, 30_000] {
            let stalled_at = p.now;
            p.lane_clear = true;
            p.now = stalled_at + Duration::from_millis(wait - 1);
            assert_eq!(p.frame(Reopen::Opens), None, "before {wait} ms");
            p.now = stalled_at + Duration::from_millis(wait);
            assert_eq!(
                p.frame(Reopen::Stalls).as_deref(),
                Some("h264_nvenc"),
                "after {wait} ms"
            );
        }
        assert_eq!(p.recovery.tries, 3);
        // No fourth: x264 is the pipeline's encoder, as after a real error.
        assert!(p.recovery.owed.is_none());
        assert_eq!(p.encoder_name, "libx264");
        p.lane_clear = true;
        for hours in [1, 24] {
            p.later(hours * 3_600_000);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        // x264's own reopens never wait for the lane.
        p.lane_clear = false;
        p.fps = 30;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("libx264"));
        assert_eq!(p.recovery.tries, 3);
    }

    #[test]
    fn a_real_open_error_is_not_tried_again() {
        ffmpeg_next::init().unwrap();
        // A real FFmpeg failure is not a stall.
        let error = EncoderState::new(
            "libx264",
            0,
            0,
            8_000_000,
            60,
            EncoderInput::Yuv420,
            Opening::First,
            &mut || {},
        )
        .err()
        .expect("a 0x0 libx264 opened");
        assert!(!is_open_stall(error.as_ref()), "{error}");
        // A first open that fails for real owes nothing: the loop replaces
        // the preferred encoder with the fallback, as before.
        let mut recovery = StallRecovery::default();
        let now = Instant::now();
        assert!(!recovery.failed(false, "h264_amf", "libx264", now));
        assert!(!recovery.opened("libx264", "h264_amf"));
        assert!(recovery.owed.is_none());
        // After a stall, a try that fails for real ends the recovery, and
        // x264 becomes the pipeline's encoder.
        let mut p = Pipeline::new("h264_amf", "libx264", now);
        assert!(p.recovery.failed(true, "h264_amf", "libx264", now));
        p.later(2_000);
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_amf"));
        assert!(p.recovery.owed.is_none());
        assert_eq!(p.encoder_name, "libx264");
        for hours in [1, 24] {
            p.later(hours * 3_600_000);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        // A later stall of the fallback's own reopen does not bring tries back.
        assert!(!p.recovery.failed(true, "libx264", "libx264", p.now));
        assert!(p.recovery.owed.is_none());
    }

    #[test]
    fn an_hevc_session_whose_reopens_stall_takes_h264_after_three_tries() {
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        // An ABR step: the HEVC session's reopen stalls, and the session
        // streams on at its old bitrate.
        p.bitrate = 12_000_000;
        assert_eq!(p.frame(Reopen::Stalls).as_deref(), Some("hevc_nvenc"));
        // While the stuck open holds the lane, nothing else opens in it.
        for _ in 0..100 {
            p.later(16);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        // Each time the lane clears the bitrate reopen goes again, and
        // stalls again. Each counts as a try.
        for attempt in 1..=3 {
            p.later(1_000);
            p.lane_clear = true;
            assert_eq!(
                p.frame(Reopen::Stalls).as_deref(),
                Some("hevc_nvenc"),
                "try {attempt}"
            );
            assert_eq!(p.recovery.tries, attempt);
        }
        // Out of tries: nothing is owed, and HEVC is off for this client.
        assert!(p.recovery.owed.is_none());
        assert!(p.hevc_open_failed);
        // Not a fourth HEVC open, then or once the lane is clear: the next
        // frame with a clear lane reopens on H.264, at once and once.
        for _ in 0..100 {
            p.later(16);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        p.lane_clear = true;
        assert_eq!(p.frame(Reopen::Stalls).as_deref(), Some("h264_nvenc"));
        // That stalled as well. HEVC keeps streaming, and nothing is tried
        // again frame after frame, with or without the lane.
        for clear in [false, true] {
            p.lane_clear = clear;
            for _ in 0..100 {
                p.later(16);
                assert_eq!(p.frame(Reopen::Opens), None);
            }
        }
        assert_eq!(p.opened.0, "hevc_nvenc");
        // A new bitrate is a new reopen, once.
        p.bitrate = 10_000_000;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.frame(Reopen::Opens), None);
        assert_eq!(p.recovery.tries, 3);
    }

    #[test]
    fn a_real_error_in_a_try_of_hevc_takes_h264_at_once() {
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.bitrate = 12_000_000;
        assert_eq!(p.frame(Reopen::Stalls).as_deref(), Some("hevc_nvenc"));
        p.later(500);
        p.lane_clear = true;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("hevc_nvenc"));
        assert!(p.recovery.owed.is_none() && p.hevc_open_failed);
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        for _ in 0..100 {
            assert_eq!(p.frame(Reopen::Opens), None);
        }
    }

    #[test]
    fn hevc_turned_off_during_a_recovery_takes_effect_once_the_lane_is_clear() {
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.bitrate = 12_000_000;
        assert_eq!(p.frame(Reopen::Stalls).as_deref(), Some("hevc_nvenc"));
        // The user turns HEVC off (or an H.264-only iPad connects) while
        // the stuck open holds the lane: H.264 waits only for the lane.
        p.hevc_wanted = false;
        p.later(100);
        assert_eq!(p.frame(Reopen::Opens), None);
        p.later(400);
        p.lane_clear = true;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert!(p.recovery.owed.is_none());
        assert_eq!(p.frame(Reopen::Opens), None);

        // On x264 while libx265 is owed, HEVC off leaves nothing owed.
        let now = Instant::now();
        let mut p = Pipeline::new("libx264", "libx264", now);
        p.hevc_wanted = true;
        assert!(p.recovery.failed(true, "libx265", "libx264", now));
        p.lane_clear = false;
        p.hevc_wanted = false;
        assert_eq!(p.frame(Reopen::Opens), None);
        assert!(p.recovery.owed.is_none());
        p.lane_clear = true;
        p.later(60_000);
        assert_eq!(p.frame(Reopen::Opens), None);
    }

    #[test]
    fn an_hevc_reopen_that_failed_is_tried_again_after_hevc_is_turned_off_and_on() {
        // H.264 streams and the user turns HEVC on. The HEVC reopen fails,
        // and H.264 streams on without trying HEVC again.
        let mut p = Pipeline::new("h264_nvenc", "h264_nvenc", Instant::now());
        p.hevc_wanted = true;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("hevc_nvenc"));
        assert!(p.hevc_open_failed);
        for _ in 0..100 {
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        // HEVC off, then on again, at the same bitrate and frame rate.
        p.hevc_wanted = false;
        assert_eq!(p.frame(Reopen::Opens), None);
        p.hevc_wanted = true;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("hevc_nvenc"));
        assert_eq!(p.frame(Reopen::Opens), None);
    }

    #[test]
    fn an_h264_downgrade_that_failed_is_tried_again_when_hevc_is_turned_off() {
        // HEVC streams. An ABR step's HEVC reopen fails, and so does the
        // H.264 reopen that follows: HEVC streams on at the old bitrate.
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.bitrate = 12_000_000;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("hevc_nvenc"));
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        for _ in 0..100 {
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        assert_eq!(p.opened.0, "hevc_nvenc");
        // The user turns HEVC off: the stream must leave HEVC.
        p.hevc_wanted = false;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.opened, ("h264_nvenc".to_string(), 12_000_000, 60));
        assert_eq!(p.frame(Reopen::Opens), None);
    }

    #[test]
    fn an_h264_only_client_after_a_failed_downgrade_gets_h264() {
        // HEVC streams and the user turns HEVC off. The H.264 reopen fails,
        // and HEVC streams on.
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.hevc_wanted = false;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        for _ in 0..100 {
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        assert_eq!(p.opened.0, "hevc_nvenc");
        // An iPad that decodes only H.264 connects: it gets H.264.
        p.session = 2;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.frame(Reopen::Opens), None);
    }

    #[test]
    fn a_recovery_that_ends_in_a_real_error_does_not_hold_a_new_preference() {
        // HEVC streams. An ABR step's HEVC reopen stalls; its try fails for
        // real, ending the recovery, and the H.264 reopen fails too.
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.bitrate = 12_000_000;
        assert_eq!(p.frame(Reopen::Stalls).as_deref(), Some("hevc_nvenc"));
        p.later(500);
        p.lane_clear = true;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("hevc_nvenc"));
        assert!(p.recovery.owed.is_none() && p.hevc_open_failed);
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        for _ in 0..100 {
            p.later(16);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        // HEVC off takes H.264, and HEVC on again tries HEVC, once each.
        p.hevc_wanted = false;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.frame(Reopen::Opens), None);
        p.hevc_wanted = true;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("hevc_nvenc"));
        assert_eq!(p.frame(Reopen::Opens), None);
        assert_eq!(p.recovery.tries, 3);
    }

    #[test]
    fn a_reopen_goes_ahead_as_soon_as_the_lane_clears_whenever_a_try_is_due() {
        // QSV streams. ABR steps stall its reopen, twice: the next tries
        // are due 2 s and then 8 s later. Each time the lane clears 1 s
        // after the stall, and a bitrate change reopens on the next frame.
        let mut p = Pipeline::new("h264_qsv", "h264_qsv", Instant::now());
        for (bitrate, stalls) in [(10_000_000, true), (12_000_000, true), (14_000_000, false)] {
            p.bitrate = bitrate;
            let reopen = if stalls {
                Reopen::Stalls
            } else {
                Reopen::Opens
            };
            assert_eq!(p.frame(reopen).as_deref(), Some("h264_qsv"));
            if !stalls {
                break;
            }
            p.later(1_000);
            p.lane_clear = true;
        }
        assert_eq!(p.opened, ("h264_qsv".to_string(), 14_000_000, 60));
        assert!(p.recovery.owed.is_none());
        // A frame rate change and an AMF keyframe, the same way.
        let mut p = Pipeline::new("h264_amf", "h264_amf", Instant::now());
        p.fps = 30;
        assert_eq!(p.frame(Reopen::Stalls).as_deref(), Some("h264_amf"));
        p.fps = 60;
        p.keyframe = true;
        p.later(16);
        assert_eq!(p.frame(Reopen::Opens), None, "the lane is still stuck");
        p.later(1_000);
        p.lane_clear = true;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_amf"));
        assert!(p.recovery.owed.is_none());
    }

    #[test]
    fn a_qsv_open_as_slow_as_the_slowest_seen_opens_on_qsv() {
        use crate::stall::{encoder_open_limits, no_hooks, run_in, Call, Lanes, Wait};
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        let lanes: &'static Lanes = Box::leak(Box::new(Lanes::new()));
        let out = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
        struct Sink(Arc<parking_lot::Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer = Arc::clone(&out);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Sink(Arc::clone(&writer)))
            .finish();
        let mut recovery = StallRecovery::default();
        let now = Instant::now();
        let desired = recovery.encoder_for_frame("h264_qsv", now, lane_clear(lanes));
        // The open returns once the caller's clock reads 4.4 s, the slowest
        // QSV open in the field logs.
        let start = Instant::now();
        let reads = Arc::new(AtomicU64::new(0));
        let clock_reads = Arc::clone(&reads);
        let opened = tracing::subscriber::with_default(subscriber, || {
            run_in(
                lanes,
                Call::EncoderOpen,
                Wait {
                    limits: encoder_open_limits(&desired),
                    slice: Duration::from_millis(1),
                    now: move || {
                        start
                            + Duration::from_millis(
                                100 * clock_reads.fetch_add(1, Ordering::SeqCst),
                            )
                    },
                },
                move || {
                    while reads.load(Ordering::SeqCst) < 44 {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    "h264_qsv"
                },
                || {},
                no_hooks(),
            )
        });
        assert_eq!(opened, Ok("h264_qsv"));
        assert!(!recovery.opened(&desired, "h264_qsv"));
        assert!(recovery.owed.is_none());
        assert!(lane_clear(lanes));
        let lines = String::from_utf8(out.lock().clone()).unwrap();
        assert!(!lines.contains("stall=encoder_open"), "{lines}");
        // Not even slow: the warning starts above the slowest open seen.
        assert!(!lines.contains("slow=encoder_open"), "{lines}");
    }

    #[test]
    fn hevc_software_does_not_block_behind_a_stuck_hardware_open() {
        use crate::stall::{no_hooks, run_in, Call, Lanes, Stall, Wait};
        use std::time::Duration;
        for name in ["h264_nvenc", "hevc_amf", "h264_qsv", "libx265"] {
            assert!(takes_codec_lock(name), "{name}");
        }
        for name in ["libx264", "h264_videotoolbox", "hevc_videotoolbox"] {
            assert!(!takes_codec_lock(name), "{name}");
        }
        let lanes: &'static Lanes = Box::leak(Box::new(Lanes::new()));
        // An NVENC open is stuck holding FFmpeg's lock.
        let start = Instant::now();
        let reads = std::sync::atomic::AtomicU64::new(0);
        let (release, released) = std::sync::mpsc::channel::<()>();
        let stuck = run_in(
            lanes,
            Call::EncoderOpen,
            Wait {
                limits: Call::EncoderOpen.limits(),
                slice: Duration::from_millis(1),
                now: move || {
                    start + Duration::from_millis(100 * reads.fetch_add(1, Ordering::SeqCst))
                },
            },
            move || {
                let _ = released.recv();
            },
            || {},
            no_hooks(),
        );
        assert!(matches!(stuck, Err(Stall::TimedOut { .. })));
        let context = |name: &str| {
            let codec = ffmpeg_next::encoder::find_by_name(name)?;
            let mut video = ffmpeg_next::codec::Context::new_with_codec(codec)
                .encoder()
                .video()
                .ok()?;
            video.set_width(64);
            video.set_height(64);
            video.set_format(ffmpeg_next::format::Pixel::YUV420P);
            video.set_time_base(ffmpeg_next::Rational(1, 60));
            Some(video)
        };
        // A reconnect asks for HEVC again: libx265 is refused at once
        // instead of waiting on the lock with no limit, and the encode loop
        // stays on H.264.
        if let Some(video) = context("libx265") {
            let started = Instant::now();
            let opened = open_encoder_on(lanes, video, "libx265", Opening::First, &mut || {});
            assert!(
                matches!(
                    opened,
                    Err(Stall::Refused {
                        call: Call::EncoderOpen
                    })
                ),
                "{:?}",
                opened.map(|r| r.map(|_| ()))
            );
            assert!(started.elapsed() < Duration::from_secs(1));
        }
        // libx264 opens meanwhile.
        let video = context("libx264").expect("libx264 is in every build");
        assert!(matches!(
            open_encoder_on(lanes, video, "libx264", Opening::First, &mut || {}),
            Ok(Ok(_))
        ));
        let _ = release.send(());
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
        let mut state = EncoderState::new(
            "libx264",
            640,
            360,
            8_000_000,
            60,
            EncoderInput::Yuv420,
            Opening::First,
            &mut || {},
        )
        .unwrap();
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

    /// The real encode stage on NVENC at 720p, fed a synthetic frame every
    /// 33 ms, with every thread's lines kept for the checks and echoed.
    #[cfg(windows)]
    struct NativeStage {
        lines: std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
        shared: SharedControl,
        producer: FrameSlot,
        stage: std::thread::JoinHandle<Result<(), String>>,
        collector: std::thread::JoinHandle<()>,
        /// When each packet arrived, from `started`.
        packets: std::sync::Arc<parking_lot::Mutex<Vec<std::time::Duration>>>,
        started: Instant,
        number: u64,
    }

    #[cfg(windows)]
    impl NativeStage {
        fn start() -> Self {
            use std::sync::Arc;
            let lines = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
            struct Sink(Arc<parking_lot::Mutex<Vec<u8>>>);
            impl Write for Sink {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0.lock().extend_from_slice(buf);
                    std::io::stderr().write_all(buf)?;
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let writer = Arc::clone(&lines);
            tracing::subscriber::set_global_default(
                tracing_subscriber::fmt()
                    .with_ansi(false)
                    .with_writer(move || Sink(Arc::clone(&writer)))
                    .finish(),
            )
            .expect("run this test on its own");
            ffmpeg_next::init().unwrap();

            let (producer, consumer) = crate::capture::frame_slot();
            let (tx, mut rx) = mpsc::channel(NAL_CHANNEL_CAPACITY);
            let shared = SharedControl::new(0, 8_000_000);
            shared.running.store(true, Ordering::SeqCst);
            let gpu = GpuInfo {
                encoder_name: "h264_nvenc".to_string(),
                codec_display_name: "H.264 (NVENC)".to_string(),
                ..GpuInfo::software_fallback()
            };
            let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
            let stage_shared = shared.clone();
            let stage = std::thread::spawn(move || {
                run_encode_stage(consumer, tx, stage_shared, gpu, generation)
                    .map_err(|e| e.to_string())
            });
            let started = Instant::now();
            let packets = Arc::new(parking_lot::Mutex::new(Vec::new()));
            let collector = {
                let packets = Arc::clone(&packets);
                std::thread::spawn(move || {
                    while rx.blocking_recv().is_some() {
                        packets.lock().push(started.elapsed());
                    }
                })
            };
            Self {
                lines,
                shared,
                producer,
                stage,
                collector,
                packets,
                started,
                number: 0,
            }
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.lines.lock()).into_owned()
        }

        fn publish(&mut self) {
            let mut data = vec![0u8; 1280 * 720 * 4];
            crate::capture::synthetic::render_synthetic_frame(&mut data, 1280, 720, self.number);
            self.producer.publish(crate::capture::RawFrame {
                frame_number: self.number,
                timestamp: Instant::now(),
                data: std::sync::Arc::new(data),
                width: 1280,
                height: 720,
            });
            self.number += 1;
            std::thread::sleep(std::time::Duration::from_millis(33));
        }

        /// Publish until `needle` shows after byte `from` of the text, within
        /// `limit`. When it showed, to within a frame, and where.
        fn publish_until(
            &mut self,
            needle: &str,
            from: usize,
            limit: std::time::Duration,
        ) -> (std::time::Duration, usize) {
            let deadline = Instant::now() + limit;
            loop {
                if let Some(at) = self.text()[from..].find(needle) {
                    return (self.started.elapsed(), from + at);
                }
                assert!(
                    Instant::now() < deadline,
                    "no {needle:?} line:\n{}",
                    self.text()
                );
                self.publish();
            }
        }

        /// Packets that arrived between `from` and `to`.
        fn packets_between(&self, from: std::time::Duration, to: std::time::Duration) -> usize {
            self.packets
                .lock()
                .iter()
                .filter(|&&at| at > from && at < to)
                .count()
        }

        fn finish(self) -> String {
            self.producer.close();
            self.stage.join().unwrap().unwrap();
            self.collector.join().unwrap();
            String::from_utf8_lossy(&self.lines.lock()).into_owned()
        }
    }

    /// Native, on a PC with NVENC: the encode stage's first NVENC open is
    /// held for twice its limit (`ETERNAL_SIMULATE_STALL`). The stage falls
    /// through to software at once and streams, the late NVENC session is
    /// closed on its own thread, and the stage then goes back to NVENC.
    /// `cargo test -p eternal-host --lib a_real_nvenc_open -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore = "native: needs an NVIDIA GPU"]
    fn a_real_nvenc_open_that_stalls_recovers_through_software() {
        use std::time::Duration;
        std::env::set_var("ETERNAL_SIMULATE_STALL", "encoder_open");
        let mut stage = NativeStage::start();
        let started = stage.started;
        // When each step's line first shows, to within a frame.
        let steps = [
            "stall=encoder_open",
            "streaming on the fallback",
            "stall_ended=encoder_open",
            "Back on the preferred encoder",
        ];
        let mut seen: [Option<Duration>; 4] = [None; 4];
        while seen[3].is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "the stage did not go back to NVENC:\n{}",
                stage.text()
            );
            stage.publish();
            let now = stage.text();
            for (step, at) in steps.iter().zip(seen.iter_mut()) {
                if at.is_none() && now.contains(step) {
                    *at = Some(started.elapsed());
                }
            }
        }
        for _ in 0..30 {
            stage.publish();
        }
        let [stalled, _, ended, back] = seen.map(Option::unwrap);
        // A picture throughout: x264 while the open was stuck, NVENC after.
        let on_x264 = stage.packets_between(stalled + Duration::from_secs(1), ended);
        let on_nvenc = stage.packets_between(back, Duration::MAX);
        let text = stage.finish();
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("no {needle:?} line:\n{text}"))
        };
        let order: Vec<usize> = steps.iter().map(|step| at(step)).collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
        let back_line = text[order[3]..].lines().next().unwrap();
        assert!(back_line.contains("h264_nvenc"), "{back_line}");
        eprintln!("stalled at {stalled:?}, stall ended at {ended:?}, back on NVENC at {back:?}");
        // Given up on at NVENC's 15 s limit; the held open returns at 30 s.
        assert!(
            stalled >= Duration::from_secs(15) && stalled < Duration::from_secs(18),
            "{stalled:?}"
        );
        assert!(ended >= Duration::from_secs(30), "{ended:?}");
        assert!(back < ended + Duration::from_secs(5), "{back:?}");
        eprintln!("packets: {on_x264} on x264 during the stall, {on_nvenc} on NVENC after");
        assert!(on_x264 >= 100, "{on_x264} packets while the open was stuck");
        assert!(on_nvenc >= 20, "{on_nvenc} packets after NVENC came back");
        let stats = PIPELINE_STATS.lock();
        assert_eq!(stats.codec_name, "H.264 (NVENC)");
        assert!(!stats.using_software_fallback);
    }

    /// Native, on a PC with NVENC: an ABR step's NVENC reopen
    /// is held for 10 s, so it is given up on at the 9 s reopen limit and
    /// the old session streams on. The lane clears 1 s later, before the
    /// first try would be due (2 s), and a bitrate change then reopens at
    /// once instead of waiting for it.
    /// `cargo test -p eternal-host --lib a_bitrate_change_after -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore = "native: needs an NVIDIA GPU"]
    fn a_bitrate_change_after_a_stalled_nvenc_reopen_clears_is_prompt() {
        use std::time::Duration;
        let mut stage = NativeStage::start();
        let (_, opened) = stage.publish_until("Encoder opened", 0, Duration::from_secs(20));
        for _ in 0..30 {
            stage.publish();
        }
        let text = stage.text();
        assert!(text[opened..]
            .lines()
            .next()
            .unwrap()
            .contains("h264_nvenc"));
        // Only calls from here on are held.
        std::env::set_var("ETERNAL_SIMULATE_STALL", "encoder_open");
        std::env::set_var("ETERNAL_SIMULATE_STALL_MS", "10000");
        let from = text.len();
        stage
            .shared
            .abr_current_bps
            .store(6_000_000, Ordering::SeqCst);
        let stepped = stage.started.elapsed();
        let (stalled, at) =
            stage.publish_until("stall=encoder_open", from, Duration::from_secs(20));
        // ABR steps back to the session's bitrate meanwhile: nothing is
        // left to reopen when the lane clears, but the try is owed.
        stage
            .shared
            .abr_current_bps
            .store(8_000_000, Ordering::SeqCst);
        let (ended, at) =
            stage.publish_until("stall_ended=encoder_open", at, Duration::from_secs(20));
        stage
            .shared
            .abr_current_bps
            .store(5_000_000, Ordering::SeqCst);
        let changed = stage.started.elapsed();
        let (reopened, _) = stage.publish_until("to=5000000", at, Duration::from_secs(10));
        for _ in 0..30 {
            stage.publish();
        }
        let on_old_session = stage.packets_between(stalled + Duration::from_millis(500), ended);
        let after = stage.packets_between(reopened, Duration::MAX);
        let text = stage.finish();
        eprintln!(
            "ABR step at {stepped:?}, given up on at {stalled:?}, stall ended at {ended:?}, \
             bitrate changed at {changed:?}, reopened at {reopened:?}; \
             {on_old_session} packets on the old session, {after} after"
        );
        // Given up on at the reopen limit, not the first open's 15 s.
        let held = stalled - stepped;
        assert!(
            held >= Duration::from_secs(9) && held < Duration::from_secs(11),
            "{held:?}"
        );
        assert!(
            on_old_session >= 5,
            "{on_old_session} packets on the old session"
        );
        // At once: well before the first try was due, 2 s after the stall.
        assert!(
            reopened - changed < Duration::from_millis(500),
            "{:?}",
            reopened - changed
        );
        assert!(reopened < stalled + Duration::from_secs(2), "{reopened:?}");
        assert!(after >= 20, "{after} packets after the reopen");
        let line = text
            .lines()
            .find(|line| line.contains("to=5000000"))
            .unwrap();
        assert!(line.contains("Reopened encoder session"), "{line}");
        assert!(line.contains("h264_nvenc"), "{line}");
    }
}
