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

use crate::capture::{FrameSlot, RawFrame, Taken};
use crate::control::SharedControl;
use crate::gpu::{GpuInfo, GpuVendor};
use crate::stats::PIPELINE_STATS;

pub mod input;
use input::{select_input, EncoderInput};
#[cfg(target_os = "macos")]
mod vimage;

const CHANNEL_CAPACITY: usize = 4;
const AMF_CAPTURE_PACKET_LIMIT: u64 = 120;
const AMF_IDR_WARNING_PACKET: u64 = 60;
const AMF_FORCED_INTRA_PERIOD: u64 = 30;

/// The size a capture of `width`x`height` streams at. 4:2:0 needs even
/// dimensions, which libx264 enforces and hardware encoders may too, so a
/// screen such as 1680x945 encodes without its odd last row (or column):
/// cropped, not scaled, so every other pixel stays exact. Input still maps
/// onto the whole capture (see [`crate::input::norm_to_output_pixel`]), so
/// the cropped row stays reachable at the picture's edge.
pub fn encoded_size(width: u32, height: u32) -> (u32, u32) {
    (width & !1, height & !1)
}

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
                let display = codec_display_name_for(&name).to_string();
                if display == name {
                    warn!(encoder = %name, "Unknown encoder override label");
                }
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
    // The processor encodes only because startup's GPU detection did not
    // return in time, and no override chose the encoder.
    let detection_stalled = gpu.detection_stalled && shared.encoder_override.lock().is_none();
    {
        let (nvenc_failed, vendors) = {
            let failures = LAUNCH_FAILURES.lock();
            (failures.any("_nvenc"), failures.vendors.clone())
        };
        let mut stats = PIPELINE_STATS.lock();
        stats.set_codec_name(&codec_display_name);
        // Fresh pipeline start: clear any stale software-fallback flag. The encode loop
        // re-sets it below only if a hardware encoder actually fails to open. A deliberate
        // libx264 override is not a fallback, so it intentionally leaves this false. A
        // stalled detection is one from the start.
        stats.set_software_fallback(detection_stalled);
        stats.gpu_detection_stalled = detection_stalled;
        // A driver note stays while the failure that gave it is remembered,
        // as that encoder is not tried again to give it anew.
        stats.nvidia_driver_outdated &= nvenc_failed;
        stats.driver_update_vendors = vendors;
    }

    let mut encoder_state: Option<EncoderState> = None;
    // Automatic's BGRA can give way to YUV420 for the rest of the pipeline
    // (see `open_at_input`).
    let mut input_mode = *shared.encoder_input.lock();
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
    // Except when the client cannot decode what streams: then the reopen is
    // tried again with backoff.
    let mut undecodable_retry = UndecodableRetry::default();
    // HEVC gating memory: warn only once when no HEVC sibling exists, and
    // don't retry an HEVC open that failed until the preference or the
    // session changes (hevc_wanted going false resets it).
    let mut hevc_unavailable_warned = false;
    let mut hevc_open_failed = false;
    let mut stall_recovery = StallRecovery::default();
    // An open in the lane going on in the background while libx264 streams.
    let mut background: Option<Behind> = None;
    // When a picture is due while no session is open: FIRST_PICTURE_WAIT
    // after the first try of an encoder.
    let mut first_picture_by: Option<Instant> = None;
    let mut last_keyframe_reopen: Option<Instant> = None;
    let mut pending_keyframe = false;
    let mut bitrate_pacer = BitratePacer::default();

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

    let mut owed_keyframe: Option<OwedKeyframe> = None;
    loop {
        let raw_frame = match owed_keyframe.as_mut() {
            None => match frames.blocking_take() {
                Some(frame) => frame,
                None => break,
            },
            Some(owed) => match owed.next(&frames) {
                Next::Frame(frame) => frame,
                Next::Closed => break,
                Next::GaveUp => {
                    info!(
                        given = owed.given,
                        "The encoder still holds the session's first keyframe; waiting for the next capture"
                    );
                    owed_keyframe = None;
                    continue;
                }
            },
        };
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
        let preferred = preferred_encoder(
            &encoder_name,
            hevc_wanted,
            hevc_open_failed,
            &mut hevc_unavailable_warned,
        );
        // An HEVC open going on in the background is of no use once this
        // frame wants H.264: the stream goes on, and reopens from there.
        if drop_obsolete_background(&mut background, &preferred) {
            stall_recovery.trying = false;
        }
        // No open given up on, or going on in the background, still holds
        // FFmpeg's lock. Until then, an open in that lane would only be
        // refused.
        let lane_clear = crate::stall::abandoned(crate::stall::Call::EncoderOpen) == 0;
        let desired_encoder =
            stall_recovery.encoder_for_frame(&preferred, Instant::now(), lane_clear);
        // While an encoder opens in the background, libx264 streams on.
        let desired_encoder = match (&background, &encoder_state) {
            (Some(_), Some(state)) => state.opened_encoder.clone(),
            _ => desired_encoder,
        };
        forget_failed_reopen(
            &mut failed_reopen,
            &mut negotiated,
            (hevc_wanted, session_id),
            &desired_encoder,
        );
        let undecodable = session_id.is_some()
            && !client_hevc
            && encoder_state
                .as_ref()
                .is_some_and(|state| is_hevc_encoder(&state.opened_encoder));
        undecodable_retry.frame(undecodable, &mut failed_reopen, Instant::now());

        if let Some(state) = encoder_state.as_mut() {
            if state.update_bitrate_without_reopening(&desired_encoder, desired_bitrate, target_fps)
            {
                failed_reopen = None;
            }
        }
        // A bitrate decrease alone reopens at the pacer's pace; anything else
        // that reopens takes the newest target with it.
        let wanted_bitrate = encoder_state.as_ref().map_or(desired_bitrate, |state| {
            bitrate_pacer.wanted(
                (
                    &state.opened_encoder,
                    state.opened_bitrate,
                    state.opened_fps,
                ),
                (&desired_encoder, desired_bitrate, target_fps),
                Instant::now(),
            )
        });

        // Capture rebuilds at the new size after a display mode change (Radeon
        // Super Resolution, VSR, GPU scaling, a resized virtual display). The
        // open session's frame buffers no longer fit.
        let size_changed = encoder_state
            .as_ref()
            .is_some_and(|state| !state.accepts_frame(raw_frame.width, raw_frame.height));
        if size_changed {
            if let Some(behind) = background.take() {
                info!(
                    encoder = behind.open.encoder(),
                    "The capture size changed while an encoder opened in the background; that session will not be used"
                );
            }
        }
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
            let picture_by =
                *first_picture_by.get_or_insert_with(|| Instant::now() + FIRST_PICTURE_WAIT);
            match open_session(
                &LAUNCH_FAILURES,
                &desired_encoder,
                raw_frame.width,
                raw_frame.height,
                desired_bitrate,
                target_fps,
                &mut input_mode,
                first_open(&desired_encoder, picture_by),
                &mut opening,
            ) {
                Ok(state) => {
                    encoder_state = Some(state);
                    first_picture_by = None;
                    bitrate_pacer.opened(Instant::now());
                    if stall_recovery.opened(&desired_encoder, &preferred)
                        && desired_encoder != "libx265"
                    {
                        PIPELINE_STATS.lock().set_software_fallback(false);
                    }
                    note_encoder_opened(&shared, &desired_encoder);
                }
                // No picture yet, and the iPad would give up waiting for
                // it: libx264 streams while the open goes on.
                Err(e) if is_still_opening(e.as_ref()) => {
                    let open = take_still_opening(e.as_ref()).expect("the open going on");
                    warn!(
                        encoder = %desired_encoder,
                        waited_ms = open.pending.running_for().as_millis() as u64,
                        "The encoder has not opened yet; streaming on libx264 while it opens in the background"
                    );
                    let state = open_session(
                        &LAUNCH_FAILURES,
                        "libx264",
                        raw_frame.width,
                        raw_frame.height,
                        desired_bitrate,
                        target_fps,
                        &mut input_mode,
                        Opening::First,
                        &mut opening,
                    )?;
                    stall_recovery.outwaited(&desired_encoder, "libx264", Instant::now());
                    background = Some(Behind::first(open, desired_bitrate));
                    encoder_state = Some(state);
                    first_picture_by = None;
                    bitrate_pacer.opened(Instant::now());
                    PIPELINE_STATS
                        .lock()
                        .set_software_fallback(encoder_name != "libx264");
                    note_encoder_opened(&shared, "libx264");
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
                    let mut still_opening = None;
                    let (name, state) = open_remaining_encoders(
                        (&desired_encoder, &e.to_string()),
                        crate::gpu::vendors_present,
                        |name| {
                            let opened = open_session(
                                &LAUNCH_FAILURES,
                                name,
                                raw_frame.width,
                                raw_frame.height,
                                desired_bitrate,
                                target_fps,
                                &mut input_mode,
                                first_open(name, picture_by),
                                &mut opening,
                            );
                            if let Err(error) = &opened {
                                if let Some(open) = take_still_opening(error.as_ref()) {
                                    still_opening = Some(open);
                                }
                            }
                            opened
                        },
                    )?;
                    if let Some(open) = still_opening {
                        // The preferred encoder failed for a reason of its
                        // own (a stalled one leaves no lane for another),
                        // and libx264 streams while the next one opens.
                        warn!(
                            encoder = open.encoder(),
                            waited_ms = open.pending.running_for().as_millis() as u64,
                            "The encoder has not opened yet; streaming on libx264 while it opens in the background"
                        );
                        encoder_name = open.encoder().to_string();
                        stall_recovery.opened(name, &preferred);
                        stall_recovery.outwaited(open.encoder(), name, Instant::now());
                        background = Some(Behind::first(open, desired_bitrate));
                    } else {
                        // After a stall the preferred encoder stays preferred,
                        // to be tried again; a real error leaves it for good.
                        if !stall_recovery.failed(stalled, &preferred, name, Instant::now()) {
                            encoder_name = name.to_string();
                        }
                        stall_recovery.opened(name, &preferred);
                    }
                    encoder_state = Some(state);
                    first_picture_by = None;
                    bitrate_pacer.opened(Instant::now());
                    PIPELINE_STATS
                        .lock()
                        .set_software_fallback(name == "libx264");
                    note_encoder_opened(&shared, name);
                }
                Err(e) => return Err(e),
            }
        } else {
            // A reopen this frame: the open that went on in the background,
            // once it returned, or a new one.
            let reopened = if let Some(result) = background.as_mut().and_then(|behind| {
                behind.open.poll(&mut input_mode, |args| {
                    EncoderState::new(
                        &args.encoder_name,
                        args.size.0,
                        args.size.1,
                        args.bitrate_bps,
                        args.target_fps,
                        &mut EncoderInput::Yuv420,
                        Opening::Behind,
                        &mut || {},
                    )
                })
            }) {
                let behind = background.take().expect("it was just polled");
                if let Err(error) = &result {
                    note_open_failure(
                        &LAUNCH_FAILURES,
                        behind.open.encoder(),
                        behind.open.args.size,
                        error.as_ref(),
                    );
                }
                Some((result, behind.reopen))
            } else if background.is_none()
                && (size_changed
                    || encoder_state.as_ref().is_some_and(|state| {
                        reopen_needed(
                            (
                                &state.opened_encoder,
                                state.opened_bitrate,
                                state.opened_fps,
                            ),
                            (&desired_encoder, wanted_bitrate, target_fps),
                            keyframe_reopen,
                            failed_reopen.as_ref(),
                            lane_clear,
                        )
                    }))
            {
                // Bitrate changed (ABR rung or the user's slider), the
                // negotiated codec changed (HEVC toggle / client capability), the
                // capture size changed (display mode change), or AMF owes a
                // keyframe: hardware encoders ignore parameter pokes on an open
                // context, so each means a session reopen (~50-200ms hiccup, no
                // transport teardown, same stream epoch) followed by a forced IDR.
                // So does a try of the preferred encoder after a stalled open.
                // Every reopen takes the newest bitrate target, paced or not.
                if keyframe_reopen {
                    last_keyframe_reopen = Some(Instant::now());
                }
                let (previous, current_encoder, reason) = encoder_state
                    .as_ref()
                    .map(|s| {
                        let reason = reopen_reason(
                            (&s.opened_encoder, s.opened_fps),
                            (&desired_encoder, target_fps),
                            size_changed,
                            keyframe_reopen,
                            stall_recovery
                                .owed
                                .as_ref()
                                .is_some_and(|owed| owed.preferred == desired_encoder),
                        );
                        (s.opened_bitrate, s.opened_encoder.clone(), reason)
                    })
                    .unwrap_or_default();
                stall_recovery.starting(&desired_encoder);
                // The old session cannot take a frame of the new size.
                let how = if size_changed {
                    Opening::First
                } else if opens_behind(&current_encoder, &desired_encoder) {
                    Opening::Behind
                } else {
                    Opening::Reopen
                };
                let reopen = Reopening {
                    encoder: desired_encoder.clone(),
                    from: previous,
                    current: current_encoder,
                    reason,
                    size_changed,
                    keyframe: keyframe_reopen,
                    bitrate: desired_bitrate,
                    fps: target_fps,
                    first: false,
                    in_background: false,
                };
                match open_session(
                    &LAUNCH_FAILURES,
                    &desired_encoder,
                    raw_frame.width,
                    raw_frame.height,
                    desired_bitrate,
                    target_fps,
                    &mut input_mode,
                    how,
                    &mut opening,
                ) {
                    Err(e) if is_still_opening(e.as_ref()) => {
                        let open = take_still_opening(e.as_ref()).expect("the open going on");
                        info!(
                            from = %reopen.current,
                            encoder = %desired_encoder,
                            reason,
                            "Opening the encoder in the background; the stream goes on meanwhile"
                        );
                        background = Some(Behind {
                            open,
                            reopen: Reopening {
                                in_background: true,
                                ..reopen
                            },
                        });
                        None
                    }
                    result => Some((result, reopen)),
                }
            } else {
                None
            };
            match reopened {
                None => {}
                Some((Ok(state), reopen)) => {
                    info!(
                        from = reopen.from,
                        to = state.opened_bitrate,
                        encoder = %reopen.encoder,
                        width = state.frame.width(),
                        height = state.frame.height(),
                        size_changed = reopen.size_changed,
                        keyframe = reopen.keyframe,
                        reason = reopen.reason,
                        in_background = reopen.in_background,
                        "Reopened encoder session"
                    );
                    encoder_state = Some(state);
                    bitrate_pacer.opened(Instant::now());
                    failed_reopen = None;
                    pending_keyframe = true;
                    // Back from libx264 to hardware (libx265 only replaces a
                    // libx264 that was already preferred).
                    if stall_recovery.opened(&reopen.encoder, &preferred)
                        && reopen.encoder != "libx265"
                    {
                        PIPELINE_STATS.lock().set_software_fallback(false);
                    }
                    note_encoder_opened(&shared, &reopen.encoder);
                }
                // The old session cannot take a frame of the new size. A
                // pipeline restart opens a fresh encoder, with the software
                // fallback if the hardware one rejects this size.
                Some((Err(e), reopen)) if reopen.size_changed => {
                    return Err(format!(
                        "{} could not reopen at {}x{}: {e}",
                        reopen.encoder, raw_frame.width, raw_frame.height
                    )
                    .into());
                }
                // The pipeline's first open, which went on in the background.
                Some((Err(e), reopen)) if reopen.first => {
                    if is_open_stall(e.as_ref()) {
                        // Tried again once the lane is clear, as after any
                        // stalled open (see `StallRecovery::outwaited`).
                        warn!(
                            error = %e,
                            encoder = %reopen.encoder,
                            "The encoder did not open in the background; streaming on libx264 until it is tried again"
                        );
                    } else {
                        warn!(
                            error = %e,
                            encoder = %reopen.encoder,
                            "The encoder failed to open in the background; staying on libx264 for this pipeline"
                        );
                        stall_recovery.owed = None;
                        if is_hevc_encoder(&reopen.encoder) {
                            hevc_open_failed = true;
                        } else {
                            encoder_name = "libx264".to_string();
                        }
                    }
                }
                Some((Err(e), reopen)) => {
                    let stalled = is_open_stall(e.as_ref());
                    if stalled {
                        warn!(
                            error = %e,
                            encoder = %reopen.encoder,
                            "Encoder reopen stalled — keeping the current session"
                        );
                    } else if is_hevc_encoder(&reopen.encoder) {
                        warn!(
                            error = %e,
                            encoder = %reopen.encoder,
                            "HEVC reopen failed — keeping the current session"
                        );
                    } else {
                        warn!(
                            error = %e,
                            bitrate = reopen.bitrate,
                            "Encoder reopen failed — keeping the current session"
                        );
                    }
                    // A stall is tried again once the lane is clear, while
                    // tries are left.
                    if !stall_recovery.failed(stalled, &preferred, &reopen.current, Instant::now())
                    {
                        give_up_reopen(
                            &reopen.encoder,
                            &reopen.current,
                            stall_recovery.owed.is_some(),
                            &mut encoder_name,
                            &mut hevc_open_failed,
                        );
                        failed_reopen = Some((reopen.encoder.clone(), reopen.bitrate, reopen.fps));
                    }
                }
            }
        }

        let encoder = encoder_state
            .as_mut()
            .expect("Encoder state must be initialized");
        let current_bitrate = encoder.opened_bitrate;
        let opened_is_hevc = is_hevc_encoder(&encoder.opened_encoder);

        let convert_start = Instant::now();
        encoder.convert(&raw_frame.data, (raw_frame.width * 4) as usize)?;
        // The picture is kept only while a session's first keyframe may stay
        // inside the encoder, to be given to it again.
        let picture = (holds_last_frame(&encoder.opened_encoder)
            && (encoder.frames_submitted == 0 || owed_keyframe.is_some()))
        .then(|| raw_frame.clone());
        // Conversion finished synchronously. Recycle capture's buffer before
        // the codec or sender can block, including on the direct macOS path.
        drop(raw_frame.data);
        PIPELINE_STATS
            .lock()
            .record_convert(convert_start.elapsed().as_micros());
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
        // A bitrate Quick Sync took in place applies with this frame: an
        // error now is its refusal.
        let mut reset_refused = None;
        if let Err(error) = encoder.send_frame(pts, raw_frame.timestamp, raw_frame.frame_number) {
            if !encoder.bitrate_reset_unconfirmed {
                return Err(error.into());
            }
            reset_refused = Some(error);
        }

        let mut keyframe_out = false;
        while reset_refused.is_none() {
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
                    keyframe_out |= is_keyframe;
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
                Err(e) if encoder.bitrate_reset_unconfirmed => reset_refused = Some(e),
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(error) = reset_refused {
            QSV_RESET_REFUSED.store(true, Ordering::Relaxed);
            warn!(
                %error,
                encoder = %encoder.opened_encoder,
                bitrate = encoder.opened_bitrate,
                "Quick Sync did not take the new bitrate on the open session; reopening it, as for its bitrate changes from now on"
            );
            encoder_state = None;
            pending_keyframe = true;
            // A kept picture belonged to that session; the new one starts
            // with an IDR.
            owed_keyframe = None;
            continue;
        }
        encoder.bitrate_reset_unconfirmed = false;
        owed_keyframe =
            OwedKeyframe::after_encode(owed_keyframe.take(), picture, keyframe_out, encode_start);
    }

    info!("Capture channel closed, encoder shutting down");
    Ok(())
}

struct EncoderState {
    #[cfg(target_os = "macos")]
    native_converter: Option<vimage::Converter>,
    encoder: ffmpeg_next::codec::encoder::video::Encoder,
    /// BGRA to the encoder's YUV input, reading capture's buffer directly.
    scaler: Option<input::Scaler>,
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
    /// A Quick Sync bitrate change was made in place, and no frame has gone
    /// through the session since. Quick Sync applies it with the next frame,
    /// so an error there means it refused the change.
    bitrate_reset_unconfirmed: bool,
}

/// Set once a Quick Sync session refused a bitrate change in place: from
/// then on, this launch, its bitrate changes reopen it, as before.
static QSV_RESET_REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Encoders that take a new bitrate on the open session, without the reopen
/// that costs a stutter and an IDR. FFmpeg 7.1's libx264 wrapper applies
/// bitrate and VBV changes through x264_encoder_reconfig. Its Quick Sync
/// encoders compare `bit_rate` with the value they opened at before each
/// frame, and reset the session's rate control to it (qsvenc.c,
/// `update_bitrate` and `MFXVideoENCODE_Reset`), with no new sequence. A
/// Quick Sync open takes 630 ms at the median in the field, against 144 ms
/// for NVENC and 43 ms for AMF, so each bitrate reopen froze the picture
/// that long. Unless `qsv_refused`: a session refused the change once.
fn takes_bitrate_in_place(encoder_name: &str, qsv_refused: bool) -> bool {
    encoder_name == "libx264" || (encoder_name.ends_with("_qsv") && !qsv_refused)
}

impl EncoderState {
    fn update_bitrate_without_reopening(
        &mut self,
        desired_encoder: &str,
        bitrate_bps: u32,
        target_fps: u32,
    ) -> bool {
        if !takes_bitrate_in_place(
            &self.opened_encoder,
            QSV_RESET_REFUSED.load(Ordering::Relaxed),
        ) || desired_encoder != self.opened_encoder
            || target_fps != self.opened_fps
            || bitrate_bps == self.opened_bitrate
        {
            return false;
        }
        // Preserve the encoder's reference frames, PTS epoch and buffers
        // instead of allocating a second encoder for every ABR step. Other
        // encoders retain the existing reopen path.
        self.encoder.set_bit_rate(bitrate_bps.max(500_000) as usize);
        if self.opened_encoder == "libx264" {
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
        } else {
            // Quick Sync opened in VBR with no maximum rate or buffer size,
            // so only the target changes.
            self.bitrate_reset_unconfirmed = true;
            info!(
                from = self.opened_bitrate,
                to = bitrate_bps,
                encoder = %self.opened_encoder,
                "Updated hardware encoder bitrate without reopening"
            );
        }
        self.opened_bitrate = bitrate_bps;
        true
    }

    /// Opens a session for captures of `capture_width`x`capture_height`,
    /// at their [`encoded_size`].
    #[allow(clippy::too_many_arguments)]
    fn new(
        encoder_name: &str,
        capture_width: u32,
        capture_height: u32,
        bitrate_bps: u32,
        target_fps: u32,
        input_mode: &mut EncoderInput,
        opening: Opening,
        progress: &mut dyn FnMut(),
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (width, height) = encoded_size(capture_width, capture_height);
        info!(encoder = encoder_name, width, height, bitrate_bps, target_fps,
            input = ?input_mode, "Opening encoder session");
        if (width, height) != (capture_width, capture_height) {
            info!(
                capture_size = %format_args!("{capture_width}x{capture_height}"),
                encoded_size = %format_args!("{width}x{height}"),
                "Encoding without the capture's odd last column or row"
            );
        }
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
        let opened = open_at_input(input_mode, encoder_name, &supported, |pixel_format| {
            Self::open_at(
                codec,
                encoder_name,
                (width, height),
                bitrate_bps,
                target_fps,
                pixel_format,
                opening,
                &mut *progress,
            )
        });
        // One that goes on in the background is tried at YUV420 there.
        if let Err(error) = &opened {
            if let Some(still) = error.downcast_ref::<StillOpening>() {
                if let Some(open) = still.1.lock().as_mut() {
                    open.yuv_retry = input::retries_as_yuv(*input_mode, open.args.pixel_format);
                }
            }
        }
        opened
    }

    #[allow(clippy::too_many_arguments)]
    fn open_at(
        codec: ffmpeg_next::Codec,
        encoder_name: &str,
        (width, height): (u32, u32),
        bitrate_bps: u32,
        target_fps: u32,
        pixel_format: ffmpeg_next::format::Pixel,
        opening: Opening,
        progress: &mut dyn FnMut(),
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let legacy_pts = std::env::var("ETERNAL_LEGACY_PTS").is_ok_and(|v| v.trim() == "1");
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

        let args = SessionArgs {
            encoder_name: encoder_name.to_string(),
            size: (width, height),
            bitrate_bps,
            target_fps,
            pixel_format,
            legacy_pts,
        };
        match open_encoder(encoder, encoder_name, opening, progress) {
            Opened::Now(result) => Self::opened(result, &args),
            Opened::Behind(pending) => Err(Box::new(StillOpening(
                encoder_name.to_string(),
                parking_lot::Mutex::new(Some(BackgroundOpen {
                    pending,
                    args,
                    yuv_retry: false,
                })),
            ))),
        }
    }

    /// The session from an open that returned with `result`, or its error.
    fn opened(
        result: OpenResult,
        args: &SessionArgs,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let SessionArgs {
            ref encoder_name,
            size: (width, height),
            bitrate_bps,
            target_fps,
            pixel_format,
            legacy_pts,
        } = *args;
        let encoder_name = encoder_name.as_str();
        let encoder = match result {
            Ok(Ok(encoder)) => encoder,
            Ok(Err(error)) => {
                note_nvidia_driver(encoder_name, error, crate::gpu::nvidia_driver_versions);
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

        #[cfg(target_os = "macos")]
        let native_converter = if pixel_format == ffmpeg_next::format::Pixel::YUV420P {
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
        };
        // Where Accelerate converts, swscale is only its fallback, so it
        // keeps no threads waiting.
        #[cfg(target_os = "macos")]
        let threads = if native_converter.is_some() {
            1
        } else {
            input::scaler_threads()
        };
        #[cfg(not(target_os = "macos"))]
        let threads = input::scaler_threads();
        let scaler = input::conversion_context(pixel_format, width, height, threads)?;
        if let Some(scaler) = &scaler {
            info!(
                output = ?pixel_format,
                threads = scaler.threads(),
                "swscale BGRA conversion context created"
            );
        }

        Ok(Self {
            #[cfg(target_os = "macos")]
            native_converter,
            encoder,
            scaler,
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
            bitrate_reset_unconfirmed: false,
        })
    }

    fn next_pts(&mut self, captured: Instant, frame_number: u64) -> i64 {
        let pts = if self.legacy_pts {
            frame_number as i64
        } else {
            let epoch = *self.pts_epoch.get_or_insert(captured);
            captured.saturating_duration_since(epoch).as_micros() as i64
        };
        // Strict monotonicity (keepalive resends, a picture given again with
        // its capture number and time, and clock quirks).
        let pts = pts.max(self.last_pts + 1);
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

    /// Converts a capture whose rows lie `row_bytes` apart into `self.frame`:
    /// its top-left `self.frame`-sized part, pixel for pixel, which leaves an
    /// odd last column or row out (see [`encoded_size`]).
    fn convert(&mut self, rows: &[u8], row_bytes: usize) -> Result<(), ffmpeg_next::Error> {
        let (width, height) = (self.frame.width(), self.frame.height());
        #[cfg(target_os = "macos")]
        if let Some(converter) = self.native_converter.as_ref() {
            match converter.convert(rows, width, height, row_bytes, &mut self.frame) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    warn!(%error, "Using swscale after native color conversion failed");
                    self.native_converter = None;
                }
            }
        }
        if let Some(scaler) = self.scaler.as_mut() {
            return input::convert_packed_bgra(scaler, rows, row_bytes, &mut self.frame);
        }
        // BGRA input: the encoder converts. Its frame takes a copy,
        // so capture's buffer is free before the encoder can block.
        let picture_bytes = width as usize * 4;
        let stride = self.frame.stride(0);
        let bgra_plane = self.frame.data_mut(0);
        for y in 0..height as usize {
            bgra_plane[y * stride..][..picture_bytes]
                .copy_from_slice(&rows[y * row_bytes..][..picture_bytes]);
        }
        Ok(())
    }

    fn accepts_frame(&self, width: u32, height: u32) -> bool {
        encoded_size(width, height) == (self.frame.width(), self.frame.height())
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

/// What streams while an encoder session opens, which sets its time limit
/// and how long it is waited for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// Nothing: the pipeline's last resort, libx264, or a reopen at a new
    /// capture size, which the old session cannot encode.
    First,
    /// As `First`, with another encoder to try if this one fails: every
    /// first open but libx264's. FFmpeg's errors while it opens are logged
    /// as warnings (see [`crate::diagnostics::while_probing`]). One in the
    /// lane is waited for only until the pipeline's first picture is due,
    /// at the instant given (see [`FIRST_PICTURE_WAIT`]): then it goes on
    /// opening in the background while libx264 streams. The instant holds
    /// whatever came before the open: a fallback's earlier tries, a BGRA
    /// open retried at YUV420, a call's thread that was slow to start.
    Probe(Instant),
    /// The live session, which goes on if the reopen is given up on. The
    /// picture freezes until then, so the limit is shorter.
    Reopen,
    /// A reopen in the lane while a session that needs none streams
    /// (libx264): it opens in the background, and the stream goes on
    /// meanwhile, so it has a first open's limits (see [`opens_behind`]).
    Behind,
}

/// How long a pipeline may go without a picture, from its first try of an
/// encoder in the lane. The iPad gives up on a session after 10 s without
/// one. A hardware open that hung was waited for up to its 15 s limit, so
/// libx264 took over too late, and the iPad showed no picture (field
/// triage, Nov 2026). Past this, libx264 streams while the open goes on in
/// the background, and the stream switches to it if it opens within its
/// limit. Above the slowest NVENC and AMF opens in the field (2.6 s and
/// 1.2 s) and Quick Sync's p99 (3.2 s): the few slower Quick Sync opens
/// start on libx264 for a moment.
const FIRST_PICTURE_WAIT: Duration = Duration::from_secs(3);

/// An encoder open that returned, or one that goes on in the background.
enum Opened {
    Now(OpenResult),
    Behind(PendingOpen),
}

type PendingOpen =
    crate::stall::Pending<Result<ffmpeg_next::codec::encoder::video::Encoder, ffmpeg_next::Error>>;

/// Open `encoder` with its options. An encoder that opens under FFmpeg's
/// process-wide lock opens under its time limit (see [`crate::stall`]); one
/// that returns after it is closed on the call's thread.
fn open_encoder(
    encoder: ffmpeg_next::codec::encoder::video::Video,
    encoder_name: &str,
    opening: Opening,
    progress: &mut dyn FnMut(),
) -> Opened {
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
    mut progress: &mut dyn FnMut(),
) -> Opened {
    let name = encoder_name.to_string();
    let probing = matches!(opening, Opening::Probe(_));
    // On the thread that makes the call, where FFmpeg logs.
    let open = move || {
        let open = || encoder.open_with(encoder_options(&name));
        if probing {
            crate::diagnostics::while_probing(open)
        } else {
            open()
        }
    };
    if !takes_codec_lock(encoder_name) {
        return Opened::Now(Ok(open()));
    }
    let limits = open_limits(encoder_name, opening);
    match crate::stall::start_on(lanes, crate::stall::Call::EncoderOpen, limits, open) {
        Ok(pending) => match wait_or_park(pending, opening, &mut progress) {
            Ok(result) => Opened::Now(result),
            Err(pending) => Opened::Behind(pending),
        },
        Err(stall) => Opened::Now(Err(stall)),
    }
}

/// Waits for an open in the lane as `opening` says: to its limit, until the
/// first picture is due, or not at all. Err: it goes on, parked.
fn wait_or_park<T, N: Fn() -> Instant>(
    mut pending: crate::stall::Pending<T, N>,
    opening: Opening,
    progress: &mut impl FnMut(),
) -> Result<Result<T, crate::stall::Stall>, crate::stall::Pending<T, N>> {
    let waited = match opening {
        Opening::Probe(by) => pending.wait_until(by, progress),
        Opening::Behind => None,
        Opening::First | Opening::Reopen => pending.wait_up_to(Duration::MAX, progress),
    };
    if let Some(result) = waited {
        return Ok(result);
    }
    match pending.park() {
        Some(result) => Ok(result),
        None => Err(pending),
    }
}

/// A pipeline's first open of `encoder_name`, whose first picture is due
/// `by`: anything but libx264 has another encoder after it.
fn first_open(encoder_name: &str, by: Instant) -> Opening {
    if encoder_name == "libx264" {
        Opening::First
    } else {
        Opening::Probe(by)
    }
}

fn open_limits(encoder_name: &str, opening: Opening) -> crate::stall::Limits {
    match opening {
        Opening::First | Opening::Probe(_) | Opening::Behind => {
            crate::stall::encoder_open_limits(encoder_name)
        }
        Opening::Reopen => crate::stall::encoder_reopen_limits(encoder_name),
    }
}

/// The encoder a frame prefers: the H.265 encoder for `encoder_name` when
/// HEVC is wanted (the user's preference and the client's decoding) and has
/// not failed, if FFmpeg has it; else `encoder_name`. `warned` says once
/// that there is none.
fn preferred_encoder(
    encoder_name: &str,
    hevc_wanted: bool,
    hevc_open_failed: bool,
    warned: &mut bool,
) -> String {
    if !hevc_wanted || hevc_open_failed {
        return encoder_name.to_string();
    }
    match hevc_encoder_for(encoder_name) {
        Some(hevc) if ffmpeg_next::encoder::find_by_name(hevc).is_some() => hevc.to_string(),
        _ => {
            if !*warned {
                *warned = true;
                warn!(
                    encoder = %encoder_name,
                    "HEVC requested but no H.265 encoder is available — staying on H.264"
                );
            }
            encoder_name.to_string()
        }
    }
}

/// Drops an HEVC open going on in the background once the frame prefers
/// H.264 (HEVC turned off, or a client that decodes only H.264): adopted,
/// it would replace a session the client can decode with one it may not.
/// The session that streams meanwhile goes on. True when one was dropped;
/// its result is released when it returns.
fn drop_obsolete_background(background: &mut Option<Behind>, preferred: &str) -> bool {
    let obsolete = background.as_ref().is_some_and(|behind| {
        is_hevc_encoder(behind.open.encoder()) && !is_hevc_encoder(preferred)
    });
    if let Some(behind) = background.take_if(|_| obsolete) {
        info!(
            encoder = behind.open.encoder(),
            preferred,
            "The codec changed while an encoder opened in the background; that session will not be used"
        );
    }
    obsolete
}

/// Whether a reopen from the `current` session to `wanted` opens in the
/// background: libx264 needs no lane, so it streams on while an encoder in
/// the lane opens, instead of freezing for as long as the open takes.
fn opens_behind(current: &str, wanted: &str) -> bool {
    !takes_codec_lock(current) && takes_codec_lock(wanted)
}

/// What a session opens with, to finish an open that returns later.
struct SessionArgs {
    encoder_name: String,
    size: (u32, u32),
    bitrate_bps: u32,
    target_fps: u32,
    pixel_format: ffmpeg_next::format::Pixel,
    legacy_pts: bool,
}

/// An encoder session that opens in the background while libx264 streams.
struct BackgroundOpen {
    pending: PendingOpen,
    args: SessionArgs,
    /// An Automatic BGRA open: if it fails, YUV420 is tried, as
    /// [`open_at_input`] tries it for an open that fails while waited for.
    yuv_retry: bool,
}

impl BackgroundOpen {
    fn encoder(&self) -> &str {
        &self.args.encoder_name
    }

    /// The session once its open returned, or the open's error; past its
    /// limit, its stall. None while it opens within its limit. A BGRA open
    /// that failed is tried again at YUV420 with `retry_yuv`, and `input`
    /// keeps YUV420 for the pipeline's later opens: the error comes back only
    /// if that fails too, so an encoder that works at YUV420 is not left for
    /// the launch.
    fn poll(
        &mut self,
        input: &mut EncoderInput,
        retry_yuv: impl FnOnce(
            &SessionArgs,
        )
            -> Result<EncoderState, Box<dyn std::error::Error + Send + Sync>>,
    ) -> Option<Result<EncoderState, Box<dyn std::error::Error + Send + Sync>>> {
        let result = self.pending.poll()?;
        let opened = EncoderState::opened(result, &self.args);
        let Err(error) = &opened else {
            return Some(opened);
        };
        if !std::mem::take(&mut self.yuv_retry) || is_open_stall(error.as_ref()) {
            return Some(opened);
        }
        warn!(
            encoder = %self.args.encoder_name,
            %error,
            "Encoder did not open with BGRA input; using YUV420 for this stream"
        );
        *input = EncoderInput::Yuv420;
        match retry_yuv(&self.args) {
            Err(error) if is_still_opening(error.as_ref()) => {
                *self = take_still_opening(error.as_ref()).expect("the open going on");
                None
            }
            retried => Some(retried),
        }
    }
}

/// An open in the lane that had not returned when its wait ended and goes
/// on in the background, carried out of [`EncoderState::new`] to the encode
/// loop, which streams on libx264 meanwhile.
struct StillOpening(String, parking_lot::Mutex<Option<BackgroundOpen>>);

impl std::fmt::Debug for StillOpening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StillOpening").field(&self.0).finish()
    }
}

impl std::fmt::Display for StillOpening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} has not opened yet", self.0)
    }
}

impl std::error::Error for StillOpening {}

fn is_still_opening(error: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    error.is::<StillOpening>()
}

/// The open that goes on in the background, from the error that carries it.
fn take_still_opening(
    error: &(dyn std::error::Error + Send + Sync + 'static),
) -> Option<BackgroundOpen> {
    error.downcast_ref::<StillOpening>()?.1.lock().take()
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

/// `vendors_present` lists the graphics cards' vendors when the fallback
/// runs (see [`crate::gpu::vendors_present`]). The encoders of the vendors
/// listed are tried first, so a PC with a working one never probes the
/// others: AMF on a PC without an AMD card only logs that its DLL failed to
/// load. Every other hardware encoder is still tried after them, because
/// Windows does not list every card that can encode (an NVIDIA card in TCC
/// mode). A hybrid laptop lists both of its cards; an unknown vendor moves
/// no encoder. With no list, the order is NVENC, AMF, Quick Sync, as before.
///
/// `failed` is the encoder that failed first, and why. If every encoder
/// fails, one error names each and its reason: their FFmpeg errors were
/// warnings while they were tried.
fn open_remaining_encoders<T>(
    (failed, why): (&str, &str),
    vendors_present: impl FnOnce() -> Option<Vec<GpuVendor>>,
    mut open: impl FnMut(&str) -> Result<T, Box<dyn std::error::Error + Send + Sync>>,
) -> Result<(&'static str, T), Box<dyn std::error::Error + Send + Sync>> {
    // Open errors already start with the encoder's name.
    let mut reasons = vec![why.to_string()];
    let note = |reasons: &mut Vec<String>, name: &str, reason: String| {
        reasons.push(if reason.starts_with(name) {
            reason
        } else {
            format!("{name}: {reason}")
        });
    };
    let mut hardware = if matches!(failed, "h264_nvenc" | "h264_amf" | "h264_qsv") {
        vec![
            ("h264_nvenc", GpuVendor::Nvidia),
            ("h264_amf", GpuVendor::Amd),
            ("h264_qsv", GpuVendor::Intel),
        ]
    } else {
        Vec::new()
    };
    let vendors = if hardware.is_empty() {
        None
    } else {
        vendors_present()
    };
    if let Some(vendors) = &vendors {
        // A stable sort: each group keeps NVENC, AMF, Quick Sync.
        hardware.sort_by_key(|(_, vendor)| !vendors.contains(vendor));
    }
    for (name, vendor) in hardware {
        if name == failed {
            continue;
        }
        if vendors
            .as_ref()
            .is_some_and(|vendors| !vendors.contains(&vendor))
        {
            info!(
                encoder = name,
                %vendor,
                "No graphics card from this vendor is listed; trying its encoder after the listed ones"
            );
        }
        match open(name) {
            Ok(state) => return Ok((name, state)),
            // Every other open in the lane would wait behind it.
            Err(error) if is_still_opening(error.as_ref()) => {
                info!(
                    encoder = name,
                    "Hardware encoder is still opening; libx264 streams while it opens in the background"
                );
                break;
            }
            Err(error) => {
                warn!(encoder = name, %error, "Hardware encoder did not open");
                note(&mut reasons, name, error.to_string());
            }
        }
    }
    warn!("No hardware encoder opened; falling back to libx264 (software)");
    open("libx264")
        .map(|state| ("libx264", state))
        .inspect_err(|error| {
            note(&mut reasons, "libx264", error.to_string());
            error!(encoders = %reasons.join(" | "), "No encoder could open");
        })
}

/// The oldest NVIDIA driver the bundled FFmpeg 7.1.1's NVENC opens with. Its
/// NVENC headers are nv-codec-headers 13.0.19 (Video Codec SDK 13.0), whose
/// README requires driver 570.0 or newer on Windows. Older drivers fail with
/// "Driver does not support the required nvenc API version", and FFmpeg 7.1
/// prints the minimum as "(unknown)".
pub const NVENC_MIN_DRIVER: &str = "570";
/// [`NVENC_MIN_DRIVER`] in hundredths, as [`nvidia_release`] returns it.
const NVENC_MIN_RELEASE: u32 = 57_000;

/// NVIDIA's driver number in hundredths (56607 for 566.07) from the Windows
/// driver version packed as DXGI reports it (32.0.15.6607): the last digit
/// of the third part followed by the four digits of the fourth.
fn nvidia_release(windows_version: u64) -> u32 {
    let third = ((windows_version >> 16) & 0xffff) as u32;
    let fourth = (windows_version & 0xffff) as u32;
    (third % 10) * 10_000 + fourth
}

/// The newest NVIDIA driver on the PC when an NVENC open failed because it is
/// too old: FFmpeg returned ENOSYS, and every NVIDIA driver is older than
/// NVENC needs. ENOSYS alone is not enough: FFmpeg also returns it for
/// parameters the encoder does not support. `driver_versions` (the NVIDIA
/// adapters' Windows driver versions) are read only then; none known means
/// not outdated.
fn outdated_nvidia_driver(
    encoder_name: &str,
    error: ffmpeg_next::Error,
    driver_versions: impl FnOnce() -> Vec<u64>,
) -> Option<u32> {
    if !encoder_name.ends_with("_nvenc")
        || !matches!(error, ffmpeg_next::Error::Other { errno } if errno == libc::ENOSYS)
    {
        return None;
    }
    let newest = driver_versions().into_iter().map(nvidia_release).max()?;
    (newest < NVENC_MIN_RELEASE).then_some(newest)
}

/// Says once, in the log and on the Stream page's fallback notice, that
/// NVENC refused an NVIDIA driver older than it needs.
fn note_nvidia_driver(
    encoder_name: &str,
    error: ffmpeg_next::Error,
    driver_versions: impl FnOnce() -> Vec<u64>,
) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some(driver) = outdated_nvidia_driver(encoder_name, error, driver_versions) else {
        return;
    };
    if !LOGGED.swap(true, Ordering::Relaxed) {
        warn!(
            driver = %format!("{}.{:02}", driver / 100, driver % 100),
            minimum = NVENC_MIN_DRIVER,
            "The NVIDIA driver is too old for the graphics card to encode video; video is encoded on the processor unless another graphics card can, and updating the NVIDIA driver enables hardware encoding"
        );
    }
    PIPELINE_STATS.lock().nvidia_driver_outdated = true;
}

/// Hardware encoders that failed to open this launch, each at the encoded
/// size it failed at. Each is not tried again at that size, so its FFmpeg
/// errors are logged once instead of at every pipeline start and reopen
/// (field triage, Nov 2026: 13-44 lines per PC, over and over, for an NVENC
/// API too old, an AMF component or a Quick Sync session that could not be
/// created). Forgotten when the user clicks Restart, as the fallback notice
/// asks after a driver update. A stall is not a failure: the driver did not
/// answer in time, which says nothing about the encoder.
#[derive(Default)]
pub(crate) struct LaunchFailures {
    opens: Vec<(String, (u32, u32))>,
    /// AMD and Intel cards whose encoder failed to open: the fallback
    /// notice asks for their driver update.
    vendors: Vec<GpuVendor>,
}

static LAUNCH_FAILURES: parking_lot::Mutex<LaunchFailures> =
    parking_lot::Mutex::new(LaunchFailures {
        opens: Vec::new(),
        vendors: Vec::new(),
    });

impl LaunchFailures {
    /// Why `encoder` is not opened at `size`: it failed there this launch.
    fn earlier(&self, encoder: &str, size: (u32, u32)) -> Option<String> {
        self.opens
            .iter()
            .any(|(name, at)| name == encoder && *at == size)
            .then(|| {
                format!(
                    "{encoder} failed to open at {}x{} earlier in this launch; not tried again until Restart",
                    size.0, size.1
                )
            })
    }

    /// Whether an encoder named with `suffix` failed this launch.
    fn any(&self, suffix: &str) -> bool {
        self.opens.iter().any(|(name, _)| name.ends_with(suffix))
    }

    /// `encoder` failed to open at `size`, for a reason of its own. When it
    /// is AMD's or Intel's, and `vendors_present` (read only then) lists
    /// that vendor's card, that card's driver could not start it.
    fn failed(
        &mut self,
        encoder: &str,
        size: (u32, u32),
        vendors_present: impl FnOnce() -> Option<Vec<GpuVendor>>,
    ) {
        if !is_hardware_encoder(encoder) || self.earlier(encoder, size).is_some() {
            return;
        }
        self.opens.push((encoder.to_string(), size));
        let vendor = if encoder.ends_with("_amf") {
            GpuVendor::Amd
        } else if encoder.ends_with("_qsv") {
            GpuVendor::Intel
        } else {
            return;
        };
        if self.vendors.contains(&vendor)
            || !vendors_present().is_some_and(|present| present.contains(&vendor))
        {
            return;
        }
        warn!(
            %vendor,
            encoder,
            "The graphics driver could not start video encoding on this card; video is encoded on the processor unless another graphics card can, and updating the driver may enable hardware encoding"
        );
        self.vendors.push(vendor);
    }
}

fn is_hardware_encoder(encoder_name: &str) -> bool {
    ["_nvenc", "_amf", "_qsv"]
        .iter()
        .any(|suffix| encoder_name.ends_with(suffix))
}

/// Forget this launch's failed opens and their driver notes, so the next
/// pipeline tries every encoder again: the user clicked Restart, maybe
/// after a driver update.
pub fn forget_launch_failures() {
    let forgotten = std::mem::take(&mut *LAUNCH_FAILURES.lock());
    if !forgotten.opens.is_empty() {
        info!(
            encoders = forgotten.opens.len(),
            "Trying the encoders that failed to open again"
        );
    }
}

/// Opens a session as [`EncoderState::new`] does, unless `encoder_name`
/// failed to open at this size earlier in this launch, and remembers a
/// hardware encoder's own failure in `failures`.
#[allow(clippy::too_many_arguments)]
fn open_session(
    failures: &parking_lot::Mutex<LaunchFailures>,
    encoder_name: &str,
    capture_width: u32,
    capture_height: u32,
    bitrate_bps: u32,
    target_fps: u32,
    input_mode: &mut EncoderInput,
    opening: Opening,
    progress: &mut dyn FnMut(),
) -> Result<EncoderState, Box<dyn std::error::Error + Send + Sync>> {
    let size = encoded_size(capture_width, capture_height);
    if let Some(why) = failures.lock().earlier(encoder_name, size) {
        return Err(why.into());
    }
    let opened = EncoderState::new(
        encoder_name,
        capture_width,
        capture_height,
        bitrate_bps,
        target_fps,
        input_mode,
        opening,
        progress,
    );
    if let Err(error) = &opened {
        note_open_failure(failures, encoder_name, size, error.as_ref());
    }
    opened
}

/// Remembers a hardware encoder's own failure to open at `size`, and shows
/// its driver note on the Stream page.
fn note_open_failure(
    failures: &parking_lot::Mutex<LaunchFailures>,
    encoder_name: &str,
    size: (u32, u32),
    error: &(dyn std::error::Error + Send + Sync + 'static),
) {
    if is_open_stall(error) {
        return;
    }
    let vendors = {
        let mut failures = failures.lock();
        failures.failed(encoder_name, size, crate::gpu::vendors_present);
        failures.vendors.clone()
    };
    PIPELINE_STATS.lock().driver_update_vendors = vendors;
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

/// An open given up on or refused, or one still going on in the background:
/// neither says the encoder does not work.
fn is_open_stall(error: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    error.downcast_ref::<OpenStalled>().is_some() || is_still_opening(error)
}

/// Opens at the pixel format `input` picks from `supported`. When nothing
/// was picked and an NVIDIA encoder's BGRA open fails, YUV420 is tried at
/// once, and `input` keeps it for this pipeline's later opens, as a failed
/// encoder stays failed. A stalled open is not a failure of BGRA, and is
/// not tried again here.
fn open_at_input<T>(
    input: &mut EncoderInput,
    encoder_name: &str,
    supported: &[ffmpeg_next::format::Pixel],
    mut open: impl FnMut(
        ffmpeg_next::format::Pixel,
    ) -> Result<T, Box<dyn std::error::Error + Send + Sync>>,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    let pixel_format = select_input(*input, encoder_name, supported)?;
    match open(pixel_format) {
        Err(error)
            if input::retries_as_yuv(*input, pixel_format) && !is_open_stall(error.as_ref()) =>
        {
            warn!(
                encoder = encoder_name,
                %error,
                "Encoder did not open with BGRA input; using YUV420 for this stream"
            );
            *input = EncoderInput::Yuv420;
            open(select_input(*input, encoder_name, supported)?)
        }
        result => result,
    }
}

/// A reopen, for its log line and for what follows if it fails.
struct Reopening {
    encoder: String,
    /// The bitrate of the session it replaces.
    from: u32,
    /// The session that streams until it opens.
    current: String,
    reason: &'static str,
    size_changed: bool,
    keyframe: bool,
    bitrate: u32,
    fps: u32,
    /// The pipeline's first open, which went on past its first picture's
    /// wait (see [`FIRST_PICTURE_WAIT`]).
    first: bool,
    in_background: bool,
}

/// An open going on in the background, and the reopen it makes once it
/// returns.
struct Behind {
    open: BackgroundOpen,
    reopen: Reopening,
}

impl Behind {
    /// The pipeline's first open of `open`'s encoder, while libx264 streams
    /// at `bitrate`.
    fn first(open: BackgroundOpen, bitrate: u32) -> Self {
        let reopen = Reopening {
            encoder: open.encoder().to_string(),
            from: bitrate,
            current: "libx264".to_string(),
            reason: "recovery",
            size_changed: false,
            keyframe: false,
            bitrate: open.args.bitrate_bps,
            fps: open.args.target_fps,
            first: true,
            in_background: true,
        };
        Self { open, reopen }
    }
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

    /// The pipeline's first open of `preferred` had not returned when its
    /// first picture was due, and goes on in the background while `using`
    /// streams. If it opens, [`Self::opened`] ends the recovery. If it is
    /// given up on at its limit, the first try is due once the lane is
    /// clear, as after an open that stalled while the stream waited.
    fn outwaited(&mut self, preferred: &str, using: &str, now: Instant) {
        self.trying = false;
        self.owed = Some(Owed {
            preferred: preferred.to_string(),
            using: using.to_string(),
            next_try: now + STALL_RETRY_WAITS[0],
        });
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

/// How long after the session opened a bitrate decrease may wait, so that a
/// burst of decreases (slider moves, a ceiling clamp then an ABR step, an
/// ABR step just after an AMF keyframe reopen) reopens once.
const BITRATE_DECREASE_GAP: Duration = Duration::from_secs(1);

/// How long after a Quick Sync session opened a bitrate change alone may
/// wait, up or down, once Quick Sync refused to take one in place (see
/// [`takes_bitrate_in_place`]): each of its reopens freezes the picture
/// for about 0.6 s, and one PC in the field reopened 148 times.
const QSV_BITRATE_REOPEN_GAP: Duration = Duration::from_secs(5);

/// Paces reopens for a bitrate change alone. Hardware encoders (and
/// libx265) take a new bitrate only by reopening, and each reopen costs an
/// IDR and a visible stutter. A decrease takes effect at once, or 1 s after
/// the session opened if that is later, so the stream never overruns a
/// congested link more than 1 s longer than before, and a decrease right
/// after another reopen does not make a second one at once. An increase
/// takes effect at once, as do codec, frame rate, size, keyframe and
/// recovery reopens, and every reopen opens at the newest target. Quick
/// Sync, whose reopens are slowest, waits 5 s for either.
#[derive(Default)]
struct BitratePacer {
    /// When the open session opened, for any reason.
    opened_at: Option<Instant>,
}

impl BitratePacer {
    /// The bitrate this frame wants. `opened` and `wanted` are encoder,
    /// bitrate and frame rate. The newest target, unless only the bitrate
    /// differs, downwards, within 1 s of the session's open, or either way
    /// within 5 s of a Quick Sync session's: then the session's own.
    fn wanted(&self, opened: (&str, u32, u32), wanted: (&str, u32, u32), now: Instant) -> u32 {
        let gap = if opened.0.ends_with("_qsv") {
            QSV_BITRATE_REOPEN_GAP
        } else if wanted.1 < opened.1 {
            BITRATE_DECREASE_GAP
        } else {
            Duration::ZERO
        };
        let waits = (opened.0, opened.2) == (wanted.0, wanted.2)
            && wanted.1 != opened.1
            && self
                .opened_at
                .is_some_and(|at| now.duration_since(at) < gap);
        if waits {
            opened.1
        } else {
            wanted.1
        }
    }

    /// A session opened, at the newest target.
    fn opened(&mut self, now: Instant) {
        self.opened_at = Some(now);
    }
}

/// Why the open session reopens, for its log line: the first of these that
/// holds. `opened` and `wanted` are encoder and frame rate; `owed_try` is a
/// try of the encoder a stalled open left owed.
fn reopen_reason(
    opened: (&str, u32),
    wanted: (&str, u32),
    size_changed: bool,
    keyframe: bool,
    owed_try: bool,
) -> &'static str {
    if size_changed {
        "size"
    } else if opened.0 != wanted.0 {
        if owed_try || is_hevc_encoder(opened.0) == is_hevc_encoder(wanted.0) {
            "recovery"
        } else {
            "codec"
        }
    } else if opened.1 != wanted.1 {
        "fps"
    } else if keyframe {
        "keyframe"
    } else {
        "bitrate"
    }
}

/// The first wait before an H.264 reopen that failed is tried again for a
/// client that cannot decode the HEVC session; each wait doubles, up to
/// [`UNDECODABLE_RETRY_MAX`].
const UNDECODABLE_RETRY_FIRST: Duration = Duration::from_secs(2);
const UNDECODABLE_RETRY_MAX: Duration = Duration::from_secs(8);

/// Tries again a reopen that failed while the open session's codec is one
/// the client cannot decode (HEVC to an H.264-only client). Such a client
/// has no picture, so the failure is never left to wait for an ABR step.
#[derive(Default)]
struct UndecodableRetry {
    /// When the failed reopen is tried again.
    due: Option<Instant>,
    /// The last wait; zero before the first.
    wait: Duration,
    tries: u32,
}

impl UndecodableRetry {
    /// Each frame, before the reopen decision. `undecodable`: the client
    /// cannot decode the open session's codec. Forgets `failed_reopen` once
    /// a try is due.
    fn frame(
        &mut self,
        undecodable: bool,
        failed_reopen: &mut Option<(String, u32, u32)>,
        now: Instant,
    ) {
        if !undecodable {
            *self = Self::default();
            return;
        }
        let Some((encoder, _, _)) = failed_reopen.as_ref() else {
            return;
        };
        match self.due {
            None => {
                self.wait = if self.wait.is_zero() {
                    UNDECODABLE_RETRY_FIRST
                } else {
                    (self.wait * 2).min(UNDECODABLE_RETRY_MAX)
                };
                self.due = Some(now + self.wait);
            }
            Some(due) if now >= due => {
                self.tries += 1;
                info!(
                    encoder = %encoder,
                    attempt = self.tries,
                    waited_ms = self.wait.as_millis() as u64,
                    "Trying the H.264 reopen again: the client cannot decode HEVC"
                );
                self.due = None;
                *failed_reopen = None;
            }
            Some(_) => {}
        }
    }
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

/// The H.265 encoder to stream when HEVC is wanted: an H.265 encoder
/// itself (an override such as ETERNAL_ENCODER=hevc_nvenc), else the H.265
/// sibling of an H.264 one.
fn hevc_encoder_for(encoder_name: &str) -> Option<&str> {
    if is_hevc_encoder(encoder_name) {
        Some(encoder_name)
    } else {
        hevc_variant_of(encoder_name)
    }
}

/// True for encoders that cannot be asked for a keyframe on a given frame.
/// FFmpeg 7.1's AMF wrapper never reads `AVFrame::pict_type` (8.0 added that,
/// behind its `forced_idr` option), so AMF keeps encoding P-frames until its
/// own IDR period. Every new session starts with an IDR carrying its
/// parameter sets, so the encode loop reopens AMF to honour a request.
pub fn ignores_forced_keyframes(encoder_name: &str) -> bool {
    matches!(encoder_name, "h264_amf" | "hevc_amf")
}

/// True for encoders that keep each frame until they are given the next.
/// FFmpeg 7.1's AMF wrapper asks AMF for output only when it submits a frame
/// (8.0 can wait for it, with `query_timeout`). On a still desktop a new
/// viewer's opening keyframe sat inside AMF until capture's next picture,
/// the 750 ms keepalive: AMD PCs were slow on 39-56% of reconnects.
pub fn holds_last_frame(encoder_name: &str) -> bool {
    matches!(encoder_name, "h264_amf" | "hevc_amf")
}

/// How long the encode loop waits for capture's next picture before it gives
/// an encoder that [holds its last frame](holds_last_frame) the last picture
/// again, to bring out a session's first keyframe. It is about one frame at
/// 60 fps, and longer than AMF takes to encode one.
const KEYFRAME_FLUSH_WAIT: Duration = Duration::from_millis(20);
/// How long after the keyframe went in the last picture is given again,
/// before the keyframe is left to capture's next picture: an AMD driver can
/// take a few hundred ms to encode a fresh session's first frame, and
/// capture's keepalive follows at 750 ms.
const KEYFRAME_FLUSH_FOR: Duration = Duration::from_millis(500);

/// A session's first keyframe still inside an encoder that holds its last
/// frame, and the picture to give it again.
struct OwedKeyframe {
    /// The last picture encoded. It pins one of capture's few buffers.
    picture: Option<RawFrame>,
    /// When the picture stops being given again.
    until: Instant,
    given: u32,
}

/// What the encode loop encodes next while a keyframe is owed.
enum Next {
    Frame(RawFrame),
    Closed,
    /// The deadline passed: capture's next picture brings the keyframe out.
    GaveUp,
}

impl OwedKeyframe {
    fn until(until: Instant) -> Self {
        Self {
            picture: None,
            until,
            given: 0,
        }
    }

    /// What is owed after a frame was submitted at `submitted`: nothing once
    /// the keyframe came out or the encoder holds no frame, else `picture`
    /// to give again, until [`KEYFRAME_FLUSH_FOR`] after the first try.
    fn after_encode(
        owed: Option<Self>,
        picture: Option<RawFrame>,
        keyframe_out: bool,
        submitted: Instant,
    ) -> Option<Self> {
        let picture = picture.filter(|_| !keyframe_out)?;
        let mut owed = owed.unwrap_or_else(|| Self::until(submitted + KEYFRAME_FLUSH_FOR));
        owed.picture = Some(picture);
        Some(owed)
    }

    /// Capture's next picture if one arrives within [`KEYFRAME_FLUSH_WAIT`],
    /// else the last picture again, until the deadline.
    fn next(&mut self, frames: &FrameSlot) -> Next {
        let wait = KEYFRAME_FLUSH_WAIT.min(self.until.saturating_duration_since(Instant::now()));
        match frames.take_within(wait, &mut self.picture) {
            Taken::Frame(frame) => Next::Frame(frame),
            Taken::Closed => Next::Closed,
            Taken::Nothing => match self.picture.take() {
                Some(picture) if Instant::now() < self.until => {
                    self.given += 1;
                    debug!(
                        frame = picture.frame_number,
                        given = self.given,
                        "Giving the encoder its last picture again to bring out the keyframe"
                    );
                    Next::Frame(picture)
                }
                _ => Next::GaveUp,
            },
        }
    }
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
        // The period runs all stream long, and FFmpeg 7.1's AMF ignores it
        // (see `ignores_forced_keyframes`): debug only.
        if !force_hit {
            debug!(
                encoder = encoder_name,
                frame = frame_number,
                period = AMF_FORCED_INTRA_PERIOD,
                reason,
                "[AMF] Forced IDR"
            );
        } else if is_amf {
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
                &mut EncoderInput::Yuv420,
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

    fn libx264_at(width: u32, height: u32) -> EncoderState {
        EncoderState::new(
            "libx264",
            width,
            height,
            8_000_000,
            60,
            &mut EncoderInput::Yuv420,
            Opening::First,
            &mut || {},
        )
        .unwrap_or_else(|error| panic!("{width}x{height}: {error}"))
    }

    /// swscale gets threads only where it converts every frame: with
    /// Accelerate converting on macOS it is a fallback and runs on one.
    #[test]
    fn swscale_gets_threads_only_where_it_converts_every_frame() {
        let state = libx264_at(96, 64);
        let scaler = state.scaler.as_ref().expect("x264 takes YUV420P");
        #[cfg(target_os = "macos")]
        assert_eq!(
            (state.native_converter.is_some(), scaler.threads()),
            (true, 1)
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(scaler.threads(), input::scaler_threads());
    }

    /// A synthetic capture, in a buffer as capture hands it over.
    fn synthetic_capture(width: u32, height: u32, frame_number: u64) -> Vec<u8> {
        let frame_bytes = width as usize * 4 * height as usize;
        let mut rows = crate::capture::frame_buffer(frame_bytes);
        crate::capture::synthetic::render_synthetic_frame(
            &mut rows[..frame_bytes],
            width,
            height,
            frame_number,
        );
        rows
    }

    /// A screen with an odd height (1680x945, from a report) or width
    /// streams: libx264 refused to open at those sizes, and the pipeline
    /// gave up after six restarts. The encoder opens at the even size below,
    /// takes the capture's frames, and they decode at that size with the
    /// picture intact.
    #[test]
    fn odd_screen_sizes_encode_at_the_even_size_below() {
        ffmpeg_next::init().unwrap();
        for ((width, height), encoded) in [
            ((1680, 945), (1680, 944)),
            ((1365, 768), (1364, 768)),
            ((641, 361), (640, 360)),
            ((1366, 768), (1366, 768)),
        ] {
            let mut state = libx264_at(width, height);
            assert_eq!((state.frame.width(), state.frame.height()), encoded);
            // The same capture size never reopens the session; another does.
            assert!(state.accepts_frame(width, height));
            assert!(!state.accepts_frame(width + 2, height));
            assert!(!state.accepts_frame(width, height + 2));
            let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264).unwrap();
            let mut decoder = ffmpeg_next::codec::Context::new_with_codec(codec)
                .decoder()
                .video()
                .unwrap();
            let mut decoded = ffmpeg_next::frame::Video::empty();
            let mut counters = Vec::new();
            let start = Instant::now();
            for n in 1..=10u64 {
                let rows = synthetic_capture(width, height, n);
                state.convert(&rows, width as usize * 4).unwrap();
                let captured = start + Duration::from_micros(n * 16_667);
                let pts = state.next_pts(captured, n);
                state.frame.set_pts(Some(pts));
                state.send_frame(pts, captured, n).unwrap();
                while state.encoder.receive_packet(&mut state.packet).is_ok() {
                    if state.h264.needs_parameter_sets() {
                        state
                            .h264
                            .refresh_parameter_sets_from_extradata(encoder_extradata(
                                &state.encoder,
                            ));
                    }
                    // What the transport sends: Annex B with parameter sets.
                    let (annex_b, _) = normalize_h264_payload_with_info(
                        state.packet.data().unwrap_or_default(),
                        state.packet.is_key(),
                        &mut state.h264,
                        &state.opened_encoder,
                    );
                    decoder
                        .send_packet(&ffmpeg_next::Packet::copy(&annex_b))
                        .unwrap();
                    while decoder.receive_frame(&mut decoded).is_ok() {
                        assert_eq!((decoded.width(), decoded.height()), encoded);
                        counters.push(crate::capture::synthetic::decode_frame_counter_from_luma(
                            decoded.data(0),
                            decoded.stride(0),
                            encoded.0,
                        ));
                    }
                }
            }
            assert_eq!(
                counters,
                (1..=counters.len() as u64).collect::<Vec<_>>(),
                "{width}x{height}"
            );
            assert!(counters.len() >= 5, "{width}x{height}: {counters:?}");
        }
    }

    /// The rows of each of `frame`'s planes that hold the picture.
    fn picture(frame: &ffmpeg_next::frame::Video) -> Vec<Vec<u8>> {
        use ffmpeg_next::format::Pixel;
        let mut rows = Vec::new();
        for plane in 0..frame.planes() {
            let bytes = match (frame.format(), plane) {
                (Pixel::BGRA | Pixel::BGRZ, _) => frame.width() as usize * 4,
                (Pixel::NV12, 1) => frame.plane_width(1) as usize * 2,
                _ => frame.plane_width(plane) as usize,
            };
            for y in 0..frame.plane_height(plane) as usize {
                rows.push(frame.data(plane)[y * frame.stride(plane)..][..bytes].to_vec());
            }
        }
        rows
    }

    /// A capture with an odd last column or row encodes exactly what the
    /// rest of it, alone, does: the crop leaves that column and row out and
    /// resamples nothing, on every input path (swscale to YUV420P and NV12,
    /// Accelerate on macOS, the BGRA copy NVENC reads), with or without
    /// padding after the capture's last row.
    #[test]
    fn cropping_changes_no_pixel_of_the_picture() {
        use ffmpeg_next::format::Pixel;
        ffmpeg_next::init().unwrap();
        for (width, height) in [(1365u32, 945u32), (1365, 944), (1366, 945)] {
            let (even_width, even_height) = encoded_size(width, height);
            let capture = synthetic_capture(width, height, 77);
            let row_bytes = width as usize * 4;
            let unpadded = &capture[..row_bytes * height as usize];
            // The capture's top-left even_width x even_height, alone.
            let even_bytes = even_width as usize * 4;
            let mut alone = crate::capture::frame_buffer(even_bytes * even_height as usize);
            for y in 0..even_height as usize {
                alone[y * even_bytes..][..even_bytes]
                    .copy_from_slice(&capture[y * row_bytes..][..even_bytes]);
            }
            let mut paths = vec!["YUV420P", "NV12", "BGRA"];
            if cfg!(target_os = "macos") {
                paths.push("Accelerate");
            }
            for path in paths {
                let label = format!("{width}x{height} {path}");
                let mut state = libx264_at(width, height);
                // The frame and conversion an encoder that takes `path` has.
                #[cfg(target_os = "macos")]
                if path != "Accelerate" {
                    state.native_converter = None;
                }
                let format = match path {
                    "NV12" => Pixel::NV12,
                    "BGRA" => Pixel::BGRA,
                    _ => Pixel::YUV420P,
                };
                state.frame = ffmpeg_next::frame::Video::new(format, even_width, even_height);
                state.scaler = input::conversion_context(
                    format,
                    even_width,
                    even_height,
                    input::scaler_threads(),
                )
                .unwrap();
                assert_eq!(state.scaler.is_some(), format != Pixel::BGRA);
                state.convert(&alone, even_bytes).unwrap();
                let expected = picture(&state.frame);
                for rows in [&capture[..], unpadded] {
                    state.frame = ffmpeg_next::frame::Video::new(format, even_width, even_height);
                    state.convert(rows, row_bytes).unwrap();
                    assert_eq!(picture(&state.frame), expected, "{label}");
                }
                // Accelerate converted each, with no fallback to swscale.
                #[cfg(target_os = "macos")]
                assert_eq!(state.native_converter.is_some(), path == "Accelerate");
                if format == Pixel::BGRA {
                    let rows = capture.chunks(row_bytes).take(even_height as usize);
                    for (y, row) in rows.enumerate() {
                        assert_eq!(expected[y], row[..even_bytes], "{label} row {y}");
                    }
                }
            }
        }
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
                &mut EncoderInput::Yuv420,
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
        let (selected, ()) = open_remaining_encoders(("h264_nvenc", "test"), no_list, |name| {
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

    /// No adapter list: off Windows, or DXGI could not list every card.
    fn no_list() -> Option<Vec<GpuVendor>> {
        None
    }

    /// The encoders the fallback tries after `failed`, given the vendors
    /// DXGI lists, and the one chosen: `opens` and libx264 open, nothing
    /// else does.
    fn fallback(
        failed: &str,
        vendors: Option<Vec<GpuVendor>>,
        opens: &str,
    ) -> (Vec<String>, &'static str) {
        let mut attempts = Vec::new();
        let (selected, ()) = open_remaining_encoders(
            (failed, "test"),
            || vendors,
            |name| {
                attempts.push(name.to_string());
                if name == opens || name == "libx264" {
                    Ok(())
                } else {
                    Err("device unavailable".into())
                }
            },
        )
        .unwrap();
        (attempts, selected)
    }

    #[test]
    fn the_fallback_tries_the_listed_cards_encoders_first() {
        use GpuVendor::{Amd, Intel, Nvidia};
        // An NVIDIA desktop whose NVENC failed: the other encoders are still
        // tried, as before, but its own card has none.
        assert_eq!(
            fallback("h264_nvenc", Some(vec![Nvidia]), "none").0,
            ["h264_amf", "h264_qsv", "libx264"]
        );
        // RTX + Radeon iGPU: AMF, then Quick Sync.
        assert_eq!(
            fallback("h264_nvenc", Some(vec![Nvidia, Amd]), "h264_amf"),
            (vec!["h264_amf".to_string()], "h264_amf")
        );
        // An Intel PC whose Quick Sync failed once, then an AMD eGPU: AMF
        // moves ahead of NVENC, because the adapters are read when the
        // fallback runs, not at launch.
        assert_eq!(
            fallback("h264_qsv", Some(vec![Intel]), "none").0,
            ["h264_nvenc", "h264_amf", "libx264"]
        );
        assert_eq!(
            fallback("h264_qsv", Some(vec![Intel, Amd]), "h264_amf"),
            (vec!["h264_amf".to_string()], "h264_amf")
        );
        // A non-Windows backend never asks.
        let (selected, ()) = open_remaining_encoders(
            ("h264_videotoolbox", "test"),
            || panic!("asked"),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(selected, "libx264");
    }

    #[test]
    fn an_nvidia_card_windows_does_not_list_still_takes_over() {
        // An Intel display adapter and an NVIDIA card in TCC mode, which DXGI
        // does not list: Quick Sync fails, NVENC works.
        assert_eq!(
            fallback("h264_qsv", Some(vec![GpuVendor::Intel]), "h264_nvenc"),
            (vec!["h264_nvenc".to_string()], "h264_nvenc")
        );
        // The same with an AMD display adapter.
        assert_eq!(
            fallback("h264_amf", Some(vec![GpuVendor::Amd]), "h264_nvenc"),
            (vec!["h264_nvenc".to_string()], "h264_nvenc")
        );
    }

    #[test]
    fn an_incomplete_adapter_list_keeps_the_usual_order() {
        // DXGI described the NVIDIA card, then failed on the Intel one (see
        // `gpu::tests`): no list, so Quick Sync is tried, after AMF, as
        // before.
        assert_eq!(
            fallback("h264_nvenc", None, "h264_qsv"),
            (
                vec!["h264_amf".to_string(), "h264_qsv".to_string()],
                "h264_qsv"
            )
        );
        assert_eq!(
            fallback("h264_qsv", None, "none").0,
            ["h264_nvenc", "h264_amf", "libx264"]
        );
    }

    #[test]
    fn an_unknown_vendor_moves_no_encoder() {
        use GpuVendor::{Intel, Unknown};
        // A card DXGI lists under a vendor ID with no encoder here (Windows'
        // basic display adapter before a driver is installed, a remote or
        // virtual display card): every hardware encoder is still tried, in
        // the usual order.
        assert_eq!(
            fallback("h264_nvenc", Some(vec![Unknown]), "h264_qsv"),
            (
                vec!["h264_amf".to_string(), "h264_qsv".to_string()],
                "h264_qsv"
            )
        );
        // Beside it, a listed vendor's encoder still comes first.
        assert_eq!(
            fallback("h264_nvenc", Some(vec![Unknown, Intel]), "none").0,
            ["h264_qsv", "h264_amf", "libx264"]
        );
    }

    #[test]
    fn a_hybrid_pc_tries_both_listed_cards_before_the_others() {
        use GpuVendor::{Amd, Intel, Nvidia};
        // An Intel + NVIDIA laptop whose NVENC failed: Quick Sync, then AMF.
        assert_eq!(
            fallback("h264_nvenc", Some(vec![Intel, Nvidia]), "none").0,
            ["h264_qsv", "h264_amf", "libx264"]
        );
        // A Radeon + Intel laptop whose AMF failed: Quick Sync before NVENC,
        // which is not probed when Quick Sync works.
        assert_eq!(
            fallback("h264_amf", Some(vec![Amd, Intel]), "h264_qsv"),
            (vec!["h264_qsv".to_string()], "h264_qsv")
        );
        assert_eq!(
            fallback("h264_amf", Some(vec![Amd, Intel]), "none").0,
            ["h264_qsv", "h264_nvenc", "libx264"]
        );
    }

    /// What `run` logs on this thread, as the log file has it.
    fn logged<R>(run: impl FnOnce() -> R) -> (R, Vec<String>) {
        use std::sync::Arc;
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
        let out = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
        let writer = Arc::clone(&out);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Sink(Arc::clone(&writer)))
            .finish();
        crate::logging::prepare_log_capture();
        let result = tracing::subscriber::with_default(subscriber, run);
        let lines = String::from_utf8(out.lock().clone()).unwrap();
        (result, lines.lines().map(str::to_string).collect())
    }

    /// An open that returned, as every open but a wait-limited one in the
    /// lane does.
    fn now(opened: Opened) -> OpenResult {
        match opened {
            Opened::Now(result) => result,
            Opened::Behind(_) => panic!("the open went on in the background"),
        }
    }

    /// A real FFmpeg open that fails with an FFmpeg error line: libx264,
    /// which opens on the calling thread, with no picture size.
    fn failing_open(opening: Opening) -> Vec<String> {
        ffmpeg_next::init().unwrap();
        crate::diagnostics::install_ffmpeg_logging();
        let codec = ffmpeg_next::encoder::find_by_name("libx264").expect("libx264");
        let mut video = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder()
            .video()
            .unwrap();
        video.set_format(ffmpeg_next::format::Pixel::YUV420P);
        video.set_time_base(ffmpeg_next::Rational(1, 60));
        let (opened, lines) = logged(|| {
            now(open_encoder_on(
                crate::stall::lanes(),
                video,
                "libx264",
                opening,
                &mut || {},
            ))
        });
        assert!(matches!(opened, Ok(Err(_))), "the open must fail");
        lines
            .into_iter()
            .filter(|line| line.contains("ffmpeg"))
            .collect()
    }

    #[test]
    fn ffmpeg_errors_are_warnings_only_while_an_encoder_is_one_of_several() {
        let probed = failing_open(Opening::Probe(Instant::now() + FIRST_PICTURE_WAIT));
        assert!(!probed.is_empty());
        assert!(
            probed
                .iter()
                .all(|line| line.contains(" WARN ") && line.contains("ffmpeg_level=16")),
            "{probed:?}"
        );
        // After the probe, and for an open with nothing after it, FFmpeg's
        // errors are errors again.
        for opening in [Opening::First, Opening::Reopen] {
            let lines = failing_open(opening);
            assert!(
                lines.iter().any(|line| line.contains(" ERROR ")),
                "{opening:?}: {lines:?}"
            );
            assert_eq!(
                lines.iter().filter(|line| line.contains(" WARN ")).count(),
                0,
                "{opening:?}: {lines:?}"
            );
        }
        // The pipeline's first open probes, except the last resort.
        let by = Instant::now() + FIRST_PICTURE_WAIT;
        assert_eq!(first_open("h264_nvenc", by), Opening::Probe(by));
        assert_eq!(first_open("hevc_amf", by), Opening::Probe(by));
        assert_eq!(first_open("libx265", by), Opening::Probe(by));
        assert_eq!(first_open("h264_videotoolbox", by), Opening::Probe(by));
        assert_eq!(first_open("libx264", by), Opening::First);
        // A probe has the first open's limits.
        assert_eq!(
            open_limits("h264_qsv", Opening::Probe(Instant::now())),
            open_limits("h264_qsv", Opening::First)
        );
    }

    #[test]
    fn a_probe_on_the_encoder_lane_thread_lowers_that_threads_errors() {
        use crate::stall::{no_hooks, run_in, Call, Lanes, Wait};
        use std::time::Duration;
        crate::diagnostics::install_ffmpeg_logging();
        let lanes: &'static Lanes = Box::leak(Box::new(Lanes::new()));
        let log_error = || {
            let message = std::ffi::CString::new("probe check %d\n").unwrap();
            unsafe {
                ffmpeg_next::ffi::av_log(
                    std::ptr::null_mut(),
                    ffmpeg_next::ffi::AV_LOG_ERROR,
                    message.as_ptr(),
                    7i32,
                );
            }
        };
        // The call's thread logs to the caller's log here, as it does to the
        // app's one log file in the app.
        let lines = |probe: bool| {
            let (result, lines) = logged(|| {
                let dispatch = tracing::dispatcher::get_default(Clone::clone);
                run_in(
                    lanes,
                    Call::EncoderOpen,
                    Wait {
                        limits: Call::EncoderOpen.limits(),
                        slice: Duration::from_millis(1),
                        now: Instant::now,
                    },
                    move || {
                        tracing::dispatcher::with_default(&dispatch, || {
                            if probe {
                                crate::diagnostics::while_probing(log_error)
                            } else {
                                log_error()
                            }
                        })
                    },
                    || {},
                    no_hooks(),
                )
            });
            assert!(result.is_ok());
            lines
                .into_iter()
                .filter(|line| line.contains("probe check 7"))
                .collect::<Vec<_>>()
        };
        let probed = lines(true);
        assert_eq!(probed.len(), 1);
        assert!(probed[0].contains(" WARN "), "{probed:?}");
        let plain = lines(false);
        assert_eq!(plain.len(), 1);
        assert!(plain[0].contains(" ERROR "), "{plain:?}");
        // The caller's thread was never marked.
        let (_, own) = logged(log_error);
        assert!(own.iter().any(|line| line.contains(" ERROR ")), "{own:?}");
    }

    #[test]
    fn one_error_names_every_encoder_when_none_opens() {
        let ((result, attempts), lines) = logged(|| {
            let mut attempts = Vec::new();
            let result = open_remaining_encoders::<()>(
                ("h264_nvenc", "h264_nvenc open failed: FFmpeg code=-38"),
                || Some(vec![GpuVendor::Nvidia, GpuVendor::Intel]),
                |name| {
                    attempts.push(name.to_string());
                    Err(format!("{name} open failed: device unavailable").into())
                },
            );
            (result, attempts)
        });
        assert!(result.is_err());
        assert_eq!(attempts, ["h264_qsv", "h264_amf", "libx264"]);
        let errors: Vec<_> = lines
            .iter()
            .filter(|line| line.contains(" ERROR "))
            .collect();
        assert_eq!(errors.len(), 1, "{lines:?}");
        for reason in [
            "h264_nvenc open failed: FFmpeg code=-38",
            "h264_qsv open failed: device unavailable",
            "h264_amf open failed: device unavailable",
            "libx264 open failed: device unavailable",
        ] {
            assert!(errors[0].contains(reason), "{reason}: {}", errors[0]);
        }
        // An encoder that opens logs no error.
        let (_, lines) = logged(|| {
            open_remaining_encoders(("h264_nvenc", "failed"), no_list, |name| match name {
                "libx264" => Ok(()),
                _ => Err("device unavailable".into()),
            })
            .unwrap()
        });
        assert!(
            !lines.iter().any(|line| line.contains(" ERROR ")),
            "{lines:?}"
        );
    }

    #[test]
    fn only_nvencs_refusal_of_a_driver_older_than_570_means_an_outdated_driver() {
        let version = |a: u64, b: u64, c: u64, d: u64| a << 48 | b << 32 | c << 16 | d;
        // Field drivers: GTX 1650 on 517.18, RTX 4060 laptop on 566.07.
        let old = version(31, 0, 15, 1718);
        let laptop = version(32, 0, 15, 6607);
        let first_new_enough = version(32, 0, 15, 7000);
        let current = version(32, 0, 16, 2101);
        assert_eq!(nvidia_release(old), 51_718);
        assert_eq!(nvidia_release(laptop), 56_607);
        assert_eq!(nvidia_release(first_new_enough), NVENC_MIN_RELEASE);
        assert_eq!(nvidia_release(current), 62_101);
        assert_eq!(nvidia_release(version(21, 21, 13, 7654)), 37_654);
        assert_eq!(
            NVENC_MIN_RELEASE,
            NVENC_MIN_DRIVER.parse::<u32>().unwrap() * 100
        );

        let enosys = ffmpeg_next::Error::Other {
            errno: libc::ENOSYS,
        };
        let eperm = ffmpeg_next::Error::Other { errno: libc::EPERM };
        let outdated = |name: &str, error, drivers: &[u64]| {
            outdated_nvidia_driver(name, error, || drivers.to_vec())
        };
        assert_eq!(outdated("h264_nvenc", enosys, &[old]), Some(51_718));
        assert_eq!(outdated("hevc_nvenc", enosys, &[laptop]), Some(56_607));
        // 570.00 itself is new enough.
        assert_eq!(outdated("h264_nvenc", enosys, &[first_new_enough]), None);
        // ENOSYS with a driver new enough is an unsupported parameter.
        assert_eq!(outdated("h264_nvenc", enosys, &[current]), None);
        assert_eq!(outdated("h264_nvenc", enosys, &[old, current]), None);
        // No NVIDIA driver version known.
        assert_eq!(outdated("h264_nvenc", enosys, &[]), None);
        // "Cannot load nvcuda.dll": no NVIDIA driver at all.
        assert_eq!(outdated("h264_nvenc", eperm, &[old]), None);
        assert_eq!(
            outdated("h264_nvenc", ffmpeg_next::Error::External, &[old]),
            None
        );
        // Other encoders' failures never read the NVIDIA drivers.
        assert_eq!(
            outdated_nvidia_driver("h264_amf", enosys, || panic!("read")),
            None
        );
        assert_eq!(
            outdated_nvidia_driver("h264_nvenc", eperm, || panic!("read")),
            None
        );
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
        let (selected, ()) = open_remaining_encoders(("h264_nvenc", "test"), no_list, |name| {
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
    /// the inputs (HEVC wanted, the client's session and whether it decodes
    /// HEVC, bitrate, frame rate, the lane, the clock) and says how each
    /// reopen ends.
    struct Pipeline {
        recovery: StallRecovery,
        encoder_name: String,
        hevc_open_failed: bool,
        failed_reopen: Option<(String, u32, u32)>,
        negotiated: Option<(bool, Option<u32>)>,
        undecodable_retry: UndecodableRetry,
        /// The client's session; `None` with no client.
        session: Option<u32>,
        client_hevc: bool,
        /// Encoder, bitrate and frame rate of the open session.
        opened: (String, u32, u32),
        pacer: BitratePacer,
        /// Bitrate changes the open libx264 or Quick Sync session took in
        /// place.
        in_place: usize,
        /// A Quick Sync session refused a bitrate change in place.
        qsv_refused: bool,
        /// The user's preference.
        hevc_wanted: bool,
        bitrate: u32,
        fps: u32,
        keyframe: bool,
        lane_clear: bool,
        now: Instant,
    }

    impl Pipeline {
        /// `opened` streams at 15 Mbps (where ABR starts) and 60 fps, and
        /// `encoder_name` is the H.264 encoder. HEVC is wanted if `opened`
        /// is HEVC.
        fn new(encoder_name: &str, opened: &str, now: Instant) -> Self {
            Self {
                recovery: StallRecovery::default(),
                encoder_name: encoder_name.to_string(),
                hevc_open_failed: false,
                failed_reopen: None,
                negotiated: None,
                undecodable_retry: UndecodableRetry::default(),
                session: Some(1),
                client_hevc: true,
                opened: (opened.to_string(), 15_000_000, 60),
                pacer: BitratePacer::default(),
                in_place: 0,
                qsv_refused: false,
                hevc_wanted: is_hevc_encoder(opened),
                bitrate: 15_000_000,
                fps: 60,
                keyframe: false,
                lane_clear: true,
                now,
            }
        }

        /// One frame. The encoder it reopened to, if it reopened.
        fn frame(&mut self, reopen: Reopen) -> Option<String> {
            let client_hevc = self.session.is_some() && self.client_hevc;
            let hevc_wanted = self.hevc_wanted && client_hevc;
            if !hevc_wanted {
                self.hevc_open_failed = false;
            }
            let preferred = if hevc_wanted && !self.hevc_open_failed {
                hevc_encoder_for(&self.encoder_name).unwrap().to_string()
            } else {
                self.encoder_name.clone()
            };
            let desired = self
                .recovery
                .encoder_for_frame(&preferred, self.now, self.lane_clear);
            forget_failed_reopen(
                &mut self.failed_reopen,
                &mut self.negotiated,
                (hevc_wanted, self.session),
                &desired,
            );
            let undecodable =
                self.session.is_some() && !client_hevc && is_hevc_encoder(&self.opened.0);
            self.undecodable_retry
                .frame(undecodable, &mut self.failed_reopen, self.now);
            // As `update_bitrate_without_reopening`.
            if takes_bitrate_in_place(&self.opened.0, self.qsv_refused)
                && (self.opened.0.as_str(), self.opened.2) == (desired.as_str(), self.fps)
                && self.opened.1 != self.bitrate
            {
                self.opened.1 = self.bitrate;
                self.failed_reopen = None;
                self.in_place += 1;
            }
            let opened = (self.opened.0.as_str(), self.opened.1, self.opened.2);
            let wanted = self
                .pacer
                .wanted(opened, (&desired, self.bitrate, self.fps), self.now);
            if !reopen_needed(
                opened,
                (&desired, wanted, self.fps),
                self.keyframe,
                self.failed_reopen.as_ref(),
                self.lane_clear,
            ) {
                return None;
            }
            self.recovery.starting(&desired);
            if reopen == Reopen::Opens {
                self.opened = (desired.clone(), self.bitrate, self.fps);
                self.pacer.opened(self.now);
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
        let (name, opened) = open_remaining_encoders((preferred, "stalled"), no_list, |name| {
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
            &mut EncoderInput::Yuv420,
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
        p.session = Some(2);
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.frame(Reopen::Opens), None);
    }

    #[test]
    fn an_h264_only_client_gets_h264_when_the_downgrade_is_tried_again() {
        // HEVC streams and an iPad that decodes only H.264 connects. The
        // H.264 reopen fails for real: the iPad cannot decode what streams.
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.session = Some(2);
        p.client_hevc = false;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.opened.0, "hevc_nvenc");
        // It is tried again 2 s after the next frame, with no bitrate
        // change, and opens.
        for _ in 0..125 {
            p.later(16);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        p.later(16);
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.opened, ("h264_nvenc".to_string(), 15_000_000, 60));
        for _ in 0..1_000 {
            p.later(16);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
    }

    #[test]
    fn a_downgrade_for_an_h264_only_client_backs_off_to_every_8_s() {
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.session = Some(2);
        p.client_hevc = false;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        // A frame every 10 ms. Each try fails, and the next waits 2, 4, 8,
        // then 8 s again, counted from the frame after the failure.
        let mut tries = Vec::new();
        for frame in 1..=3_400 {
            p.later(10);
            if p.frame(Reopen::Fails).is_some() {
                tries.push(frame * 10);
            }
        }
        assert_eq!(tries, [2_010, 6_020, 14_030, 22_040, 30_050]);
        assert_eq!(p.opened.0, "hevc_nvenc");
        assert_eq!(p.recovery.tries, 0, "a real error is not a stall try");
        // An iPad that decodes HEVC connects instead: nothing is retried,
        // and the next H.264 failure is not either.
        p.session = Some(3);
        p.client_hevc = true;
        p.hevc_wanted = false;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        for _ in 0..3_000 {
            p.later(10);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
    }

    #[test]
    fn a_failed_downgrade_is_tried_once_when_the_client_leaves() {
        // HEVC streams to an H.264-only iPad whose H.264 reopen failed.
        let mut p = Pipeline::new("h264_nvenc", "hevc_nvenc", Instant::now());
        p.session = Some(2);
        p.client_hevc = false;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        // The iPad leaves before the retry: no client, a new negotiation,
        // one try. It fails too, and with nobody to decode, the HEVC
        // session streams on without further tries.
        p.later(1_000);
        p.session = None;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        for _ in 0..6_000 {
            p.later(10);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        assert_eq!(p.opened.0, "hevc_nvenc");
        // An iPad that wants HEVC comes back: HEVC streams on as it is.
        p.session = Some(3);
        p.client_hevc = true;
        p.hevc_wanted = true;
        for _ in 0..100 {
            p.later(10);
            assert_eq!(p.frame(Reopen::Opens), None);
        }
        // Had an H.264-only iPad come back, it would get H.264 at once.
        p.session = Some(4);
        p.client_hevc = false;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
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
        // QSV streams, and reopens for a bitrate change since it refused
        // one in place. ABR steps stall its reopen, twice: the next tries
        // are due 2 s and then 8 s later. Each time the lane clears 1 s
        // after the stall, and a bitrate change reopens on the next frame.
        let mut p = Pipeline::new("h264_qsv", "h264_qsv", Instant::now());
        p.qsv_refused = true;
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

    /// Plays `steps` through the pipeline, a frame every 16 ms from its
    /// clock, until `end_ms`: at each offset in ms, a new ABR target, or
    /// `None` for an AMF keyframe reopen. The reopens, as (ms, bitrate).
    fn play(p: &mut Pipeline, steps: &[(u64, Option<u32>)], end_ms: u64) -> Vec<(u64, u32)> {
        let start = p.now;
        let mut steps = steps.iter().peekable();
        let mut reopens = Vec::new();
        for ms in (0..=end_ms).step_by(16) {
            p.now = start + Duration::from_millis(ms);
            while let Some((_, step)) = steps.next_if(|(at, _)| *at <= ms) {
                match step {
                    Some(bitrate) => p.bitrate = *bitrate,
                    None => p.keyframe = true,
                }
            }
            if p.frame(Reopen::Opens).is_some() {
                reopens.push((ms, p.opened.1));
            }
        }
        reopens
    }

    #[test]
    fn the_amd_laptops_last_50_seconds_never_reopen_amf_twice_within_a_second() {
        // The gaming laptop's h264_amf host log from 11-01 23:03:53.22Z,
        // the 50 s before its display froze, in ms from an AMF keyframe
        // reopen at 10 Mbps. ABR stepped 10 -> 8 -> 4 -> 6 -> 8 Mbps and
        // the iPad asked for two more keyframes. On the candidate the 7
        // reopens came at 0, 432, 14_368, 14_416, 29_936, 42_672 and 45_424
        // ms: each drop 48 to 432 ms after a keyframe reopen.
        let mut p = Pipeline::new("h264_amf", "h264_amf", Instant::now());
        p.opened.1 = 10_000_000;
        p.bitrate = 10_000_000;
        let steps = [
            (0, None),
            (422, Some(8_000_000)),
            (14_360, None),
            (14_409, Some(4_000_000)),
            (29_929, Some(6_000_000)),
            (42_663, None),
            (45_417, Some(8_000_000)),
        ];
        // Still 7, but each drop waits for the second after the keyframe
        // reopen before it, and the rises go at once.
        let reopens = play(&mut p, &steps, 51_622);
        assert_eq!(
            reopens,
            [
                (0, 10_000_000),
                (1_008, 8_000_000),
                (14_368, 8_000_000),
                (15_376, 4_000_000),
                (29_936, 6_000_000),
                (42_672, 6_000_000),
                (45_424, 8_000_000),
            ]
        );
        assert!(reopens
            .windows(2)
            .all(|pair| pair[1].0 - pair[0].0 >= 1_000));

        // ABR's steps alone, as on a PC whose encoder takes keyframe
        // requests: each reopens on the frame it arrives, as before.
        let mut p = Pipeline::new("h264_nvenc", "h264_nvenc", Instant::now());
        p.opened.1 = 10_000_000;
        p.bitrate = 10_000_000;
        let steps: Vec<_> = steps.into_iter().filter(|(_, s)| s.is_some()).collect();
        assert_eq!(
            play(&mut p, &steps, 51_622),
            [
                (432, 8_000_000),
                (14_416, 4_000_000),
                (29_936, 6_000_000),
                (45_424, 8_000_000),
            ]
        );
    }

    #[test]
    fn a_drop_reopens_at_once_or_within_a_second_of_the_last_open() {
        // Quick Sync takes a new bitrate in place; see
        // `quick_sync_takes_every_bitrate_change_in_place_until_it_refuses_one`.
        for name in ["h264_nvenc", "h264_amf", "hevc_nvenc", "libx265"] {
            let h264 = match name {
                "hevc_nvenc" => "h264_nvenc",
                "libx265" => "libx264",
                other => other,
            };
            // Nothing opened in the last second: the same frame.
            let mut p = Pipeline::new(h264, name, Instant::now());
            assert_eq!(
                play(&mut p, &[(0, Some(10_000_000))], 5_000),
                [(0, 10_000_000)],
                "{name}"
            );
            // 100 ms after a frame rate reopen: 1 s after that reopen.
            let mut p = Pipeline::new(h264, name, Instant::now());
            p.fps = 30;
            let reopens = play(&mut p, &[(112, Some(10_000_000))], 5_000);
            assert_eq!(reopens, [(0, 15_000_000), (1_008, 10_000_000)], "{name}");
        }
    }

    #[test]
    fn a_burst_of_drops_reopens_once_at_the_newest_target() {
        // An AMF keyframe reopen, then three drops within the second.
        let mut p = Pipeline::new("h264_amf", "h264_amf", Instant::now());
        let steps = [
            (0, None),
            (200, Some(12_000_000)),
            (500, Some(10_000_000)),
            (800, Some(8_000_000)),
        ];
        assert_eq!(
            play(&mut p, &steps, 30_000),
            [(0, 15_000_000), (1_008, 8_000_000)]
        );
        // With no open just before it, the first drop goes at once and the
        // rest of the burst follows as one.
        let mut p = Pipeline::new("h264_nvenc", "h264_nvenc", Instant::now());
        let steps = [
            (0, Some(12_000_000)),
            (300, Some(10_000_000)),
            (600, Some(8_000_000)),
        ];
        assert_eq!(
            play(&mut p, &steps, 30_000),
            [(0, 12_000_000), (1_008, 8_000_000)]
        );
    }

    #[test]
    fn a_rise_reopens_at_once() {
        // On a session open for long.
        let mut p = Pipeline::new("h264_nvenc", "h264_nvenc", Instant::now());
        assert_eq!(
            play(&mut p, &[(0, Some(20_000_000))], 5_000),
            [(0, 20_000_000)]
        );
        // 112 ms after a frame rate reopen.
        let mut p = Pipeline::new("h264_amf", "h264_amf", Instant::now());
        p.fps = 30;
        assert_eq!(
            play(&mut p, &[(112, Some(20_000_000))], 5_000),
            [(0, 15_000_000), (112, 20_000_000)]
        );
        // While a drop waits for the second after an open, a rise back above
        // the session's bitrate reopens at once, at the newest target.
        let mut p = Pipeline::new("h264_amf", "h264_amf", Instant::now());
        let steps = [(0, None), (304, Some(10_000_000)), (608, Some(20_000_000))];
        assert_eq!(
            play(&mut p, &steps, 5_000),
            [(0, 15_000_000), (608, 20_000_000)]
        );
    }

    #[test]
    fn a_bitrate_the_user_sets_with_abr_off_applies_as_a_target() {
        // ETERNAL_ABR=0: the target is the user's Max bitrate. A rise
        // reopens on the frame it arrives, even right after an open; a
        // drop waits only for that second.
        let mut abr = crate::transport::abr::AbrController::new(8_000_000, false);
        let mut p = Pipeline::new("h264_nvenc", "h264_nvenc", Instant::now());
        p.opened.1 = abr.current_bps();
        p.bitrate = abr.current_bps();
        p.fps = 30;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        let raised = abr.set_ceiling(20_000_000);
        assert!(raised.changed);
        p.later(100);
        p.bitrate = raised.target_bps;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_nvenc"));
        assert_eq!(p.opened.1, 20_000_000);
        let lowered = abr.set_ceiling(10_000_000);
        assert!(lowered.changed);
        p.later(100);
        p.bitrate = lowered.target_bps;
        assert_eq!(play(&mut p, &[], 2_000), [(912, 10_000_000)]);
    }

    #[test]
    fn codec_and_frame_rate_changes_still_reopen_at_once_at_the_newest_target() {
        // A frame rate reopen, then a drop that waits for the second after
        // it. 112 ms later a frame rate change, an HEVC toggle or an AMF
        // keyframe reopens on the frame it arrives, at the waiting target.
        for change in ["fps", "codec", "keyframe"] {
            let name = if change == "keyframe" {
                "h264_amf"
            } else {
                "h264_nvenc"
            };
            let mut p = Pipeline::new(name, name, Instant::now());
            p.fps = 30;
            assert_eq!(p.frame(Reopen::Opens).as_deref(), Some(name));
            p.bitrate = 10_000_000;
            assert_eq!(play(&mut p, &[], 96), [], "{change}");
            p.later(16);
            match change {
                "fps" => p.fps = 60,
                "codec" => p.hevc_wanted = true,
                _ => p.keyframe = true,
            }
            let expected = if change == "codec" {
                "hevc_nvenc"
            } else {
                name
            };
            assert_eq!(
                p.frame(Reopen::Opens).as_deref(),
                Some(expected),
                "{change}"
            );
            assert_eq!(p.opened.1, 10_000_000, "{change}");
            assert_eq!(play(&mut p, &[], 60_000), [], "{change}");
        }
    }

    #[test]
    fn a_waiting_drop_is_applied_never_dropped() {
        // A drop waits for the second after a keyframe reopen; a second
        // keyframe reopen meanwhile takes it. A drop after that waits for
        // the second after that reopen, then reopens on its own.
        let mut p = Pipeline::new("h264_amf", "h264_amf", Instant::now());
        let steps = [
            (0, None),
            (304, Some(10_000_000)),
            (608, None),
            (704, Some(8_000_000)),
        ];
        assert_eq!(
            play(&mut p, &steps, 30_000),
            [(0, 15_000_000), (608, 10_000_000), (1_616, 8_000_000)]
        );
        // A drop that failed to reopen is not lost either: the next drop
        // reopens at once, the second after an open being long over.
        let mut p = Pipeline::new("h264_nvenc", "h264_nvenc", Instant::now());
        p.bitrate = 10_000_000;
        assert_eq!(p.frame(Reopen::Fails).as_deref(), Some("h264_nvenc"));
        assert_eq!(play(&mut p, &[], 1_000), []);
        assert_eq!(
            play(&mut p, &[(0, Some(8_000_000))], 1_000),
            [(0, 8_000_000)]
        );
    }

    #[test]
    fn libx264_takes_every_bitrate_change_in_place_at_once() {
        // Rises and drops, some within a second of each other: each applies
        // on the frame it arrives, and none reopens.
        let t0 = Instant::now();
        let mut p = Pipeline::new("libx264", "libx264", t0);
        for (at, bitrate) in [
            (0, 12_000_000),
            (100, 10_000_000),
            (5_000, 15_000_000),
            (5_100, 12_000_000),
        ] {
            p.now = t0 + Duration::from_millis(at);
            p.bitrate = bitrate;
            assert_eq!(p.frame(Reopen::Opens), None, "at {at} ms");
            assert_eq!(p.opened.1, bitrate, "at {at} ms");
        }
        assert_eq!(p.in_place, 4);
        assert_eq!(play(&mut p, &[], 60_000), []);
    }

    #[test]
    fn quick_sync_takes_every_bitrate_change_in_place_until_it_refuses_one() {
        for name in ["libx264", "h264_qsv", "hevc_qsv"] {
            assert!(takes_bitrate_in_place(name, false), "{name}");
        }
        for name in [
            "h264_nvenc",
            "hevc_nvenc",
            "h264_amf",
            "hevc_amf",
            "libx265",
        ] {
            assert!(!takes_bitrate_in_place(name, false), "{name}");
        }
        assert!(!takes_bitrate_in_place("h264_qsv", true));
        assert!(takes_bitrate_in_place("libx264", true));
        // As libx264: rises and drops, some within a second of each other,
        // each on the frame it arrives, and no reopen. The field's worst PC
        // reopened Quick Sync 148 times, about 0.6 s of frozen picture each.
        let t0 = Instant::now();
        let mut p = Pipeline::new("h264_qsv", "h264_qsv", t0);
        for (at, bitrate) in [
            (0, 12_000_000),
            (100, 10_000_000),
            (5_000, 15_000_000),
            (5_100, 12_000_000),
        ] {
            p.now = t0 + Duration::from_millis(at);
            p.bitrate = bitrate;
            assert_eq!(p.frame(Reopen::Opens), None, "at {at} ms");
            assert_eq!(p.opened.1, bitrate, "at {at} ms");
        }
        assert_eq!(p.in_place, 4);
        // A frame rate change still reopens.
        p.fps = 30;
        assert_eq!(p.frame(Reopen::Opens).as_deref(), Some("h264_qsv"));
        // Once a session refused a change, a change reopens, at most every
        // 5 s, rises too: here 5 s after that frame rate reopen, and 5 s
        // after the previous bitrate reopen.
        p.qsv_refused = true;
        let reopens = play(
            &mut p,
            &[
                (0, Some(10_000_000)),
                (1_000, Some(14_000_000)),
                (6_000, Some(8_000_000)),
                (6_500, Some(9_000_000)),
            ],
            15_000,
        );
        assert_eq!(reopens, [(5_008, 14_000_000), (10_016, 9_000_000)]);
        assert_eq!(p.in_place, 4);
    }

    #[test]
    fn a_quick_sync_bitrate_reopen_waits_5_s_for_either_direction() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut pacer = BitratePacer::default();
        pacer.opened(t0);
        let qsv = ("h264_qsv", 10_000_000, 60);
        assert_eq!(
            pacer.wanted(qsv, ("h264_qsv", 12_000_000, 60), at(1_000)),
            10_000_000
        );
        assert_eq!(
            pacer.wanted(qsv, ("h264_qsv", 8_000_000, 60), at(4_999)),
            10_000_000
        );
        assert_eq!(
            pacer.wanted(qsv, ("h264_qsv", 8_000_000, 60), at(5_000)),
            8_000_000
        );
        assert_eq!(
            pacer.wanted(qsv, ("h264_qsv", 12_000_000, 60), at(5_000)),
            12_000_000
        );
        // A frame rate or codec change goes ahead at the newest target.
        assert_eq!(
            pacer.wanted(qsv, ("h264_qsv", 12_000_000, 30), at(10)),
            12_000_000
        );
        assert_eq!(
            pacer.wanted(qsv, ("hevc_qsv", 12_000_000, 60), at(10)),
            12_000_000
        );
        // Other encoders keep #165's pacing: a rise at once, a drop 1 s on.
        let nvenc = ("h264_nvenc", 10_000_000, 60);
        assert_eq!(
            pacer.wanted(nvenc, ("h264_nvenc", 12_000_000, 60), at(10)),
            12_000_000
        );
        assert_eq!(
            pacer.wanted(nvenc, ("h264_nvenc", 8_000_000, 60), at(999)),
            10_000_000
        );
        assert_eq!(
            pacer.wanted(nvenc, ("h264_nvenc", 8_000_000, 60), at(1_000)),
            8_000_000
        );
    }

    /// A clock that stands still until the test moves it: the ms since the
    /// test began are in the returned atomic.
    fn paused_clock() -> (
        std::sync::Arc<std::sync::atomic::AtomicU64>,
        impl Fn() -> Instant + Clone + Send + 'static,
    ) {
        let start = Instant::now();
        let ms = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let read = std::sync::Arc::clone(&ms);
        (ms, move || {
            start + Duration::from_millis(read.load(Ordering::SeqCst))
        })
    }

    /// A fake hardware open of `name` in `lanes`, on `clock`, that hangs
    /// until the returned sender sends, then opens.
    fn hung_open<N: Fn() -> Instant>(
        lanes: &'static crate::stall::Lanes,
        name: &'static str,
        opening: Opening,
        clock: N,
    ) -> (
        std::sync::mpsc::Sender<()>,
        crate::stall::Pending<&'static str, N>,
    ) {
        let (release, hang) = stuck_open();
        let pending = crate::stall::start_in(
            lanes,
            crate::stall::Call::EncoderOpen,
            crate::stall::Wait {
                limits: open_limits(name, opening),
                slice: Duration::from_millis(1),
                now: clock,
            },
            move || {
                hang();
                name
            },
            crate::stall::no_hooks(),
        )
        .unwrap();
        (release, pending)
    }

    /// Waits for the pipeline's first open as the encode loop does, as
    /// `opening` says, each 1 ms heartbeat moving the paused clock 100 ms on.
    fn first_open_of<N: Fn() -> Instant>(
        pending: crate::stall::Pending<&'static str, N>,
        opening: Opening,
        ms: &std::sync::atomic::AtomicU64,
    ) -> Result<Result<&'static str, crate::stall::Stall>, crate::stall::Pending<&'static str, N>>
    {
        wait_or_park(pending, opening, &mut || {
            ms.fetch_add(100, Ordering::SeqCst);
        })
    }

    #[test]
    fn time_before_the_open_starts_counts_against_the_first_pictures_wait() {
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        let (ms, clock) = paused_clock();
        let opening = Opening::Probe(clock() + FIRST_PICTURE_WAIT);
        // 2.2 s go by before the open's thread is running, as Windows once
        // took to start one on a loaded PC.
        ms.store(2_200, Ordering::SeqCst);
        let (release, pending) = hung_open(lanes, "h264_nvenc", opening, clock.clone());
        let Err(parked) = first_open_of(pending, opening, &ms) else {
            panic!("the hung open returned")
        };
        // Parked when the picture was due, 3 s after it was asked for, not
        // 3 s after the thread began.
        assert_eq!(ms.load(Ordering::SeqCst), 3_000);
        assert_eq!(parked.running_for(), Duration::from_millis(800));
        release.send(()).unwrap();
        drop(parked);
        wait_for_clear_lane(lanes);
        // Due before the open even starts: parked at once.
        ms.store(4_000, Ordering::SeqCst);
        let (release, pending) = hung_open(lanes, "h264_amf", opening, clock);
        let Err(parked) = wait_or_park(pending, opening, &mut || panic!("it waited")) else {
            panic!("returned")
        };
        assert_eq!(parked.running_for(), Duration::ZERO);
        release.send(()).unwrap();
        drop(parked);
        wait_for_clear_lane(lanes);
    }

    #[test]
    fn a_hung_first_open_leaves_the_picture_to_libx264_when_it_is_due() {
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        let (ms, clock) = paused_clock();
        let ((parked, opened_late), lines) = logged(|| {
            let opening = Opening::Probe(clock() + FIRST_PICTURE_WAIT);
            let (release, pending) = hung_open(lanes, "h264_qsv", opening, clock.clone());
            let Err(mut parked) = first_open_of(pending, opening, &ms) else {
                panic!("the hung open returned")
            };
            // Given up waiting for when the first picture was due, 7 s
            // before the iPad's timeout; not given up on.
            let parked_at = parked.running_for();
            // libx264 needs no lane and opens at once. Every hardware open
            // is refused at once instead of waiting on FFmpeg's lock.
            assert_eq!(
                open_in(lanes, "libx264", Opening::First, || {}).unwrap(),
                "libx264"
            );
            for name in ["h264_nvenc", "h264_amf", "hevc_qsv", "libx265"] {
                let error = open_in(lanes, name, Opening::First, || {}).unwrap_err();
                assert!(is_open_stall(error.as_ref()), "{name}: {error}");
                assert!(error.to_string().contains("not started"), "{error}");
            }
            // The stream: libx264 while QSV opens.
            let t0 = Instant::now();
            let at = |ms: u64| t0 + Duration::from_millis(ms);
            let mut recovery = StallRecovery::default();
            recovery.outwaited("h264_qsv", "libx264", at(3_000));
            for ms in [3_000, 4_000, 5_000, 14_000] {
                assert_eq!(
                    recovery.encoder_for_frame("h264_qsv", at(ms), lane_clear(lanes)),
                    "libx264",
                    "at {ms} ms"
                );
            }
            // QSV opens at 4.4 s, the slowest open in the field, within
            // its 15 s limit: the stream takes that very session.
            ms.store(4_400, Ordering::SeqCst);
            assert!(parked.poll().is_none());
            release.send(()).unwrap();
            wait_for_clear_lane(lanes);
            let opened = parked.poll();
            assert!(recovery.opened("h264_qsv", "h264_qsv"));
            assert!(recovery.owed.is_none());
            assert_eq!(recovery.tries, 0);
            (parked_at, opened)
        });
        assert_eq!(parked, FIRST_PICTURE_WAIT);
        assert_eq!(opened_late, Some(Ok("h264_qsv")));
        assert!(
            !lines.iter().any(|line| line.contains("stall=encoder_open")),
            "{lines:?}"
        );
    }

    #[test]
    fn an_open_that_returns_before_the_first_picture_is_due_is_not_parked() {
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        let (ms, clock) = paused_clock();
        let opening = Opening::Probe(clock() + FIRST_PICTURE_WAIT);
        let (release, pending) = hung_open(lanes, "h264_nvenc", opening, clock);
        // NVENC's slowest open in the field: 2.6 s. The clock stops there,
        // however long the hand-over takes on a loaded machine.
        let mut beats = 0;
        let mut released = false;
        let opened = wait_or_park(pending, opening, &mut || {
            beats += 1;
            if !released && ms.fetch_add(100, Ordering::SeqCst) + 100 >= 2_600 {
                release.send(()).unwrap();
                released = true;
            }
        });
        assert!(matches!(opened, Ok(Ok("h264_nvenc"))));
        assert!(lane_clear(lanes));
        // The watchdog's heartbeat ran meanwhile.
        assert!(beats >= 26, "{beats}");
        // A deadline already past (an earlier encoder of the fallback used
        // the wait up) parks at once; so does a background reopen.
        for behind in [false, true] {
            let (_ms, clock) = paused_clock();
            let opening = if behind {
                Opening::Behind
            } else {
                Opening::Probe(clock())
            };
            let (release, pending) = hung_open(lanes, "h264_amf", opening, clock);
            let Err(parked) = wait_or_park(pending, opening, &mut || panic!("it waited")) else {
                panic!("returned")
            };
            assert_eq!(parked.running_for(), Duration::ZERO);
            release.send(()).unwrap();
            drop(parked);
            wait_for_clear_lane(lanes);
        }
        // A reopen of a live session still waits, to its 9 s limit.
        let (ms, clock) = paused_clock();
        let (release, pending) = hung_open(lanes, "h264_qsv", Opening::Reopen, clock);
        let gave_up = wait_or_park(pending, Opening::Reopen, &mut || {
            ms.fetch_add(100, Ordering::SeqCst);
        });
        let Ok(Err(crate::stall::Stall::TimedOut { after, .. })) = gave_up else {
            panic!("not given up on at the reopen limit")
        };
        assert_eq!(after, Duration::from_secs(9));
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
    }

    #[test]
    fn a_first_open_hung_past_its_limit_is_tried_again_in_the_background() {
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        let (ms, clock) = paused_clock();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut recovery = StallRecovery::default();
        let opening = Opening::Probe(clock() + FIRST_PICTURE_WAIT);
        let (release, pending) = hung_open(lanes, "h264_nvenc", opening, clock.clone());
        let Err(mut parked) = first_open_of(pending, opening, &ms) else {
            panic!("the hung open returned")
        };
        recovery.outwaited("h264_nvenc", "libx264", at(3_000));
        // Given up on at NVENC's 15 s limit, with the one stall line; the
        // lane stays taken until the driver returns.
        ms.store(15_000, Ordering::SeqCst);
        let (result, lines) = logged(|| parked.poll());
        assert!(matches!(
            result,
            Some(Err(crate::stall::Stall::TimedOut { .. }))
        ));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("stall=encoder_open after_ms=15000")),
            "{lines:?}"
        );
        assert!(!lane_clear(lanes));
        assert_eq!(
            recovery.encoder_for_frame("h264_nvenc", at(20_000), false),
            "libx264"
        );
        // The driver returns at 30 s. The try is due: it opens in the
        // background, so libx264 streams on instead of freezing for it.
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        let desired = recovery.encoder_for_frame("h264_nvenc", at(30_000), true);
        assert_eq!(desired, "h264_nvenc");
        assert!(opens_behind("libx264", &desired));
        assert_eq!(
            open_limits("h264_nvenc", Opening::Behind),
            crate::stall::encoder_open_limits("h264_nvenc")
        );
        recovery.starting(&desired);
        ms.store(30_000, Ordering::SeqCst);
        let (release, pending) = hung_open(lanes, "h264_nvenc", Opening::Behind, clock);
        let Err(mut trying) = wait_or_park(pending, Opening::Behind, &mut || {
            panic!("a background open waited")
        }) else {
            panic!("returned")
        };
        assert_eq!(
            recovery.encoder_for_frame("h264_nvenc", at(30_016), lane_clear(lanes)),
            "libx264"
        );
        ms.store(30_200, Ordering::SeqCst);
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        assert_eq!(trying.poll(), Some(Ok("h264_nvenc")));
        assert!(recovery.opened("h264_nvenc", "h264_nvenc"));
        assert!(recovery.owed.is_none());
    }

    #[test]
    fn only_a_switch_from_libx264_into_the_lane_opens_in_the_background() {
        for (current, wanted, behind) in [
            ("libx264", "h264_nvenc", true),
            ("libx264", "hevc_amf", true),
            ("libx264", "h264_qsv", true),
            ("libx264", "libx265", true),
            ("libx264", "libx264", false),
            ("h264_nvenc", "hevc_nvenc", false),
            ("h264_qsv", "h264_qsv", false),
            ("hevc_amf", "libx264", false),
            ("h264_videotoolbox", "hevc_videotoolbox", false),
        ] {
            assert_eq!(
                opens_behind(current, wanted),
                behind,
                "{current} -> {wanted}"
            );
        }
    }

    #[test]
    fn a_hardware_encoder_that_failed_is_remembered_at_its_size_for_the_launch() {
        use GpuVendor::{Amd, Intel, Nvidia};
        let hd = (1920, 1080);
        let mut failures = LaunchFailures::default();
        failures.failed("h264_nvenc", hd, || panic!("NVENC never reads the cards"));
        assert!(failures.earlier("h264_nvenc", hd).is_some());
        // Another size, another encoder: tried.
        assert!(failures.earlier("h264_nvenc", (2732, 2048)).is_none());
        assert!(failures.earlier("hevc_nvenc", hd).is_none());
        assert!(failures.any("_nvenc"));
        assert!(!failures.any("_amf"));
        // Software encoders are never left out.
        failures.failed("libx264", hd, || panic!("read"));
        failures.failed("libx265", hd, || panic!("read"));
        assert!(failures.earlier("libx264", hd).is_none());
        assert!(failures.earlier("libx265", hd).is_none());
        // AMF on a PC with an AMD card: its driver gets the note.
        failures.failed("h264_amf", hd, || Some(vec![Nvidia, Amd]));
        assert_eq!(failures.vendors, [Amd]);
        // Quick Sync on a PC without an Intel card: no note, as that
        // failure is expected there. Still remembered.
        failures.failed("h264_qsv", hd, || Some(vec![Nvidia, Amd]));
        assert_eq!(failures.vendors, [Amd]);
        assert!(failures.earlier("h264_qsv", hd).is_some());
        // Unknown cards (DXGI listed some, not all): no note.
        failures.failed("hevc_qsv", hd, || None);
        assert_eq!(failures.vendors, [Amd]);
        failures.failed("h264_qsv", (1280, 720), || Some(vec![Intel]));
        assert_eq!(failures.vendors, [Amd, Intel]);
        // Each failure is kept once, and the cards are read once a vendor.
        failures.failed("hevc_amf", hd, || panic!("read again"));
        failures.failed("h264_nvenc", hd, || panic!("read"));
        assert_eq!(
            failures
                .opens
                .iter()
                .filter(|(name, size)| name == "h264_nvenc" && *size == hd)
                .count(),
            1
        );
        let why = failures.earlier("h264_amf", hd).unwrap();
        assert!(
            why.contains("h264_amf failed to open at 1920x1080 earlier in this launch"),
            "{why}"
        );
        assert!(why.contains("Restart"), "{why}");
    }

    #[test]
    fn an_encoder_that_failed_to_open_is_not_opened_again_or_logged_again() {
        ffmpeg_next::init().unwrap();
        crate::diagnostics::install_ffmpeg_logging();
        let failures = parking_lot::Mutex::new(LaunchFailures::default());
        // A real open that fails: Quick Sync on a PC without an Intel card
        // ("Error creating a MFX session"), or not in this FFmpeg build
        // (macOS). Both are the encoder's own failure.
        let open = |failures: &parking_lot::Mutex<LaunchFailures>| {
            logged(|| {
                open_session(
                    failures,
                    "h264_qsv",
                    1280,
                    720,
                    8_000_000,
                    60,
                    &mut EncoderInput::Yuv420,
                    Opening::First,
                    &mut || {},
                )
                .err()
                .map(|error| error.to_string())
            })
        };
        let (Some(first), first_lines) = open(&failures) else {
            eprintln!("Quick Sync opens on this PC; nothing to remember");
            return;
        };
        eprintln!("first open: {first}");
        assert!(failures.lock().earlier("h264_qsv", (1280, 720)).is_some());
        // Every pipeline after it: no open, no FFmpeg line, and the reason.
        for _ in 0..3 {
            let (again, lines) = open(&failures);
            let again = again.expect("opened");
            assert!(again.contains("earlier in this launch"), "{again}");
            assert!(
                !lines
                    .iter()
                    .any(|line| line.contains("ffmpeg") || line.contains("Opening encoder session")),
                "{lines:?}"
            );
        }
        eprintln!(
            "first open logged {} FFmpeg lines",
            first_lines
                .iter()
                .filter(|line| line.contains("ffmpeg"))
                .count()
        );
        // libx264 is never held back, and a Restart tries Quick Sync again.
        assert!(open_session(
            &failures,
            "libx264",
            1280,
            720,
            8_000_000,
            60,
            &mut EncoderInput::Yuv420,
            Opening::First,
            &mut || {},
        )
        .is_ok());
        *failures.lock() = LaunchFailures::default();
        let (retried, _) = open(&failures);
        let retried = retried.expect("opened");
        assert!(!retried.contains("earlier in this launch"), "{retried}");
    }

    /// A background open in `lanes` of `name` at `pixel_format`, parked
    /// already, that hangs until the returned sender sends, then gives `open`.
    fn parked_background_open(
        lanes: &'static crate::stall::Lanes,
        name: &str,
        pixel_format: ffmpeg_next::format::Pixel,
        open: impl FnOnce() -> Result<ffmpeg_next::codec::encoder::video::Encoder, ffmpeg_next::Error>
            + Send
            + 'static,
    ) -> (std::sync::mpsc::Sender<()>, BackgroundOpen) {
        let (release, hang) = stuck_open();
        let mut pending = crate::stall::start_in(
            lanes,
            crate::stall::Call::EncoderOpen,
            crate::stall::Wait {
                limits: open_limits(name, Opening::Behind),
                slice: Duration::from_millis(1),
                now: Instant::now as fn() -> Instant,
            },
            move || {
                hang();
                open()
            },
            crate::stall::no_hooks(),
        )
        .unwrap();
        assert!(pending.park().is_none());
        let args = SessionArgs {
            encoder_name: name.to_string(),
            size: (64, 64),
            bitrate_bps: 8_000_000,
            target_fps: 60,
            pixel_format,
            legacy_pts: false,
        };
        (
            release,
            BackgroundOpen {
                pending,
                args,
                yuv_retry: false,
            },
        )
    }

    /// A real libx264 session at 64x64 YUV420P, opened on the call's thread
    /// as a hardware encoder would be.
    fn real_yuv_encoder() -> Result<ffmpeg_next::codec::encoder::video::Encoder, ffmpeg_next::Error>
    {
        let codec = ffmpeg_next::encoder::find_by_name("libx264").expect("libx264");
        let mut video = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder()
            .video()?;
        video.set_width(64);
        video.set_height(64);
        video.set_format(ffmpeg_next::format::Pixel::YUV420P);
        video.set_time_base(ffmpeg_next::Rational(1, 60));
        video.open_with(encoder_options("libx264"))
    }

    #[test]
    fn a_bgra_open_that_fails_in_the_background_is_tried_again_at_yuv420_there() {
        use ffmpeg_next::format::Pixel;
        ffmpeg_next::init().unwrap();
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        let refused = || {
            Err(ffmpeg_next::Error::Other {
                errno: libc::EINVAL,
            })
        };
        // NVENC's Automatic BGRA open outlived the first picture's wait, and
        // fails after it, as an NVENC that takes only YUV420 does.
        let (release, mut open) = parked_background_open(lanes, "h264_nvenc", Pixel::BGRA, refused);
        open.yuv_retry = true;
        let mut input = EncoderInput::Automatic;
        assert!(open
            .poll(&mut input, |_| panic!("tried again before it failed"))
            .is_none());
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        // YUV420 is tried at once, in the background too, and the pipeline
        // keeps it; the BGRA failure is not handed back to be remembered.
        let mut retried_at = None;
        let mut yuv_release = None;
        let polled = open.poll(&mut input, |args| {
            retried_at = Some((args.encoder_name.clone(), args.size, args.bitrate_bps));
            let (release, yuv) =
                parked_background_open(lanes, &args.encoder_name, Pixel::YUV420P, real_yuv_encoder);
            yuv_release = Some(release);
            Err(Box::new(StillOpening(
                args.encoder_name.clone(),
                parking_lot::Mutex::new(Some(yuv)),
            )))
        });
        assert!(polled.is_none(), "the BGRA failure was handed back");
        assert_eq!(
            retried_at,
            Some(("h264_nvenc".to_string(), (64, 64), 8_000_000))
        );
        assert_eq!(input, EncoderInput::Yuv420);
        assert_eq!(open.args.pixel_format, Pixel::YUV420P);
        assert!(open
            .poll(&mut input, |_| panic!("tried again while YUV420 opens"))
            .is_none());
        // It opens: the stream takes that session.
        yuv_release.unwrap().send(()).unwrap();
        wait_for_clear_lane(lanes);
        let state = open
            .poll(&mut input, |_| panic!("YUV420 is tried once"))
            .expect("returned")
            .expect("the YUV420 open failed");
        assert_eq!(state.opened_encoder, "h264_nvenc");
        assert_eq!(state.frame.format(), Pixel::YUV420P);

        // When YUV420 fails too, that failure is handed back, to be
        // remembered for the launch.
        let (release, mut open) = parked_background_open(lanes, "h264_nvenc", Pixel::BGRA, refused);
        open.yuv_retry = true;
        let mut input = EncoderInput::Automatic;
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        let failed = open.poll(&mut input, |_| {
            Err("h264_nvenc open failed at YUV420".into())
        });
        let error = failed.expect("returned").err().expect("it opened");
        assert!(error.to_string().contains("at YUV420"), "{error}");
        assert!(!is_open_stall(error.as_ref()));
        assert_eq!(input, EncoderInput::Yuv420);

        // A YUV420 open, or a picked BGRA, is not tried again; nor is a
        // stall, which says nothing about BGRA.
        let (release, mut open) = parked_background_open(lanes, "h264_nvenc", Pixel::BGRA, refused);
        let mut input = EncoderInput::Bgra;
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        let failed = open.poll(&mut input, |_| panic!("a picked BGRA was tried again"));
        assert!(failed.expect("returned").is_err());
        assert_eq!(input, EncoderInput::Bgra);
    }

    #[test]
    fn an_hevc_open_in_the_background_is_dropped_once_the_frame_wants_h264() {
        use ffmpeg_next::format::Pixel;
        ffmpeg_next::init().unwrap();
        let lanes: &'static crate::stall::Lanes = Box::leak(Box::new(crate::stall::Lanes::new()));
        // An H.264 encoder whose H.265 sibling this FFmpeg build has.
        let Some(h264) = ["libx264", "h264_nvenc", "h264_videotoolbox"]
            .into_iter()
            .find(|name| {
                ffmpeg_next::encoder::find_by_name(hevc_variant_of(name).unwrap()).is_some()
            })
        else {
            eprintln!("no H.265 encoder in this FFmpeg build");
            return;
        };
        // The encode loop's negotiation, from the user's preference and the
        // client's decoding.
        let preferred = |hevc_enabled: bool, client_hevc: bool| {
            preferred_encoder(h264, hevc_enabled && client_hevc, false, &mut true)
        };
        let hevc = preferred(true, true);
        assert!(is_hevc_encoder(&hevc), "{hevc}");
        for change in ["an H.264-only client connected", "HEVC was turned off"] {
            let (release, open) =
                parked_background_open(lanes, &hevc, Pixel::YUV420P, real_yuv_encoder);
            let mut background = Some(Behind::first(open, 8_000_000));
            assert!(!drop_obsolete_background(
                &mut background,
                &preferred(true, true)
            ));
            assert!(background.is_some(), "{change}: dropped while wanted");
            let now = match change {
                "an H.264-only client connected" => preferred(true, false),
                _ => preferred(false, true),
            };
            assert_eq!(now, h264, "{change}");
            // The HEVC session opens meanwhile, ready to be taken.
            release.send(()).unwrap();
            wait_for_clear_lane(lanes);
            assert!(
                drop_obsolete_background(&mut background, &now),
                "{change}: the HEVC session would replace one the client decodes"
            );
            assert!(background.is_none(), "{change}");
        }
        // Still wanted, it is taken once it opens.
        let (release, open) =
            parked_background_open(lanes, &hevc, Pixel::YUV420P, real_yuv_encoder);
        let mut background = Some(Behind::first(open, 8_000_000));
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
        assert!(!drop_obsolete_background(&mut background, &hevc));
        let state = background
            .as_mut()
            .unwrap()
            .open
            .poll(&mut EncoderInput::Yuv420, |_| panic!("tried again"))
            .expect("returned")
            .expect("opened");
        assert_eq!(state.opened_encoder, hevc);
        // An H.264 open is kept when HEVC is turned on: the client decodes
        // it, and the stream reopens to HEVC from there.
        let (release, open) = parked_background_open(lanes, h264, Pixel::YUV420P, real_yuv_encoder);
        let mut background = Some(Behind::first(open, 8_000_000));
        assert!(!drop_obsolete_background(&mut background, &hevc));
        assert!(background.is_some());
        release.send(()).unwrap();
        wait_for_clear_lane(lanes);
    }

    #[test]
    fn the_reopen_line_says_why() {
        let nvenc = ("h264_nvenc", 60);
        let x264 = ("libx264", 60);
        assert_eq!(reopen_reason(nvenc, nvenc, false, false, false), "bitrate");
        assert_eq!(
            reopen_reason(nvenc, ("h264_nvenc", 30), false, false, false),
            "fps"
        );
        assert_eq!(
            reopen_reason(nvenc, ("hevc_nvenc", 60), false, false, false),
            "codec"
        );
        assert_eq!(reopen_reason(x264, nvenc, false, false, true), "recovery");
        assert_eq!(
            reopen_reason(x264, ("hevc_nvenc", 60), false, false, true),
            "recovery"
        );
        let amf = ("h264_amf", 60);
        assert_eq!(reopen_reason(amf, amf, false, true, false), "keyframe");
        assert_eq!(
            reopen_reason(nvenc, ("hevc_nvenc", 30), true, true, false),
            "size"
        );
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
        crate::logging::prepare_log_capture();
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
            let opened = now(open_encoder_on(
                lanes,
                video,
                "libx265",
                Opening::First,
                &mut || {},
            ));
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
            now(open_encoder_on(
                lanes,
                video,
                "libx264",
                Opening::First,
                &mut || {}
            )),
            Ok(Ok(_))
        ));
        let _ = release.send(());
    }

    #[test]
    fn startup_uses_software_only_after_all_other_hardware_fails() {
        for failed in ["h264_nvenc", "h264_amf", "h264_qsv"] {
            let mut attempts = Vec::new();
            let (selected, ()) = open_remaining_encoders((failed, "test"), no_list, |name| {
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
        assert!(
            open_remaining_encoders::<()>(("h264_amf", "test"), no_list, |_| {
                Err("no encoder can open".into())
            })
            .is_err()
        );
    }

    #[test]
    fn non_windows_backends_keep_their_software_fallback() {
        let mut attempts = Vec::new();
        let (selected, ()) =
            open_remaining_encoders(("h264_videotoolbox", "test"), no_list, |name| {
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
            &mut EncoderInput::Yuv420,
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
        for name in ["h264_nvenc", "h264_amf", "libx265"] {
            state.opened_encoder = name.to_string();
            assert!(!state.update_bitrate_without_reopening(name, 4_000_000, 60));
        }
        // Quick Sync takes it in place, to be confirmed by its next frame.
        state.opened_encoder = "h264_qsv".to_string();
        assert!(!state.bitrate_reset_unconfirmed);
        assert!(state.update_bitrate_without_reopening("h264_qsv", 4_000_000, 60));
        assert_eq!(state.opened_bitrate, 4_000_000);
        assert!(state.bitrate_reset_unconfirmed);
        assert!(!state.update_bitrate_without_reopening("h264_qsv", 4_000_000, 60));
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

    /// Automatic's BGRA open failing on NVENC opens YUV420P at once and keeps
    /// it for later opens; a picked input, a stall, or a YUV420P open that
    /// fails is not tried again.
    #[test]
    fn a_failed_automatic_bgra_open_falls_back_to_yuv420_and_stays_there() {
        use ffmpeg_next::format::Pixel;
        let nvenc = [Pixel::YUV420P, Pixel::NV12, Pixel::BGRZ, Pixel::BGRA];
        type Opened = Result<Pixel, Box<dyn std::error::Error + Send + Sync>>;
        let refuse_bgra = |tried: &mut Vec<Pixel>, format: Pixel| -> Opened {
            tried.push(format);
            if format == Pixel::BGRA {
                Err("h264_nvenc open failed: FFmpeg code=-22".into())
            } else {
                Ok(format)
            }
        };

        let mut input = EncoderInput::Automatic;
        let mut tried = Vec::new();
        let opened = open_at_input(&mut input, "h264_nvenc", &nvenc, |format| {
            refuse_bgra(&mut tried, format)
        });
        assert_eq!(opened.unwrap(), Pixel::YUV420P);
        assert_eq!(tried, [Pixel::BGRA, Pixel::YUV420P]);
        assert_eq!(input, EncoderInput::Yuv420);
        // A reopen (bitrate, HEVC) goes straight to YUV420P.
        tried.clear();
        let opened = open_at_input(&mut input, "hevc_nvenc", &nvenc, |format| {
            refuse_bgra(&mut tried, format)
        });
        assert_eq!(opened.unwrap(), Pixel::YUV420P);
        assert_eq!(tried, [Pixel::YUV420P]);

        // A BGRA that opens stays.
        let mut input = EncoderInput::Automatic;
        let opened = open_at_input(&mut input, "h264_nvenc", &nvenc, Ok);
        assert_eq!(opened.unwrap(), Pixel::BGRA);
        assert_eq!(input, EncoderInput::Automatic);

        // A picked BGRA fails as before: the next encoder is tried instead.
        let mut input = EncoderInput::Bgra;
        tried.clear();
        assert!(open_at_input(&mut input, "h264_nvenc", &nvenc, |format| {
            refuse_bgra(&mut tried, format)
        })
        .is_err());
        assert_eq!(tried, [Pixel::BGRA]);
        assert_eq!(input, EncoderInput::Bgra);

        // A stall says nothing about BGRA, and holds FFmpeg's lock.
        let mut input = EncoderInput::Automatic;
        let mut tries = 0;
        let stalled = open_at_input(&mut input, "h264_nvenc", &nvenc, |_| -> Opened {
            tries += 1;
            Err(Box::new(OpenStalled(
                "h264_nvenc open failed: stalled".into(),
            )))
        });
        assert!(is_open_stall(stalled.unwrap_err().as_ref()));
        assert_eq!(tries, 1);
        assert_eq!(input, EncoderInput::Automatic);

        // Other encoders never had BGRA, so their failure is theirs.
        let mut input = EncoderInput::Automatic;
        let mut tried = Vec::new();
        assert!(open_at_input(
            &mut input,
            "h264_amf",
            &[Pixel::NV12, Pixel::YUV420P],
            |format| -> Opened {
                tried.push(format);
                Err("h264_amf open failed".into())
            }
        )
        .is_err());
        assert_eq!(tried, [Pixel::YUV420P]);
        assert_eq!(input, EncoderInput::Automatic);
    }

    /// ETERNAL_ENCODER=hevc_nvenc streams HEVC: its label says so, and HEVC
    /// stays on it instead of looking for an H.265 sibling it doesn't have.
    #[test]
    fn an_hevc_override_is_labelled_and_streamed_as_hevc() {
        for (name, label) in [
            ("hevc_nvenc", "HEVC (NVENC)"),
            ("hevc_amf", "HEVC (AMF)"),
            ("hevc_qsv", "HEVC (QSV)"),
            ("libx265", "HEVC (x265)"),
        ] {
            assert_eq!(codec_display_name_for(name), label);
            assert_eq!(hevc_encoder_for(name), Some(name));
        }
        for name in ["h264_nvenc", "h264_amf", "h264_qsv", "libx264"] {
            assert!(codec_display_name_for(name).starts_with("H.264 ("));
            assert_eq!(hevc_encoder_for(name), hevc_variant_of(name));
        }
        // Still unknown, so still warned about.
        assert_eq!(codec_display_name_for("mystery_codec"), "mystery_codec");
        assert_eq!(hevc_encoder_for("mystery_codec"), None);
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
    fn a_resume_keyframe_request_reopens_only_amf_and_only_after_its_cooldown() {
        // The encode loop's keyframe calls, one frame every 16 ms. The session
        // grants the iPad's requests (Resume among them) at most every 500 ms
        // and sets `force_next_idr` for each; the loop reopens AMF for one at
        // most every 500 ms and forces an IDR on the others' open session.
        let grants = [0, 500, 1_000, 1_600];
        for name in ["h264_amf", "hevc_amf", "h264_nvenc", "h264_qsv", "libx264"] {
            let mut pending = false;
            let mut frames_submitted = 300;
            let mut last_keyframe_reopen: Option<u64> = None;
            let (mut reopens, mut idrs) = (Vec::new(), 0);
            for now in (0..3_000).step_by(16) {
                pending |= grants.iter().any(|&at| (now..now + 16).contains(&at));
                let since = last_keyframe_reopen.map(|at| Duration::from_millis(now - at));
                if pending && reopen_for_keyframe(name, frames_submitted, since) {
                    last_keyframe_reopen = Some(now);
                    frames_submitted = 0;
                    reopens.push(now);
                }
                if take_keyframe_request(&mut pending, name, frames_submitted) {
                    idrs += 1;
                }
                frames_submitted += 1;
            }
            assert_eq!(idrs, grants.len(), "{name}: one IDR per grant");
            if ignores_forced_keyframes(name) {
                assert_eq!(reopens, [0, 512, 1_024, 1_600], "{name}: 500 ms apart");
            } else {
                assert!(reopens.is_empty(), "{name}");
            }
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

    fn raw(frame_number: u64, data: &std::sync::Arc<Vec<u8>>) -> RawFrame {
        RawFrame {
            frame_number,
            timestamp: Instant::now(),
            data: std::sync::Arc::clone(data),
            width: 2,
            height: 2,
        }
    }

    /// A picture given again to bring out a keyframe keeps its capture time
    /// and number, but is a separate frame to the encoder: it must not share
    /// the PTS of the frame before it, under ETERNAL_LEGACY_PTS too.
    #[test]
    fn a_picture_given_again_gets_its_own_pts_in_both_pts_modes() {
        ffmpeg_next::init().unwrap();
        for legacy in [false, true] {
            let mut state = libx264_at(640, 360);
            state.legacy_pts = legacy;
            let captured = Instant::now();
            // Frame 7, given again twice, then capture's next frame.
            let frames = [(captured, 7), (captured, 7), (captured, 7)]
                .into_iter()
                .chain([(captured + Duration::from_millis(16), 8)]);
            let mut pts = Vec::new();
            for (at, n) in frames {
                let next = state.next_pts(at, n);
                state.frame.set_pts(Some(next));
                state.send_frame(next, at, n).unwrap();
                while state.encoder.receive_packet(&mut state.packet).is_ok() {}
                pts.push(next);
            }
            assert!(
                pts.windows(2).all(|pair| pair[0] < pair[1]),
                "legacy={legacy}: PTS {pts:?}"
            );
        }
    }

    /// Capture recycles three buffers, and the mailbox may hold one while the
    /// encoder works on another. A kept picture that outlived a newer capture
    /// took the third: the synthetic source found no free buffer and panicked.
    #[test]
    fn a_newer_capture_releases_the_kept_picture_before_it_is_encoded() {
        let (producer, consumer) = crate::capture::frame_slot();
        let kept = std::sync::Arc::new(vec![0u8; 16]);
        let newer = std::sync::Arc::new(vec![0u8; 16]);
        let mut owed = OwedKeyframe::until(Instant::now() + Duration::from_secs(5));
        owed.picture = Some(raw(1, &kept));
        producer.publish(raw(2, &newer));
        let Next::Frame(frame) = owed.next(&consumer) else {
            panic!("no frame");
        };
        assert_eq!(frame.frame_number, 2);
        assert_eq!(
            std::sync::Arc::strong_count(&kept),
            1,
            "the kept picture still pins a capture buffer while the newer one is encoded"
        );
    }

    #[test]
    fn the_deadline_counts_from_the_first_keyframe_submission() {
        let kept = std::sync::Arc::new(vec![0u8; 16]);
        let submitted = Instant::now()
            .checked_sub(Duration::from_millis(200))
            .unwrap();
        // The keyframe went in 200 ms ago; sending and receiving took that long.
        let owed = OwedKeyframe::after_encode(None, Some(raw(1, &kept)), false, submitted)
            .expect("the keyframe is owed");
        assert_eq!(owed.until, submitted + KEYFRAME_FLUSH_FOR);
        // A retry submitted later keeps the first deadline.
        let owed =
            OwedKeyframe::after_encode(Some(owed), Some(raw(1, &kept)), false, Instant::now())
                .expect("still owed");
        assert_eq!(owed.until, submitted + KEYFRAME_FLUSH_FOR);
        assert!(
            OwedKeyframe::after_encode(Some(owed), Some(raw(1, &kept)), true, Instant::now())
                .is_none(),
            "the keyframe came out"
        );
        assert!(OwedKeyframe::after_encode(None, None, false, submitted).is_none());
    }

    #[test]
    fn the_kept_picture_is_given_again_before_the_deadline() {
        let (_producer, consumer) = crate::capture::frame_slot();
        let kept = std::sync::Arc::new(vec![0u8; 16]);
        let mut owed = OwedKeyframe::until(Instant::now() + Duration::from_secs(60));
        owed.picture = Some(raw(1, &kept));
        let started = Instant::now();
        let Next::Frame(frame) = owed.next(&consumer) else {
            panic!("the picture was not given again");
        };
        assert_eq!(frame.frame_number, 1);
        assert_eq!(owed.given, 1);
        assert!(
            started.elapsed() >= KEYFRAME_FLUSH_WAIT,
            "it did not wait for capture first"
        );
    }

    #[test]
    fn the_kept_picture_is_not_given_again_after_the_deadline() {
        let (_producer, consumer) = crate::capture::frame_slot();
        let kept = std::sync::Arc::new(vec![0u8; 16]);
        let passed = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        let mut owed = OwedKeyframe::until(passed);
        owed.picture = Some(raw(1, &kept));
        assert!(matches!(owed.next(&consumer), Next::GaveUp));
        assert_eq!(owed.given, 0);
        assert_eq!(
            std::sync::Arc::strong_count(&kept),
            1,
            "the picture is let go"
        );
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
            let mut data = crate::capture::frame_buffer(1280 * 720 * 4);
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
    /// held for twice its limit (`ETERNAL_SIMULATE_STALL`). The stage streams
    /// on software once the first picture is due, gives the open up at its
    /// limit, closes the late NVENC session on its own thread, and then goes
    /// back to NVENC through an open in the background.
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
            "streaming on libx264 while it opens in the background",
            "stall=encoder_open",
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
        let [outwaited, stalled, ended, back] = seen.map(Option::unwrap);
        let first_packet = stage.packets.lock().first().copied().expect("no packet");
        // A picture throughout: x264 while the open was stuck, NVENC after.
        let on_x264 = stage.packets_between(outwaited + Duration::from_secs(1), ended);
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
        assert!(
            text.contains("Opening the encoder in the background"),
            "the try froze the stream:\n{text}"
        );
        eprintln!(
            "first picture at {first_packet:?}, x264 from {outwaited:?}, stalled at {stalled:?}, \
             stall ended at {ended:?}, back on NVENC at {back:?}"
        );
        // The first picture well before the iPad's 10 s timeout: 3 s, then
        // libx264's open and first frame.
        assert!(
            outwaited >= Duration::from_secs(3) && outwaited < Duration::from_secs(5),
            "{outwaited:?}"
        );
        assert!(first_packet < Duration::from_secs(5), "{first_packet:?}");
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

    /// Native, on a PC with NVENC: the first NVENC open takes 6 s, past the
    /// first picture's 3 s but within its 15 s limit. The stage streams on
    /// software from 3 s and switches to that same NVENC session when it
    /// opens, with no stall and no second open.
    /// `cargo test -p eternal-host --lib a_slow_real_nvenc_open -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore = "native: needs an NVIDIA GPU"]
    fn a_slow_real_nvenc_open_is_taken_once_it_opens() {
        use std::time::Duration;
        std::env::set_var("ETERNAL_SIMULATE_STALL", "encoder_open");
        std::env::set_var("ETERNAL_SIMULATE_STALL_MS", "6000");
        let mut stage = NativeStage::start();
        let (outwaited, from) = stage.publish_until(
            "streaming on libx264 while it opens in the background",
            0,
            Duration::from_secs(20),
        );
        let (switched, _) =
            stage.publish_until("in_background=true", from, Duration::from_secs(20));
        for _ in 0..30 {
            stage.publish();
        }
        let first_packet = stage.packets.lock().first().copied().expect("no packet");
        let on_x264 = stage.packets_between(outwaited + Duration::from_millis(500), switched);
        let after = stage.packets_between(switched, Duration::MAX);
        let text = stage.finish();
        eprintln!(
            "first picture at {first_packet:?}, x264 from {outwaited:?}, NVENC from {switched:?}; \
             {on_x264} packets on x264, {after} after"
        );
        assert!(first_packet < Duration::from_secs(5), "{first_packet:?}");
        assert!(
            switched >= Duration::from_secs(6) && switched < Duration::from_secs(8),
            "{switched:?}"
        );
        assert!(on_x264 >= 30, "{on_x264} packets on x264");
        assert!(after >= 20, "{after} packets on NVENC");
        let line = text
            .lines()
            .find(|line| line.contains("in_background=true"))
            .unwrap();
        assert!(line.contains("Reopened encoder session"), "{line}");
        assert!(line.contains("h264_nvenc"), "{line}");
        assert!(!text.contains("stall=encoder_open"), "{text}");
        // NVENC opened once: the session that went on in the background.
        assert_eq!(
            text.matches("Encoder opened").count(),
            2,
            "x264 and NVENC, once each:\n{text}"
        );
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
        // NVENC streams before the ABR step. A first open slower than the
        // first picture's wait streams libx264 until NVENC's session, opened
        // in the background, takes over.
        let mut from = 0;
        let opened = loop {
            let (_, at) = stage.publish_until("Encoder opened", from, Duration::from_secs(20));
            if stage.text()[at..]
                .lines()
                .next()
                .unwrap()
                .contains("h264_nvenc")
            {
                break at;
            }
            from = at + 1;
        };
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
