//! PC audio capture, resampling, and low-delay Opus encoding.

pub mod encoder;
pub mod pcm;
pub mod resample;
mod stage;
pub mod synthetic;

pub use stage::run_audio_stage;
pub(crate) use stage::{report_unavailable, restart_stats};

use std::collections::VecDeque;
use std::time::Instant;

use crate::control::SharedControl;
use std::sync::atomic::Ordering;

#[cfg(windows)]
pub mod wasapi;

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 960;
pub const BITRATE: usize = 128_000;
pub type AudioResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Whether automation named an audio source (`ETERNAL_AUDIO`). The Mac
/// host captures no audio yet and its only source is a test tone, so it
/// streams audio only then.
pub fn source_requested() -> bool {
    std::env::var_os("ETERNAL_AUDIO").is_some()
}

#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub seq: u32,
    pub capture_ts_us: u64,
    pub discontinuity: bool,
    pub opus: Vec<u8>,
}

pub const QUEUE_CAPACITY: usize = 3;
/// The sender drops a packet captured longer ago than this.
pub const SEND_DEADLINE_US: u64 = 100_000;

pub struct SessionPacket {
    pub session_id: u32,
    /// The capture session that numbered this packet (`AudioStats::stream`).
    pub stream: u64,
    pub packet: AudioPacket,
}

pub fn wanted_session(shared: &SharedControl) -> Option<u32> {
    if shared.audio_enabled.load(Ordering::SeqCst) {
        shared.session.lock().audio_session_id()
    } else {
        None
    }
}

pub fn codec_available() -> bool {
    ffmpeg_next::encoder::find_by_name("libopus").is_some()
}

#[derive(Clone)]
pub struct AudioStats {
    pub device: String,
    pub status: String,
    pub error: Option<String>,
    pub kbps: f64,
    pub packets_per_sec: f64,
    pub packets: u64,
    pub quiet_packets: u64,
    pub dropped: u64,
    pub capture_queue_dropped: u64,
    pub stale_dropped: u64,
    pub oversized_dropped: u64,
    pub send_failed: u64,
    /// Sequence numbers dropped after missing the send deadline (stale or
    /// capture queue full), and those the host failed to send.
    pub late_seqs: SeqRanges,
    pub failed_seqs: SeqRanges,
    /// Each capture session numbers from 0. `stream` changes with every one,
    /// and `wire` is the session and epoch its packets went out under.
    pub stream: u64,
    pub wire: WireStream,
    samples: VecDeque<(Instant, usize)>,
}

static NEXT_STREAM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The wire session id and stream epoch of one capture session's packets.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum WireStream {
    #[default]
    Unknown,
    Known(u32, u32),
    Mixed,
}

impl std::fmt::Display for WireStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("none"),
            Self::Known(session_id, epoch) => write!(f, "{session_id}/{epoch}"),
            Self::Mixed => f.write_str("mixed"),
        }
    }
}

const MAX_SEQ_RANGES: usize = 32;

/// Inclusive sequence ranges for the stats log, such as `3-5,9`. At most
/// 32 ranges are kept; a trailing `+` marks drops that did not fit. A range
/// never crosses the u32 wrap.
#[derive(Clone)]
pub struct SeqRanges {
    ranges: Vec<(u32, u32)>,
    truncated: bool,
}

impl Default for SeqRanges {
    fn default() -> Self {
        Self {
            ranges: Vec::with_capacity(MAX_SEQ_RANGES),
            truncated: false,
        }
    }
}

impl SeqRanges {
    pub fn record(&mut self, seq: u32) {
        if let Some((_, end)) = self.ranges.last_mut() {
            if end.checked_add(1) == Some(seq) {
                *end = seq;
                return;
            }
        }
        if self.ranges.len() < MAX_SEQ_RANGES {
            self.ranges.push((seq, seq));
        } else {
            self.truncated = true;
        }
    }
}

impl std::fmt::Display for SeqRanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ranges.is_empty() && !self.truncated {
            return f.write_str("none");
        }
        for (index, &(start, end)) in self.ranges.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            if start == end {
                write!(f, "{start}")?;
            } else {
                write!(f, "{start}-{end}")?;
            }
        }
        if self.truncated {
            f.write_str(if self.ranges.is_empty() { "+" } else { ",+" })?;
        }
        Ok(())
    }
}

impl Default for AudioStats {
    fn default() -> Self {
        Self {
            device: String::new(),
            status: "Waiting for client".into(),
            error: None,
            kbps: 0.0,
            packets_per_sec: 0.0,
            packets: 0,
            quiet_packets: 0,
            dropped: 0,
            capture_queue_dropped: 0,
            stale_dropped: 0,
            oversized_dropped: 0,
            send_failed: 0,
            late_seqs: SeqRanges::default(),
            failed_seqs: SeqRanges::default(),
            stream: 0,
            wire: WireStream::Unknown,
            samples: VecDeque::with_capacity(64),
        }
    }
}

impl AudioStats {
    pub fn started(device: &str) -> Self {
        Self {
            device: device.into(),
            status: "Streaming".into(),
            stream: NEXT_STREAM.fetch_add(1, Ordering::Relaxed),
            ..Self::default()
        }
    }

    /// These stats, if they belong to the capture that numbered a packet.
    /// Capture replaces the stats when audio restarts, and a retired
    /// capture's leftover packets must never be charged to its replacement.
    pub fn of_stream(&mut self, stream: u64) -> Option<&mut Self> {
        (self.stream == stream).then_some(self)
    }

    /// Note the wire stream of a packet from this capture session.
    pub fn note_wire(&mut self, session_id: u32, epoch: u32) {
        self.wire = match self.wire {
            WireStream::Unknown => WireStream::Known(session_id, epoch),
            WireStream::Known(s, e) if (s, e) == (session_id, epoch) => self.wire,
            _ => WireStream::Mixed,
        };
    }

    pub fn stopped(&mut self, status: &str) {
        self.status = status.into();
        self.kbps = 0.0;
        self.packets_per_sec = 0.0;
        self.samples.clear();
    }

    pub fn record_sent(&mut self, payload_bytes: usize) {
        let now = Instant::now();
        self.packets += 1;
        self.quiet_packets += u64::from(payload_bytes < 10);
        self.samples.push_back((now, payload_bytes));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at).as_secs_f64() > 1.0)
        {
            self.samples.pop_front();
        }
        // The one-second rolling window also gives an honest startup ramp.
        self.packets_per_sec = self.samples.len() as f64;
        self.kbps =
            self.samples.iter().map(|(_, bytes)| *bytes).sum::<usize>() as f64 * 8.0 / 1000.0;
    }
}

#[cfg(test)]
mod tests {
    use super::SeqRanges;

    #[test]
    fn dropped_sequences_log_as_compact_ranges() {
        let mut ranges = SeqRanges::default();
        assert_eq!(ranges.to_string(), "none");
        for seq in [3, 4, 5, 9, 11, 12] {
            ranges.record(seq);
        }
        assert_eq!(ranges.to_string(), "3-5,9,11-12");
        for seq in (20..200).step_by(2) {
            ranges.record(seq);
        }
        let text = ranges.to_string();
        assert_eq!(
            text.split(',').count(),
            33,
            "32 ranges and a truncation mark"
        );
        assert!(text.ends_with(",+"));
    }

    #[test]
    fn a_range_never_crosses_the_sequence_wrap() {
        let mut ranges = SeqRanges::default();
        for seq in [u32::MAX - 1, u32::MAX, 0, 1] {
            ranges.record(seq);
        }
        assert_eq!(ranges.to_string(), "4294967294-4294967295,0-1");
    }

    #[test]
    fn a_capture_session_reports_one_wire_stream() {
        let mut stats = super::AudioStats::started("test");
        assert_eq!(stats.wire.to_string(), "none");
        stats.note_wire(7, 3);
        stats.note_wire(7, 3);
        assert_eq!(stats.wire.to_string(), "7/3");
        assert!(super::AudioStats::started("test").stream > stats.stream);
        stats.note_wire(7, 4);
        assert_eq!(stats.wire.to_string(), "mixed");
    }
}
