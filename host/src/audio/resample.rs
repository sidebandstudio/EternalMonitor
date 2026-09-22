use std::collections::VecDeque;

use ffmpeg::{frame, ChannelLayout};
use ffmpeg_next as ffmpeg;

use super::{AudioResult, FRAME_SAMPLES, SAMPLE_RATE};

const FORMAT: ffmpeg::format::Sample =
    ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub rate: u32,
    pub channels: u16,
    /// Windows speaker mask; zero uses FFmpeg's standard layout for the
    /// channel count. WASAPI and FFmpeg share the native speaker bit order.
    pub channel_mask: u32,
}

impl PcmFormat {
    pub const STEREO_48K: Self = Self {
        rate: SAMPLE_RATE,
        channels: 2,
        channel_mask: 3,
    };

    fn layout(self) -> AudioResult<ChannelLayout> {
        if !(8000..=384_000).contains(&self.rate) || !(1..=8).contains(&self.channels) {
            return Err("unsupported audio mix format".into());
        }
        let mut layout = ChannelLayout::default(i32::from(self.channels));
        if self.channel_mask != 0 {
            if self.channel_mask.count_ones() != u32::from(self.channels) {
                return Err("audio channel mask disagrees with channel count".into());
            }
            // A native speaker mask owns no heap allocation. Validation above
            // keeps AVChannelLayout's count and mask consistent.
            layout.0.u.mask = u64::from(self.channel_mask);
        }
        Ok(layout)
    }
}

pub struct PcmBlock {
    pub format: PcmFormat,
    pub samples: Vec<f32>,
    pub capture_ts_us: u64,
    pub discontinuity: bool,
}

pub struct PcmFrame {
    pub samples: Vec<(f32, f32)>,
    pub capture_ts_us: u64,
}

/// swresample owns rate conversion/downmixing; the bounded remainder only
/// assembles 960-sample Opus frames. Recreate on endpoint/format changes.
pub struct Resampler {
    format: PcmFormat,
    layout: ChannelLayout,
    context: ffmpeg::software::resampling::Context,
    pending: VecDeque<(f32, f32)>,
    anchor_us: Option<u64>,
    emitted_samples: u64,
}

impl Resampler {
    pub fn new(format: PcmFormat) -> AudioResult<Self> {
        let layout = format.layout()?;
        let context = ffmpeg::software::resampling::Context::get(
            FORMAT,
            layout,
            format.rate,
            FORMAT,
            ChannelLayout::STEREO,
            SAMPLE_RATE,
        )?;
        Ok(Self {
            format,
            layout,
            context,
            pending: VecDeque::with_capacity(FRAME_SAMPLES * 2),
            anchor_us: None,
            emitted_samples: 0,
        })
    }

    pub fn format(&self) -> PcmFormat {
        self.format
    }

    pub fn push(&mut self, block: &PcmBlock) -> AudioResult<Vec<PcmFrame>> {
        let channels = usize::from(self.format.channels);
        let count = block.samples.len() / channels;
        if block.format != self.format
            || !block.samples.len().is_multiple_of(channels)
            || count == 0
            || count > self.format.rate as usize / 10
            || block.samples.iter().any(|sample| !sample.is_finite())
        {
            return Err("invalid PCM block (maximum 100 ms)".into());
        }
        self.anchor_us.get_or_insert(block.capture_ts_us);
        let mut input = frame::Audio::new(FORMAT, count, self.layout);
        input.set_rate(self.format.rate);
        for (dst, value) in input
            .data_mut(0)
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&block.samples)
        {
            dst.copy_from_slice(&value.to_ne_bytes());
        }
        // swr_get_out_samples includes fractional rate-conversion delay. A
        // same-sized output buffer would accumulate latency when upsampling.
        let capacity =
            unsafe { ffmpeg::ffi::swr_get_out_samples(self.context.as_mut_ptr(), count as i32) };
        if !(0..=SAMPLE_RATE as i32 / 5).contains(&capacity) {
            return Err("audio resampler exceeded its latency bound".into());
        }
        let mut output = frame::Audio::new(FORMAT, capacity as usize, ChannelLayout::STEREO);
        self.context.run(&input, &mut output)?;
        self.pending.extend(output.plane::<(f32, f32)>(0));
        let mut frames = Vec::new();
        while self.pending.len() >= FRAME_SAMPLES {
            let capture_ts_us =
                self.anchor_us.unwrap() + self.emitted_samples * 1_000_000 / u64::from(SAMPLE_RATE);
            frames.push(PcmFrame {
                samples: self.pending.drain(..FRAME_SAMPLES).collect(),
                capture_ts_us,
            });
            self.emitted_samples += FRAME_SAMPLES as u64;
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::synthetic::tone_db;

    #[test]
    fn resamples_44100_mono_without_rate_drift_or_a_growing_remainder() {
        let format = PcmFormat {
            rate: 44100,
            channels: 1,
            channel_mask: 4,
        };
        let mut resampler = Resampler::new(format).unwrap();
        let mut all = Vec::new();
        for block_index in 0..1001 {
            let block = PcmBlock {
                format,
                capture_ts_us: 1_000_000 + block_index * 10_000,
                discontinuity: false,
                samples: (0..441)
                    .map(|i| {
                        let position = block_index * 441 + i;
                        (0.25 * (position as f64 * std::f64::consts::TAU * 1000.0 / 44100.0).sin())
                            as f32
                    })
                    .collect(),
            };
            for frame in resampler.push(&block).unwrap() {
                assert_eq!(
                    frame.capture_ts_us,
                    1_000_000 + all.len() as u64 * 1_000_000 / 48_000
                );
                assert_eq!(frame.samples.len(), 960);
                assert!(frame.samples.iter().all(|&(l, r)| (l - r).abs() < 1e-6));
                all.extend(frame.samples.iter().map(|&(l, _)| l));
            }
            assert!(resampler.pending.len() < FRAME_SAMPLES);
        }
        assert_eq!(all.len(), 480_000);
        assert!(tone_db(&all[4800..9600], 1000.0) > -17.0);
        assert!(tone_db(&all[4800..9600], 900.0) < -60.0);
    }

    #[test]
    fn rejects_invalid_channel_masks_and_unbounded_input() {
        assert!(Resampler::new(PcmFormat {
            channels: 2,
            channel_mask: 4,
            ..PcmFormat::STEREO_48K
        })
        .is_err());
        let mut resampler = Resampler::new(PcmFormat::STEREO_48K).unwrap();
        for samples in [vec![0.0; 9601], vec![f32::NAN; 960], vec![]] {
            assert!(resampler
                .push(&PcmBlock {
                    format: PcmFormat::STEREO_48K,
                    samples,
                    capture_ts_us: 0,
                    discontinuity: false
                })
                .is_err());
        }
    }
}
