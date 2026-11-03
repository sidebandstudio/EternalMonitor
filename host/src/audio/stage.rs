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
const FRAME_US: u64 = super::FRAME_SAMPLES as u64 * 1_000_000 / SAMPLE_RATE as u64;

enum Source {
    Synthetic {
        source: SyntheticSource,
        next: Instant,
        timer: FrameTimer,
        first: bool,
    },
    #[cfg(windows)]
    Wasapi(Box<super::wasapi::WasapiSource>),
    #[cfg(test)]
    Scripted(std::collections::VecDeque<PcmBlock>),
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
            #[cfg(test)]
            Self::Scripted(_) => "Scripted test source",
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
            #[cfg(test)]
            Self::Scripted(blocks) => Ok(blocks.pop_front()),
        }
    }
}

/// Constrained VBR usually keeps 20 ms packets under 548 bytes, but field
/// logs show 550-576 byte packets on four PCs, and the old 548-byte encoder
/// limit turned each one into "PC audio unavailable" for the rest of the
/// session. Packets are now bounded by what the wire format accepts. The
/// transport drops (and counts) any packet over the link's datagram size,
/// the limit video uses on the same path.
const MAX_OPUS_PAYLOAD: usize =
    eternal_wire::v2::MAX_DGRAM_SIZE - eternal_wire::v2::audio::AUDIO_HEADER_SIZE;

struct CaptureSession {
    id: u32,
    source: Source,
    resampler: Option<Resampler>,
    encoder: OpusEncoder,
    mark_discontinuity: bool,
    next_input_us: Option<u64>,
    started_us: Option<u64>,
    stream: u64,
}

impl CaptureSession {
    fn new(id: u32) -> AudioResult<Self> {
        Ok(Self {
            id,
            source: Source::open()?,
            resampler: None,
            encoder: OpusEncoder::new(MAX_OPUS_PAYLOAD)?,
            mark_discontinuity: true,
            next_input_us: None,
            started_us: None,
            stream: 0,
        })
    }

    fn tick(
        &mut self,
        tx: &Sender<SessionPacket>,
        shared: &SharedControl,
        generation: u64,
    ) -> AudioResult<bool> {
        let Some(block) = self.read()? else {
            return Ok(true);
        };
        if !generation_is_current(generation) || !shared.running.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if wanted_session(shared) != Some(self.id) {
            return Ok(true);
        }
        self.send(block, tx)
    }

    fn read(&mut self) -> AudioResult<Option<PcmBlock>> {
        self.started_us
            .get_or_insert_with(crate::clock::host_now_us);
        self.source.read()
    }

    fn send(&mut self, block: PcmBlock, tx: &Sender<SessionPacket>) -> AudioResult<bool> {
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
        if let Some(audio) = PIPELINE_STATS.lock().audio.of_stream(self.stream) {
            audio.device = self.source.name().into();
            audio.status = "Streaming".into();
        }
        let started_us = self.started_us.unwrap_or(0);
        for frame in self.resampler.as_mut().unwrap().push(&block)? {
            // WASAPI buffers from Start() while the session opens its encoder,
            // and an idle engine's first read is silence back to Start().
            // Frames that ended before the first read predate the session and
            // are already near the sender's deadline; numbering them would
            // turn each late one into a gap for the viewer. Whole frames keep
            // the Opus frame grid where the source put it.
            if frame.capture_ts_us + FRAME_US <= started_us {
                continue;
            }
            let mut packet = self.encoder.encode(&frame.samples, frame.capture_ts_us)?;
            packet.discontinuity |= self.mark_discontinuity;
            match tx.try_send(SessionPacket {
                session_id: self.id,
                stream: self.stream,
                packet,
            }) {
                Ok(()) => self.mark_discontinuity = false,
                Err(TrySendError::Full(rejected)) => {
                    self.mark_discontinuity = true;
                    if let Some(audio) = PIPELINE_STATS.lock().audio.of_stream(rejected.stream) {
                        audio.dropped += 1;
                        audio.capture_queue_dropped += 1;
                        audio.late_seqs.record(rejected.packet.seq);
                    }
                }
                Err(TrySendError::Closed(_)) => return Ok(false),
            }
        }
        Ok(true)
    }
}

/// Replace the published audio stats with a new capture's, unless a newer
/// generation has started, and return the stats it replaced. The audio
/// thread is detached, so a stale stage can run after its successor. The
/// check and the replacement share the stats lock; otherwise a stale stage
/// descheduled between them could replace a newer capture's stats, and the
/// transport would then discard every live packet as retired.
fn publish(
    target: &parking_lot::Mutex<crate::stats::PipelineStats>,
    stats: AudioStats,
    is_current: impl FnOnce() -> bool,
) -> Option<AudioStats> {
    let mut pipeline = target.lock();
    is_current().then(|| std::mem::replace(&mut pipeline.audio, stats))
}

/// Report audio unavailable for `reason`, if this generation is current.
/// The check shares the stats lock with the change: a detached old pipeline
/// that fails to start its audio thread after its successor started must not
/// mark the live capture unavailable. True when it reported.
pub(crate) fn report_unavailable(
    target: &parking_lot::Mutex<crate::stats::PipelineStats>,
    is_current: impl FnOnce() -> bool,
    reason: &str,
) -> bool {
    let mut stats = target.lock();
    let current = is_current();
    if current {
        stats.audio.stopped("Unavailable");
        stats.audio.error = Some(reason.to_string());
    }
    current
}

/// Apply `change` to the published audio stats if this generation is current.
fn update_current(generation: u64, change: impl FnOnce(&mut AudioStats)) {
    let mut stats = PIPELINE_STATS.lock();
    if generation_is_current(generation) {
        change(&mut stats.audio);
    }
}

fn log_stats(stats: &AudioStats) {
    tracing::info!(device = %stats.device, kbps = stats.kbps,
        packets_per_sec = stats.packets_per_sec, quiet_packets = stats.quiet_packets,
        dropped = stats.dropped, capture_queue_dropped = stats.capture_queue_dropped,
        stale_dropped = stats.stale_dropped, oversized_dropped = stats.oversized_dropped,
        send_failed = stats.send_failed, late_seqs = %stats.late_seqs,
        failed_seqs = %stats.failed_seqs, audio_stream = stats.stream,
        wire_stream = %stats.wire, "Audio stream stats");
}

/// Log a capture's final stats when it ends, so drops after its last
/// periodic line still reach the log. Stats already replaced were logged by
/// whatever replaced them: a newer capture or a restart.
fn log_final(session: Option<CaptureSession>) {
    if let Some(session) = session {
        log_final_of(&PIPELINE_STATS, session.stream);
    }
}

fn log_final_of(target: &parking_lot::Mutex<crate::stats::PipelineStats>, stream: u64) {
    let stats = target.lock().audio.clone();
    if stats.stream == stream {
        log_stats(&stats);
    }
}

/// Reset the stats for a new generation, with `configure` under the same
/// lock, and log the audio capture they held. The old generation's audio
/// thread is detached and may reach its final line only after this replaced
/// its stats, so the reset hands them back here, as `publish` does.
pub(crate) fn restart_stats(
    target: &parking_lot::Mutex<crate::stats::PipelineStats>,
    configure: impl FnOnce(&mut crate::stats::PipelineStats),
) {
    let retired = {
        let mut stats = target.lock();
        let retired = stats.reset_for_restart();
        configure(&mut stats);
        retired
    };
    if retired.stream != 0 {
        log_stats(&retired);
    }
}

/// When to try again to open audio while FFmpeg's encoder lane is stuck:
/// soon at first, then at most every 8 s, so a long stall neither spins nor
/// floods the log.
#[derive(Default)]
struct StallRetry {
    next: Option<Instant>,
    wait: Duration,
}

impl StallRetry {
    const FIRST: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(8);

    fn due(&self, now: Instant) -> bool {
        self.next.is_none_or(|next| now >= next)
    }

    fn failed(&mut self, now: Instant) -> Duration {
        self.wait = (self.wait * 2).clamp(Self::FIRST, Self::MAX);
        self.next = Some(now + self.wait);
        self.wait
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
    let mut stalled = StallRetry::default();
    let mut next_log = Instant::now() + Duration::from_secs(1);
    while shared.running.load(Ordering::SeqCst)
        && generation_is_current(generation)
        && !tx.is_closed()
    {
        let wanted = wanted_session(&shared);
        if wanted.is_none() {
            log_final(active.take());
            if !shared.audio_enabled.load(Ordering::SeqCst) {
                failed_session = None;
            }
            update_current(generation, |audio| {
                audio.stopped(if shared.audio_enabled.load(Ordering::SeqCst) {
                    "Waiting for client"
                } else {
                    "Off"
                });
                if failed_session.is_none() {
                    audio.error = None;
                }
            });
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let session_id = wanted.unwrap();
        if failed_session == Some(session_id) || !stalled.due(Instant::now()) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let result = (|| -> AudioResult<bool> {
            if active
                .as_ref()
                .is_none_or(|session| session.id != session_id)
            {
                active = None;
                let mut session = CaptureSession::new(session_id)?;
                if !shared.running.load(Ordering::SeqCst) {
                    return Ok(false);
                }
                let stats = AudioStats::started(session.source.name());
                session.stream = stats.stream;
                let Some(replaced) =
                    publish(&PIPELINE_STATS, stats, || generation_is_current(generation))
                else {
                    return Ok(false);
                };
                if replaced.stream != 0 {
                    // Drops charged since its last periodic line end up here.
                    log_stats(&replaced);
                }
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
            Ok(true) => stalled = StallRetry::default(),
            Ok(false) => break,
            // Video's encoder holds FFmpeg's lock in a stuck driver call:
            // not this session's failure. Video goes on; the open is tried
            // again later, once the call has returned.
            Err(error) if super::encoder::is_stall(error.as_ref()) => {
                log_final(active.take());
                let wait = stalled.failed(Instant::now());
                tracing::warn!(
                    session_id,
                    retry_ms = wait.as_millis() as u64,
                    "PC audio waits for a stuck graphics driver call; video continues"
                );
                update_current(generation, |audio| {
                    audio.stopped("Waiting for the graphics driver");
                    audio.error = Some(error.to_string());
                });
            }
            Err(error) => {
                log_final(active.take());
                failed_session = Some(session_id);
                let reason = error.to_string();
                update_current(generation, |audio| {
                    audio.stopped("Unavailable");
                    audio.error = Some(reason.clone());
                });
                reporter.stage_exited(Stage::Audio, StageOutcome::Failed(reason));
            }
        }
        if Instant::now() >= next_log {
            next_log = Instant::now() + Duration::from_secs(1);
            let stats = PIPELINE_STATS.lock().audio.clone();
            // Log only this stage's capture, never a successor's stats.
            if active
                .as_ref()
                .is_none_or(|session| session.stream == stats.stream)
            {
                log_stats(&stats);
            }
        }
    }
    log_final(active.take());
    update_current(generation, |audio| audio.stopped("Stopped"));
    reporter.stage_exited(Stage::Audio, StageOutcome::Completed);
}

#[cfg(test)]
mod tests {

    #[test]
    fn audio_retries_a_stalled_open_with_a_bounded_backoff() {
        let mut retry = StallRetry::default();
        let now = Instant::now();
        assert!(retry.due(now));
        let waits: Vec<u64> = (0..7)
            .map(|_| retry.failed(now).as_millis() as u64)
            .collect();
        assert_eq!(waits, [500, 1000, 2000, 4000, 8000, 8000, 8000]);
        assert!(!retry.due(now + Duration::from_secs(7)));
        assert!(retry.due(now + Duration::from_secs(8)));
        // A stall is retried; any other open failure still ends the session.
        let stall: Box<dyn std::error::Error + Send + Sync> =
            Box::new(crate::stall::Stall::Refused {
                call: crate::stall::Call::EncoderOpen,
            });
        assert!(crate::audio::encoder::is_stall(stall.as_ref()));
        let other: Box<dyn std::error::Error + Send + Sync> = "libopus encoder unavailable".into();
        assert!(!crate::audio::encoder::is_stall(other.as_ref()));
    }

    #[test]
    fn opus_packets_seen_in_the_field_are_sent_not_fatal() {
        use eternal_wire::v2::audio::AudioHeader;
        let field_packet = vec![0x55; 576];
        assert!(MAX_OPUS_PAYLOAD >= field_packet.len());
        let header = AudioHeader {
            session_id: 1,
            stream_epoch: 1,
            audio_seq: 1,
            capture_ts_us: 0,
            discontinuity: false,
        };
        // 0.3.1 iPads decode it: the receiver accepts the whole wire range.
        let datagram = header.encode(&field_packet).unwrap();
        let (decoded, payload) = AudioHeader::decode(&datagram).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(payload, field_packet.as_slice());
    }

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

    #[test]
    fn loopback_backlog_from_before_the_session_is_not_sent_stale() {
        ffmpeg_next::init().unwrap();
        // WASAPI buffers from Start() while the session opens its encoder. A
        // 0.3.1 host on an i7-5500U took 105 ms, and its first six packets
        // missed the sender's deadline. Those packets carried sequence
        // numbers, so the iPad counted each one as lost.
        //
        // Timestamps are explicit, so the test needs no particular speed:
        // a packet goes out once its audio exists and the session has
        // started, so its age at send is max(start, end of its audio) - capture.
        const START: u64 = 10_000_000;
        let mut tone = SyntheticSource::default();
        let mut block = |capture_ts_us: u64, frames: usize| PcmBlock {
            format: PcmFormat::STEREO_48K,
            samples: tone
                .read(frames)
                .into_iter()
                .flat_map(|(l, r)| [l, r])
                .collect(),
            capture_ts_us,
            discontinuity: false,
        };
        let cases = [
            // Sixteen 10 ms engine periods published before the first read.
            (
                "packet backlog",
                (0..36u64)
                    .map(|index| block(START - 160_000 + index * 10_000, 480))
                    .collect::<std::collections::VecDeque<_>>(),
            ),
            // An idle engine: the first read fills 100 ms since Start() with
            // silence, most of it from before the session.
            (
                "idle silence",
                std::iter::once(block(START - 95_000, 4800))
                    .chain((0..20u64).map(|index| block(START + 5_000 + index * 10_000, 480)))
                    .collect(),
            ),
        ];
        // The first read starts the session on the host clock.
        let before = crate::clock::host_now_us();
        let mut unstarted = CaptureSession {
            id: 1,
            source: Source::Scripted(Default::default()),
            resampler: None,
            encoder: OpusEncoder::new(1000).unwrap(),
            mark_discontinuity: true,
            next_input_us: None,
            started_us: None,
            stream: 0,
        };
        assert!(unstarted.read().unwrap().is_none());
        assert!(unstarted.started_us.is_some_and(|at| at >= before));
        for (name, blocks) in cases {
            let reads = blocks.len();
            let mut session = CaptureSession {
                id: 1,
                source: Source::Scripted(blocks),
                resampler: None,
                encoder: OpusEncoder::new(1000).unwrap(),
                mark_discontinuity: true,
                next_input_us: None,
                started_us: Some(START),
                stream: 0,
            };
            let (tx, mut rx) = tokio::sync::mpsc::channel(super::super::QUEUE_CAPACITY);
            let mut sent = Vec::new();
            for _ in 0..reads {
                if let Some(block) = session.read().unwrap() {
                    assert!(session.send(block, &tx).unwrap());
                }
                while let Ok(queued) = rx.try_recv() {
                    sent.push(queued.packet);
                }
            }
            assert!(sent.len() >= 8, "{name}: live audio was not sent");
            let stale: Vec<_> = sent
                .iter()
                .map(|packet| {
                    let send_us = START.max(packet.capture_ts_us + FRAME_US);
                    (packet.seq, send_us - packet.capture_ts_us)
                })
                .filter(|&(_, age)| age > super::super::SEND_DEADLINE_US)
                .collect();
            assert!(
                stale.is_empty(),
                "{name}: packets past the send deadline: {stale:?}"
            );
            let first = &sent[0];
            assert_eq!((first.seq, first.discontinuity), (0, true), "{name}");
            assert!(
                first.capture_ts_us + FRAME_US > START,
                "{name}: the first packet replays audio captured {} us before the session",
                START - first.capture_ts_us
            );
        }
    }

    #[test]
    fn a_stale_generation_never_replaces_a_newer_capture() {
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
        let target = parking_lot::Mutex::new(crate::stats::PipelineStats::default());
        let generation = AtomicU64::new(1);
        let newer = AudioStats::started("generation 2");
        let newer_stream = newer.stream;
        let mut pending = Some(newer);
        // Generation 1's detached stage checks that it is current. Right
        // then generation 2 starts, and its stage publishes as soon as it
        // can get the stats lock.
        let published = publish(&target, AudioStats::started("generation 1"), || {
            let current = generation.load(SeqCst) == 1;
            generation.store(2, SeqCst);
            if !target.is_locked() {
                let newer = pending.take().unwrap();
                publish(&target, newer, || generation.load(SeqCst) == 2).unwrap();
            }
            current
        });
        if let Some(newer) = pending.take() {
            assert!(publish(&target, newer, || generation.load(SeqCst) == 2).is_some());
        }
        assert!(
            published.is_some(),
            "generation 1 was current when it checked"
        );
        assert_eq!(
            target.lock().audio.stream,
            newer_stream,
            "a stale generation replaced the newer capture's stats"
        );
    }

    #[test]
    fn a_replaced_capture_hands_back_its_final_stats() {
        let target = parking_lot::Mutex::new(crate::stats::PipelineStats::default());
        let mut first = AudioStats::started("A");
        first.late_seqs.record(41);
        assert_eq!(publish(&target, first, || true).unwrap().stream, 0);
        // A drop charged after A's last periodic stats line.
        target.lock().audio.late_seqs.record(42);
        let replaced = publish(&target, AudioStats::started("B"), || true).unwrap();
        assert_eq!(replaced.late_seqs.to_string(), "41-42");
        assert!(publish(&target, AudioStats::started("C"), || false).is_none());
        assert_eq!(target.lock().audio.device, "B");
    }

    #[test]
    fn an_old_pipelines_failed_audio_start_leaves_the_live_capture_alone() {
        let target = parking_lot::Mutex::new(crate::stats::PipelineStats::default());
        let mut live = AudioStats::started("Speakers");
        live.status = "Streaming".into();
        let stream = live.stream;
        publish(&target, live, || true).unwrap();
        // Generation 1's audio thread could not start after generation 2 did.
        assert!(!report_unavailable(&target, || false, "no thread"));
        {
            let stats = target.lock();
            assert_eq!(stats.audio.status, "Streaming");
            assert_eq!(stats.audio.error, None);
            assert_eq!(stats.audio.stream, stream);
        }
        // The current generation's failure is reported.
        assert!(report_unavailable(&target, || true, "no thread"));
        let stats = target.lock();
        assert_eq!(stats.audio.status, "Unavailable");
        assert_eq!(stats.audio.error.as_deref(), Some("no thread"));
    }

    /// Log lines `run` writes on this thread.
    fn logged(run: impl FnOnce()) -> String {
        let buffer = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Captured(writer.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        let text = String::from_utf8_lossy(&buffer.lock()).into_owned();
        text
    }

    struct Captured(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);
    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_restart_logs_the_final_stats_of_a_capture_whose_thread_ends_later() {
        let target = parking_lot::Mutex::new(crate::stats::PipelineStats::default());
        let capture = AudioStats::started("A");
        let stream = capture.stream;
        publish(&target, capture, || true).unwrap();
        // A drop charged after the capture's last periodic stats line.
        target.lock().audio.late_seqs.record(42);
        let lines = logged(|| {
            // The supervisor resets the stats for the next generation...
            restart_stats(&target, |stats| stats.set_bitrate(5_000_000));
            // ...before the old generation's detached thread logs its end.
            log_final_of(&target, stream);
        });
        let finals: Vec<_> = lines
            .lines()
            .filter(|line| line.contains(&format!("audio_stream={stream} ")))
            .collect();
        assert_eq!(finals.len(), 1, "{lines}");
        assert!(finals[0].contains("late_seqs=42"), "{lines}");
        assert_eq!(target.lock().audio.stream, 0);
        assert_eq!(target.lock().bitrate_bps, 5_000_000);
        // An idle restart, with no capture, logs nothing.
        assert_eq!(logged(|| restart_stats(&target, |_| {})), "");
    }

    /// Native check of the same startup path with the PC's real loopback.
    /// The delay stands in for the encoder open that follows WASAPI Start().
    /// Inaudible: it only reads the mix. Something must be rendering (even
    /// silence) for WASAPI to publish packets instead of the idle fallback.
    #[cfg(windows)]
    #[test]
    #[ignore = "opens the default output for loopback; run natively"]
    fn wasapi_startup_backlog_probe() {
        ffmpeg_next::init().unwrap();
        let delays = std::env::var("EM_PROBE_DELAYS_MS").unwrap_or_else(|_| "0,50,105,200".into());
        let trials: usize =
            std::env::var("EM_PROBE_TRIALS").map_or(5, |value| value.parse().unwrap());
        let mut late_starts = 0;
        for delay in delays.split(',').map(|value| value.parse::<u64>().unwrap()) {
            for trial in 0..trials {
                let source =
                    Source::Wasapi(Box::new(super::super::wasapi::WasapiSource::new().unwrap()));
                std::thread::sleep(Duration::from_millis(delay));
                let mut session = CaptureSession {
                    id: 1,
                    source,
                    resampler: None,
                    encoder: OpusEncoder::new(1000).unwrap(),
                    mark_discontinuity: true,
                    next_input_us: None,
                    started_us: None,
                    stream: 0,
                };
                let (tx, mut rx) = tokio::sync::mpsc::channel(super::super::QUEUE_CAPACITY);
                let (mut sent, mut stale, mut first_age, mut blocks) = (0, 0, None, 0);
                let until = Instant::now() + Duration::from_secs(1);
                while Instant::now() < until {
                    if let Some(block) = session.read().unwrap() {
                        blocks += 1;
                        session.send(block, &tx).unwrap();
                    }
                    while let Ok(queued) = rx.try_recv() {
                        let age =
                            crate::clock::host_now_us().saturating_sub(queued.packet.capture_ts_us);
                        first_age.get_or_insert(age);
                        sent += 1;
                        stale += usize::from(age > super::super::SEND_DEADLINE_US);
                    }
                }
                println!("PROBE delay_ms={delay} trial={trial} blocks={blocks} sent={sent} stale={stale} first_age_us={}", first_age.unwrap_or(0));
                late_starts += usize::from(stale > 0);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        assert_eq!(late_starts, 0, "starts that sent audio past the deadline");
    }
}
