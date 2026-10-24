//! Direct VideoToolbox encoder: the macOS host's default backend.
//!
//! FFmpeg's VideoToolbox wrapper queues 4-15 frames (67-134 ms at full Duo
//! size), never sets `ExpectedFrameRate` (so its auto level is too low at
//! 120 fps), and must reopen to change bitrate. A session of our own holds
//! 2-5 frames, emits a legal level, and changes bitrate live
//! (`spikes/mac-host/RESULTS.md`).
//!
//! The encode loop mirrors `run_encode_loop` in the parent module: it takes
//! BGRA frames from the capture slot, honours keyframe requests, follows the
//! ABR bitrate, the frame rate and the negotiated codec, and sends Annex B
//! access units to the transport as `NALUnit`s. Output arrives on
//! VideoToolbox's callback thread, so a forwarder thread sends it on; a
//! frame that finishes while capture is idle goes out at once instead of
//! waiting for the next capture frame.

mod bitstream;
pub mod config;
mod ffi;
pub mod session;

#[cfg(test)]
mod hardware_tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use self::bitstream::ProfileTierLevel;
use self::config::{
    plan_change, plan_session, Codec, HevcLimit, Limits, PlanChange, SessionPlan, Tier,
};
use self::session::{FrameInfo, OutputQueue, Tuning, VtError, VtSession};
use super::NALUnit;
use crate::capture::FrameSlot;
use crate::control::SharedControl;
use crate::stats::PIPELINE_STATS;

/// Frames inside the encoder plus encoded frames the transport has not
/// taken yet. Beyond this the loop waits, as the FFmpeg loop does when its
/// send blocks, and then encodes the freshest capture.
const MAX_PENDING_FRAMES: usize = 8;
/// Pause before reopening after VideoToolbox reports itself unavailable.
const REOPEN_PAUSE: Duration = Duration::from_millis(250);
/// About ten seconds of failed opens before the supervisor takes over.
const MAX_FAILED_OPENS: u32 = 40;
/// Consecutive frames VideoToolbox refuses before the stage gives up.
const MAX_FAILED_FRAMES: u32 = 30;

/// Whether this pipeline uses the direct backend: when the host selected
/// VideoToolbox (`GpuInfo::detect` always does on macOS) or an override asks
/// for `videotoolbox`. An explicit override that FFmpeg provides (the tests'
/// libx264, say) keeps the FFmpeg loop, as does `ETERNAL_MAC_ENCODER=ffmpeg`,
/// the escape hatch to FFmpeg's own VideoToolbox wrapper.
pub fn is_selected(override_name: Option<&str>, selected_encoder: &str) -> bool {
    let ffmpeg_requested = std::env::var("ETERNAL_MAC_ENCODER")
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case("ffmpeg"));
    selects_direct(override_name, selected_encoder, ffmpeg_requested, |name| {
        ffmpeg_next::encoder::find_by_name(name).is_some()
    })
}

fn selects_direct(
    override_name: Option<&str>,
    selected_encoder: &str,
    ffmpeg_requested: bool,
    ffmpeg_has: impl Fn(&str) -> bool,
) -> bool {
    if ffmpeg_requested {
        return false;
    }
    match override_name {
        Some("videotoolbox") => true,
        Some(name) if ffmpeg_has(name) => false,
        // No override, or one this FFmpeg lacks (the FFmpeg loop ignores
        // those too): follow the host's own selection.
        _ => matches!(
            selected_encoder,
            "videotoolbox" | "h264_videotoolbox" | "hevc_videotoolbox"
        ),
    }
}

/// Sends encoded frames to the transport from their own thread.
struct Forwarder {
    output: Arc<OutputQueue>,
    receiver_gone: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Forwarder {
    fn spawn(
        output: Arc<OutputQueue>,
        tx: mpsc::Sender<NALUnit>,
        shared: SharedControl,
        generation: u64,
    ) -> std::io::Result<Self> {
        let receiver_gone = Arc::new(AtomicBool::new(false));
        let thread = {
            let output = output.clone();
            let receiver_gone = receiver_gone.clone();
            std::thread::Builder::new()
                .name(format!("encode-out-g{generation}"))
                .spawn(move || {
                    crate::capture::timing::set_stream_qos();
                    while let Some(frame) = output.pop_blocking() {
                        if receiver_gone.load(Ordering::Relaxed) {
                            continue;
                        }
                        let bitrate = shared.abr_current_bps.load(Ordering::Relaxed);
                        PIPELINE_STATS.lock().record_encode(
                            frame.encode_us,
                            frame.data.len(),
                            bitrate,
                        );
                        crate::capture::heartbeat(&shared.hb_encode_frame_ms);
                        debug!(
                            frame = frame.frame_number,
                            encode_us = frame.encode_us,
                            nal_bytes = frame.data.len(),
                            keyframe = frame.keyframe,
                            "Frame encoded"
                        );
                        let nal = NALUnit {
                            data: frame.data,
                            sequence: frame.frame_number,
                            timestamp: frame.captured,
                            encode_duration_us: frame.encode_us,
                            is_keyframe: frame.keyframe,
                        };
                        if tx.blocking_send(nal).is_err() {
                            info!("NAL channel closed, stopping encoder");
                            receiver_gone.store(true, Ordering::Relaxed);
                        }
                    }
                })?
        };
        Ok(Self {
            output,
            receiver_gone,
            thread: Some(thread),
        })
    }

    fn receiver_gone(&self) -> bool {
        self.receiver_gone.load(Ordering::Relaxed)
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.output.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// After every open: the codec and the planned rate go on the wire
/// (HELLO_ACK/heartbeat `StreamConfig`), the codec into the GUI too.
fn note_session_opened(shared: &SharedControl, plan: &SessionPlan) {
    shared.active_codec.store(
        match plan.codec {
            Codec::Hevc => eternal_wire::v2::control::CODEC_HEVC,
            Codec::H264 => eternal_wire::v2::control::CODEC_H264,
        },
        Ordering::SeqCst,
    );
    note_frame_rate(shared, plan.encode_fps);
    PIPELINE_STATS
        .lock()
        .set_codec_name(plan.codec.display_name());
}

/// The rate the iPad's assembler times gaps and repairs by: what the
/// session encodes at, not the 120 a capped request asked for.
fn note_frame_rate(shared: &SharedControl, encode_fps: u32) {
    shared.active_fps.store(encode_fps, Ordering::SeqCst);
}

/// What the profile, tier and level VideoToolbox wrote mean for a session.
/// The level and the tier are judged independently: a level below the plan
/// never hides a tier that needs the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EmittedCheck {
    /// The level is below the plan's, which Apple decoders may reject.
    below_plan: bool,
    /// HEVC High tier without the experimental opt-in: reopen below this rate.
    lower_rate_to: Option<u32>,
}

fn check_emitted(
    level: ProfileTierLevel,
    opened: &SessionPlan,
    hevc_limit: HevcLimit,
) -> EmittedCheck {
    let tier = if level.high_tier {
        Tier::High
    } else {
        Tier::Main
    };
    EmittedCheck {
        below_plan: level.level_idc < opened.level.idc,
        lower_rate_to: (!hevc_limit.allows(tier))
            .then(|| opened.encode_fps.saturating_sub(1).max(1)),
    }
}

pub(super) fn run_encode_loop(
    frames: FrameSlot,
    tx: mpsc::Sender<NALUnit>,
    shared: SharedControl,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tuning = Tuning::from_env();
    let hevc_limit = HevcLimit::from_env();
    info!(
        ?tuning,
        ?hevc_limit,
        "Using the direct VideoToolbox encoder"
    );
    // Hardware or nothing: this backend never reports a software fallback.
    PIPELINE_STATS.lock().set_software_fallback(false);
    let output = OutputQueue::new();
    let forwarder = Forwarder::spawn(output.clone(), tx, shared.clone(), generation)?;
    // Declared after the forwarder so it drops first: closing the session
    // flushes its last frames into the queue before the forwarder drains it.
    let mut session: Option<VtSession> = None;

    // Every pipeline starts on an IDR, as in the FFmpeg loop.
    shared.force_next_idr.store(true, Ordering::SeqCst);
    let fault_after: Option<u64> = std::env::var("ETERNAL_FAULT_ENCODER_AFTER")
        .ok()
        .and_then(|value| value.trim().parse().ok());

    let mut pending_keyframe = false;
    let mut failed_opens = 0u32;
    let mut failed_frames = 0u32;
    let mut reopen_after: Option<Instant> = None;
    let mut last_submitted: Option<Instant> = None;
    let mut level_checked = false;
    // A frame size and the rate it must stay below, set when VideoToolbox
    // writes an HEVC tier `hevc_limit` rules out (see `vt_hevc_tier`).
    let mut tier_cap: Option<((u32, u32), u32)> = None;

    while let Some(raw_frame) = frames.blocking_take() {
        if !shared.running.load(Ordering::SeqCst)
            || !crate::supervisor::generation_is_current(generation)
        {
            info!("Encoder loop stopping (stop requested or generation superseded)");
            break;
        }
        if forwarder.receiver_gone() {
            return Ok(());
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

        let bitrate = shared
            .abr_current_bps
            .load(Ordering::SeqCst)
            .max(config::MIN_BITRATE_BPS);
        // The level is planned for the user's ceiling, which the ABR's
        // pick never exceeds, so ABR steps stay live property changes.
        let ceiling = shared.bitrate_bps.load(Ordering::SeqCst).max(bitrate);
        let codec = Codec::negotiate(
            shared.hevc_enabled.load(Ordering::SeqCst)
                && shared.session.lock().client_supports_hevc(),
        );
        let size = (raw_frame.width, raw_frame.height);
        let limits = Limits {
            hevc: hevc_limit,
            max_fps: tier_cap
                .filter(|(capped, _)| *capped == size)
                .map_or(u32::MAX, |(_, fps)| fps),
        };
        let plan = plan_session(
            codec,
            raw_frame.width,
            raw_frame.height,
            shared.effective_fps(),
            ceiling,
            limits,
        )?;
        pending_keyframe |= shared.force_next_idr.swap(false, Ordering::SeqCst);

        if let Some(open) = session.as_ref() {
            pending_keyframe |= open.take_keyframe_retry();
            if open.is_lost() {
                warn!(counters = ?open.counters(), "VideoToolbox session lost; reopening");
                session = None;
            }
        }
        let change = session
            .as_ref()
            .map(|open| plan_change(open.plan(), open.live_fps(), &plan));
        if change == Some(PlanChange::Reopen) {
            info!(
                codec = codec.display_name(),
                width = plan.width,
                height = plan.height,
                encode_fps = plan.encode_fps,
                level = plan.level.name,
                "Reopening VideoToolbox session for a new codec, size, level or tier"
            );
            // Dropping flushes the old session's frames ahead of the new IDR.
            session = None;
        }

        let open = match session.as_mut() {
            Some(open) => {
                if let Some(PlanChange::UpdateFrameRate(fps)) = change {
                    match open.set_frame_rate(fps) {
                        Ok(()) => note_frame_rate(&shared, fps),
                        Err(error) => warn!(%error, fps, "Could not change the frame rate live"),
                    }
                }
                let bitrate = open.plan().clamp_bitrate(bitrate);
                if open.bitrate() != bitrate {
                    if let Err(error) = open.set_bitrate(bitrate) {
                        warn!(%error, bitrate, "Could not change the bitrate live");
                    }
                }
                open
            }
            None => {
                if reopen_after.is_some_and(|at| Instant::now() < at) {
                    continue;
                }
                match VtSession::new(plan, bitrate, tuning, output.clone()) {
                    Ok(opened) => {
                        info!(properties = %opened.describe(), "VideoToolbox session ready");
                        note_session_opened(&shared, &plan);
                        failed_opens = 0;
                        reopen_after = None;
                        level_checked = false;
                        pending_keyframe = true;
                        session.insert(opened)
                    }
                    Err(error @ (VtError::NoHardware(_) | VtError::Input(_))) => {
                        error!(%error, "VideoToolbox cannot encode this stream");
                        return Err(error.into());
                    }
                    Err(error) => {
                        failed_opens += 1;
                        if failed_opens >= MAX_FAILED_OPENS {
                            return Err(format!(
                                "VideoToolbox failed to open {failed_opens} times: {error}"
                            )
                            .into());
                        }
                        warn!(%error, attempt = failed_opens, "VideoToolbox session did not open; retrying");
                        reopen_after = Some(Instant::now() + REOPEN_PAUSE);
                        continue;
                    }
                }
            }
        };

        // A level that cannot carry the target rate (H.264 at full size and
        // 120 Hz) thins frames to the planned rate.
        if let (Some(gap), Some(last)) = (plan.min_frame_interval(), last_submitted) {
            if raw_frame.timestamp.saturating_duration_since(last) < gap {
                continue;
            }
        }

        // Backpressure: wait for the encoder and transport to catch up.
        while open.in_flight() + output.queued() >= MAX_PENDING_FRAMES
            && !open.is_lost()
            && !forwarder.receiver_gone()
            && shared.running.load(Ordering::SeqCst)
        {
            output.wait_for_change(Duration::from_millis(5));
        }

        let force_keyframe = std::mem::take(&mut pending_keyframe);
        if force_keyframe {
            info!(
                encoder = codec.display_name(),
                frame = raw_frame.frame_number,
                "[encoder] Forced IDR"
            );
        }
        let info = FrameInfo {
            captured: raw_frame.timestamp,
            frame_number: raw_frame.frame_number,
        };
        let result = open.encode_bgra(
            &raw_frame.data,
            raw_frame.width as usize * 4,
            info,
            force_keyframe,
        );
        // The frame is in the session's own buffer now: recycle capture's.
        drop(raw_frame.data);
        match result {
            Ok(()) => {
                failed_frames = 0;
                last_submitted = Some(info.captured);
            }
            Err(VtError::SessionLost(status)) => {
                warn!(status, "VideoToolbox session invalidated; reopening");
                session = None;
                pending_keyframe = true;
                continue;
            }
            Err(VtError::RetryLater(status)) => {
                warn!(
                    status,
                    "VideoToolbox encoder unavailable; reopening shortly"
                );
                session = None;
                pending_keyframe = true;
                reopen_after = Some(Instant::now() + REOPEN_PAUSE);
                continue;
            }
            Err(error @ (VtError::Input(_) | VtError::NoHardware(_))) => {
                return Err(error.into());
            }
            Err(error) => {
                failed_frames += 1;
                pending_keyframe = true;
                if failed_frames >= MAX_FAILED_FRAMES {
                    return Err(format!(
                        "VideoToolbox refused {failed_frames} frames in a row: {error}"
                    )
                    .into());
                }
                warn!(%error, "VideoToolbox refused a frame");
                continue;
            }
        }

        if !level_checked {
            if let Some(level) = open.emitted_level() {
                let opened = *open.plan();
                let written = level.describe(codec);
                let check = check_emitted(level, &opened, hevc_limit);
                if check.below_plan {
                    // VideoToolbox picked the level; Apple decoders reject a
                    // stream whose level is too low for its size and rate.
                    error!(
                        written,
                        required = opened.level.name,
                        "VideoToolbox wrote a level below the stream's requirement"
                    );
                }
                if let Some(fps) = check.lower_rate_to {
                    // AutoLevel went to High tier below the rate
                    // `vt_hevc_tier` predicts: run slower on this size,
                    // whatever level it wrote.
                    error!(
                        written,
                        encode_fps = opened.encode_fps,
                        "VideoToolbox wrote HEVC High tier, which needs the experimental opt-in; lowering the frame rate"
                    );
                    tier_cap = Some((size, fps));
                    session = None;
                    pending_keyframe = true;
                } else if !check.below_plan {
                    info!(
                        written,
                        required = opened.level.name,
                        "Stream profile and level"
                    );
                }
                // Only after both the level and the tier were judged; a
                // reopen clears it for the next session.
                level_checked = true;
            }
        }
    }

    info!("Capture channel closed, encoder shutting down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_backend_is_the_default_and_explicit_ffmpeg_choices_win() {
        let ffmpeg_has = |name: &str| matches!(name, "libx264" | "h264_videotoolbox");
        let vt = "h264_videotoolbox";
        assert!(selects_direct(None, vt, false, ffmpeg_has));
        assert!(selects_direct(
            Some("videotoolbox"),
            "libx264",
            false,
            ffmpeg_has
        ));
        // A stale override for an encoder this FFmpeg lacks (a Windows
        // settings file, or libx264 in an LGPL build) does not pick FFmpeg.
        assert!(selects_direct(Some("h264_nvenc"), vt, false, ffmpeg_has));
        assert!(!selects_direct(Some("libx264"), vt, false, ffmpeg_has));
        assert!(!selects_direct(
            Some("h264_videotoolbox"),
            vt,
            false,
            ffmpeg_has
        ));
        assert!(!selects_direct(None, vt, true, ffmpeg_has));
        // A caller that selected libx264 itself keeps it.
        assert!(!selects_direct(None, "libx264", false, ffmpeg_has));
    }

    /// An HEVC VPS (Main profile) with the given tier and level, as
    /// VideoToolbox writes it, emulation-prevention bytes included.
    fn vps(high_tier: bool, level_idc: u8) -> Vec<u8> {
        vec![
            0x40,
            0x01,
            0x0c,
            0x01,
            0xff,
            0xff,
            if high_tier { 0x21 } else { 0x01 },
            0x60,
            0x00,
            0x00,
            0x03,
            0x00,
            0x90,
            0x00,
            0x00,
            0x03,
            0x00,
            0x00,
            0x03,
            0x00,
            level_idc,
        ]
    }

    #[test]
    fn high_tier_falls_back_whatever_level_was_written() {
        // Full Duo size at 60 fps plans Main tier, level 5.1 (idc 153).
        let opened =
            plan_session(Codec::Hevc, 2670, 1878, 60, 15_000_000, Limits::default()).unwrap();
        assert_eq!((opened.tier, opened.level.idc), (Tier::Main, 153));
        let lower = Some(opened.encode_fps - 1);
        let cases = [
            // (high tier, level idc, below the plan, fallback rate)
            (true, 150, true, lower), // the reviewed case: High tier 5.0
            (true, 153, false, lower),
            (true, 156, false, lower),
            (false, 150, true, None),
            (false, 156, false, None),
        ];
        for (high_tier, level_idc, below_plan, lower_rate_to) in cases {
            let nal = vps(high_tier, level_idc);
            let level = bitstream::parameter_set_ptl(Codec::Hevc, &nal).unwrap();
            assert_eq!((level.high_tier, level.level_idc), (high_tier, level_idc));
            assert_eq!(
                check_emitted(level, &opened, HevcLimit::MainTier),
                EmittedCheck {
                    below_plan,
                    lower_rate_to
                },
                "high_tier={high_tier} level_idc={level_idc}"
            );
            // With the experimental opt-in High tier is allowed: no fallback,
            // and the level is still judged on its own.
            assert_eq!(
                check_emitted(level, &opened, HevcLimit::ExperimentalHighTier),
                EmittedCheck {
                    below_plan,
                    lower_rate_to: None
                },
                "experimental high_tier={high_tier} level_idc={level_idc}"
            );
        }
    }
}
