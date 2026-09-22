//! PC audio capture, resampling, and low-delay Opus encoding.

pub mod encoder;
pub mod pcm;
pub mod resample;
pub mod synthetic;

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
