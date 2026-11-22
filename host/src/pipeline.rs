//! One pipeline generation: capture stage (dedicated thread, latest-wins
//! slot) → encoder stage (dedicated thread, lossless channel) → transport
//! (async). Each stage reports its exit to the supervisor, which owns
//! restarts, backoff, and wedge detection (see `supervisor.rs`).

use std::sync::mpsc;

use tokio::sync::mpsc as tokio_mpsc;
use tracing::{error, info, warn};

use crate::control::{SharedControl, SupervisorCommand};
use crate::gpu::GpuInfo;
use crate::supervisor::{HealthReporter, Stage, StageOutcome};
use crate::{capture, encoder, stats, transport};

pub const DEFAULT_BITRATE_BPS: u32 = 15_000_000;

/// Runs one supervised pipeline generation to completion on the current
/// (tokio) runtime. Stage threads are plain `std::thread`s so a wedged
/// DXGI/encoder call can never block the runtime's shutdown.
pub async fn run_pipeline_supervised(
    generation: u64,
    listen_port: u16,
    shared: SharedControl,
    gpu_info: GpuInfo,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
    reporter: HealthReporter,
) {
    // ffmpeg_next::init() is idempotent — called here as safety net for restarts
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        reporter.stage_exited(
            Stage::Encoder,
            StageOutcome::Failed(format!("ffmpeg init: {e}")),
        );
        note_pipeline_stopped(&stats::PIPELINE_STATS, generation);
        return;
    }

    let (frame_producer, frame_consumer) = capture::frame_slot();
    let (nal_tx, nal_rx) = tokio_mpsc::channel(encoder::NAL_CHANNEL_CAPACITY);
    let (audio_tx, audio_rx) = tokio_mpsc::channel(crate::audio::QUEUE_CAPACITY);

    let audio_shared = shared.clone();
    let audio_reporter = reporter.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("audio-g{generation}"))
        .spawn(move || {
            crate::audio::run_audio_stage(audio_tx, audio_shared, generation, audio_reporter)
        })
    {
        let reason = error.to_string();
        crate::audio::report_unavailable(
            &stats::PIPELINE_STATS,
            || crate::supervisor::generation_is_current(generation),
            &reason,
        );
        reporter.stage_exited(Stage::Audio, StageOutcome::Failed(reason));
    }

    let capture_reporter = reporter.clone();
    let capture_shared = shared.clone();
    let capture_thread = std::thread::Builder::new()
        .name(format!("capture-g{generation}"))
        .spawn(move || {
            let outcome =
                match capture::run_capture_stage(frame_producer, capture_shared, generation) {
                    Ok(()) => StageOutcome::Completed,
                    Err(e) => StageOutcome::Failed(e.to_string()),
                };
            capture_reporter.stage_exited(Stage::Capture, outcome);
        });
    if let Err(e) = capture_thread {
        reporter.stage_exited(Stage::Capture, StageOutcome::Failed(e.to_string()));
        return;
    }

    let encoder_reporter = reporter.clone();
    let encoder_shared = shared.clone();
    let encoder_gpu = gpu_info;
    let encoder_thread = std::thread::Builder::new()
        .name(format!("encode-g{generation}"))
        .spawn(move || {
            let outcome = match encoder::run_encode_stage(
                frame_consumer,
                nal_tx,
                encoder_shared,
                encoder_gpu,
                generation,
            ) {
                Ok(()) => StageOutcome::Completed,
                Err(e) => StageOutcome::Failed(e.to_string()),
            };
            encoder_reporter.stage_exited(Stage::Encoder, outcome);
        });
    if let Err(e) = encoder_thread {
        reporter.stage_exited(Stage::Encoder, StageOutcome::Failed(e.to_string()));
        return;
    }

    let transport_outcome = match transport::start_sender(
        nal_rx,
        audio_rx,
        listen_port,
        shared.clone(),
        supervisor_tx,
        generation,
    )
    .await
    {
        Ok(()) => StageOutcome::Completed,
        Err(e) => transport_failure(&*e),
    };
    reporter.stage_exited(Stage::Transport, transport_outcome);

    note_pipeline_stopped(&stats::PIPELINE_STATS, generation);
    info!(generation, "Pipeline ended");
}

/// Marks the pipeline stopped if the generation that ended is still current.
fn note_pipeline_stopped(stats: &parking_lot::Mutex<stats::PipelineStats>, generation: u64) {
    let mut stats = stats.lock();
    if crate::supervisor::generation_is_current(generation) {
        stats.mark_pipeline_stopped();
    }
}

/// A transport that could not bind the stream port waits for it (see
/// `startup::port`); anything else is a failure.
fn transport_failure(error: &(dyn std::error::Error + 'static)) -> StageOutcome {
    if error
        .downcast_ref::<std::io::Error>()
        .is_some_and(crate::startup::port::is_held)
    {
        warn!(%error, "The stream port was taken before the stream could bind it");
        return StageOutcome::PortInUse;
    }
    error!(error = %error, "Transport sender exited with error");
    StageOutcome::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_current_generation_can_mark_the_pipeline_stopped() {
        use std::sync::atomic::Ordering;

        let generation = crate::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        let mut running = stats::PipelineStats::default();
        running.mark_pipeline_started();
        running.note_encoder_opened("h264_qsv", true);
        let stats = parking_lot::Mutex::new(running);

        note_pipeline_stopped(&stats, generation.wrapping_sub(1));
        {
            let stats = stats.lock();
            assert!(stats.pipeline_running);
            assert_eq!(stats.active_encoder, "h264_qsv");
        }

        note_pipeline_stopped(&stats, generation);
        let stats = stats.lock();
        assert!(!stats.pipeline_running);
        assert!(stats.active_encoder.is_empty());
    }

    /// The stream's bind error, as `start_sender` returns it.
    #[test]
    fn a_port_in_use_waits_and_other_transport_errors_fail() {
        let in_use: Box<dyn std::error::Error + Send + Sync> =
            std::io::Error::from(std::io::ErrorKind::AddrInUse).into();
        assert_eq!(transport_failure(&*in_use), StageOutcome::PortInUse);
        // WSAEACCES: a program holds it exclusively, or Windows reserved it.
        let exclusive: Box<dyn std::error::Error + Send + Sync> =
            std::io::Error::from(std::io::ErrorKind::PermissionDenied).into();
        let waits = cfg!(windows);
        assert_eq!(
            transport_failure(&*exclusive) == StageOutcome::PortInUse,
            waits
        );
        let other: Box<dyn std::error::Error + Send + Sync> =
            std::io::Error::from(std::io::ErrorKind::AddrNotAvailable).into();
        assert!(matches!(
            transport_failure(&*other),
            StageOutcome::Failed(_)
        ));
    }
}
