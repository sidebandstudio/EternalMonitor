//! PC audio capture, resampling, and low-delay Opus encoding.

pub mod encoder;
pub mod pcm;
pub mod resample;
mod stage;
pub mod synthetic;

pub use stage::run_audio_stage;

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

#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub seq: u32,
    pub capture_ts_us: u64,
    pub discontinuity: bool,
    pub opus: Vec<u8>,
}

pub const QUEUE_CAPACITY: usize = 3;

pub struct SessionPacket {
    pub session_id: u32,
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
    samples: VecDeque<(Instant, usize)>,
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
            samples: VecDeque::with_capacity(64),
        }
    }
}

impl AudioStats {
    pub fn started(device: &str) -> Self {
        Self {
            device: device.into(),
            status: "Streaming".into(),
            ..Self::default()
        }
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
