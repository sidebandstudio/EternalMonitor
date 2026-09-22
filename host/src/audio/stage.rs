use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{error::TrySendError, Sender};

use super::encoder::OpusEncoder;
use super::resample::{PcmBlock, PcmFormat, Resampler};
use super::synthetic::SyntheticSource;
use super::{wanted_session, AudioResult, AudioStats, SessionPacket, SAMPLE_RATE};
use crate::capture::timing::FrameTimer;
use crate::control::SharedControl;
use crate::stats::PIPELINE_STATS;
use crate::supervisor::{generation_is_current, HealthReporter, Stage, StageOutcome};

// A synthetic capture block is one Opus frame, so discontinuity resets
// never discard half a frame and shift the tone/silence boundaries.
const SYNTHETIC_BLOCK_FRAMES: usize = super::FRAME_SAMPLES;
const SYNTHETIC_BLOCK_US: u64 = SYNTHETIC_BLOCK_FRAMES as u64 * 1_000_000 / SAMPLE_RATE as u64;

enum Source {
    Synthetic {
        source: SyntheticSource,
        next: Instant,
        timer: FrameTimer,
        first: bool,
    },
    #[cfg(windows)]
    Wasapi(Box<super::wasapi::WasapiSource>),
}

impl Source {
    fn open() -> AudioResult<Self> {
        let selection = std::env::var("ETERNAL_AUDIO").ok();
        if selection
            .as_deref()
            .is_some_and(|value| value != "synthetic" && value != "wasapi")
        {
            return Err("ETERNAL_AUDIO must be synthetic or wasapi".into());
        }
        #[cfg(windows)]
        if selection.as_deref() != Some("synthetic") {
            return Ok(Self::Wasapi(Box::new(super::wasapi::WasapiSource::new()?)));
        }
        #[cfg(not(windows))]
        if selection.as_deref() == Some("wasapi") {
            return Err("WASAPI requires Windows".into());
        }
        Ok(Self::Synthetic {
            source: SyntheticSource::default(),
            next: Instant::now(),
            timer: FrameTimer::new(),
            first: true,
        })
    }

    fn name(&self) -> &str {
        match self {
            Self::Synthetic { .. } => "Synthetic 1 kHz tone",
            #[cfg(windows)]
            Self::Wasapi(source) => source.name(),
        }
    }

    fn read(&mut self) -> AudioResult<Option<PcmBlock>> {
        match self {
            Self::Synthetic {
                source,
                next,
                timer,
                first,
            } => {
                *next += Duration::from_micros(SYNTHETIC_BLOCK_US);
                timer.wait_until(*next);
                let late = next.elapsed() > Duration::from_millis(20);
                if late {
                    *next = Instant::now();
                }
                let samples = source
                    .read(SYNTHETIC_BLOCK_FRAMES)
                    .into_iter()
                    .flat_map(|(l, r)| [l, r])
                    .collect();
                Ok(Some(PcmBlock {
                    format: PcmFormat::STEREO_48K,
                    samples,
                    capture_ts_us: crate::clock::host_now_us().saturating_sub(SYNTHETIC_BLOCK_US),
                    discontinuity: std::mem::take(first) || late,
                }))
            }
            #[cfg(windows)]
            Self::Wasapi(source) => source.read(),
        }
    }
}

struct CaptureSession {
    id: u32,
    source: Source,
    resampler: Option<Resampler>,
    encoder: OpusEncoder,
    mark_discontinuity: bool,
    next_input_us: Option<u64>,
}

impl CaptureSession {
    fn new(id: u32) -> AudioResult<Self> {
        Ok(Self {
            id,
            source: Source::open()?,
            resampler: None,
            encoder: OpusEncoder::new(576 - eternal_wire::v2::audio::AUDIO_HEADER_SIZE)?,
            mark_discontinuity: true,
            next_input_us: None,
        })
    }

    fn tick(
        &mut self,
        tx: &Sender<SessionPacket>,
        shared: &SharedControl,
        generation: u64,
    ) -> AudioResult<bool> {
        let Some(block) = self.source.read()? else {
            return Ok(true);
        };
        if !generation_is_current(generation) || !shared.running.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if wanted_session(shared) != Some(self.id) {
            return Ok(true);
        }
        let gap = self
            .next_input_us
            .is_some_and(|expected| block.capture_ts_us.abs_diff(expected) > 100_000);
        if block.discontinuity
            || gap
            || self
                .resampler
                .as_ref()
                .is_none_or(|resampler| resampler.format() != block.format)
        {
            self.resampler = Some(Resampler::new(block.format)?);
            self.encoder.reopen()?;
            self.mark_discontinuity = true;
        }
        self.next_input_us = Some(
            block.capture_ts_us
                + block.samples.len() as u64 / u64::from(block.format.channels) * 1_000_000
                    / u64::from(block.format.rate),
        );
        {
            let mut stats = PIPELINE_STATS.lock();
            stats.audio.device = self.source.name().into();
            stats.audio.status = "Streaming".into();
        }
        for frame in self.resampler.as_mut().unwrap().push(&block)? {
            let mut packet = self.encoder.encode(&frame.samples, frame.capture_ts_us)?;
            packet.discontinuity |= self.mark_discontinuity;
            match tx.try_send(SessionPacket {
                session_id: self.id,
                packet,
            }) {
                Ok(()) => self.mark_discontinuity = false,
                Err(TrySendError::Full(_)) => {
                    self.mark_discontinuity = true;
                    PIPELINE_STATS.lock().audio.dropped += 1;
                }
                Err(TrySendError::Closed(_)) => return Ok(false),
            }
        }
        Ok(true)
    }
}

/// An endpoint is opened only after both peers opt in. A failed capture
/// attempt stays disabled for that session; reconnecting or toggling the
/// host audio setting off and on retries without restarting video.
pub fn run_audio_stage(
    tx: Sender<SessionPacket>,
    shared: SharedControl,
    generation: u64,
    reporter: HealthReporter,
) {
    let mut active: Option<CaptureSession> = None;
    let mut failed_session = None;
    let mut next_log = Instant::now() + Duration::from_secs(1);
    while shared.running.load(Ordering::SeqCst)
        && generation_is_current(generation)
        && !tx.is_closed()
    {
        let wanted = wanted_session(&shared);
        if wanted.is_none() {
            active = None;
            if !shared.audio_enabled.load(Ordering::SeqCst) {
                failed_session = None;
            }
            let mut stats = PIPELINE_STATS.lock();
            stats
                .audio
                .stopped(if shared.audio_enabled.load(Ordering::SeqCst) {
                    "Waiting for client"
                } else {
                    "Off"
                });
            if failed_session.is_none() {
                stats.audio.error = None;
            }
            drop(stats);
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let session_id = wanted.unwrap();
        if failed_session == Some(session_id) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let result = (|| -> AudioResult<bool> {
            if active
                .as_ref()
                .is_none_or(|session| session.id != session_id)
            {
                active = None;
                let session = CaptureSession::new(session_id)?;
                if !generation_is_current(generation) || !shared.running.load(Ordering::SeqCst) {
                    return Ok(false);
                }
                PIPELINE_STATS.lock().audio = AudioStats::started(session.source.name());
                tracing::info!(
                    session_id,
                    device = session.source.name(),
                    bitrate = super::BITRATE,
                    "PC audio session started (Opus, 48 kHz stereo, 20 ms)"
                );
                active = Some(session);
            }
            active.as_mut().unwrap().tick(&tx, &shared, generation)
        })();
        if !generation_is_current(generation) || !shared.running.load(Ordering::SeqCst) {
            break;
        }
        match result {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                active = None;
                failed_session = Some(session_id);
                let reason = error.to_string();
                let mut stats = PIPELINE_STATS.lock();
                stats.audio.stopped("Unavailable");
                stats.audio.error = Some(reason.clone());
                drop(stats);
                reporter.stage_exited(Stage::Audio, StageOutcome::Failed(reason));
            }
        }
        if Instant::now() >= next_log {
            next_log = Instant::now() + Duration::from_secs(1);
            let stats = PIPELINE_STATS.lock();
            tracing::info!(device = %stats.audio.device, kbps = stats.audio.kbps,
                packets_per_sec = stats.audio.packets_per_sec, quiet_packets = stats.audio.quiet_packets,
                dropped = stats.audio.dropped, "Audio stream stats");
        }
    }
    drop(active);
    if generation_is_current(generation) {
        PIPELINE_STATS.lock().audio.stopped("Stopped");
    }
    reporter.stage_exited(Stage::Audio, StageOutcome::Completed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_silence_keeps_opus_alignment_after_a_late_capture() {
        ffmpeg_next::init().unwrap();
        let mut source = SyntheticSource::default();
        let mut resampler = Resampler::new(PcmFormat::STEREO_48K).unwrap();
        let mut quiet = 0;
        for index in 0..96_000 / SYNTHETIC_BLOCK_FRAMES {
            // A late read recreates swresample. A half-Opus-frame remainder
            // here used to shift the 100 ms quiet window by 10 ms forever.
            if index == 3 {
                resampler = Resampler::new(PcmFormat::STEREO_48K).unwrap();
            }
            let samples = source
                .read(SYNTHETIC_BLOCK_FRAMES)
                .into_iter()
                .flat_map(|(l, r)| [l, r])
                .collect();
            for frame in resampler
                .push(&PcmBlock {
                    format: PcmFormat::STEREO_48K,
                    samples,
                    capture_ts_us: index as u64 * SYNTHETIC_BLOCK_FRAMES as u64 * 1_000_000
                        / u64::from(SAMPLE_RATE),
                    discontinuity: index == 3,
                })
                .unwrap()
            {
                if frame.samples.iter().all(|&(l, r)| l == 0.0 && r == 0.0) {
                    quiet += 1;
                }
            }
        }
        assert_eq!(
            quiet, 5,
            "100 ms silence must retain five complete Opus frames"
        );
    }
}
