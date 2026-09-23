use ffmpeg::{codec, frame, ChannelLayout, Dictionary};
use ffmpeg_next as ffmpeg;

use super::{AudioPacket, AudioResult, BITRATE, FRAME_SAMPLES, SAMPLE_RATE};

const FORMAT: ffmpeg::format::Sample =
    ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed);

pub struct OpusEncoder {
    encoder: codec::encoder::Audio,
    seq: u32,
    pts: i64,
    quiet: bool,
    discontinuity: bool,
    max_payload: usize,
}

fn open_encoder() -> AudioResult<codec::encoder::Audio> {
    let codec = ffmpeg::encoder::find_by_name("libopus").ok_or("libopus encoder unavailable")?;
    let mut encoder = codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()?;
    encoder.set_rate(SAMPLE_RATE as i32);
    encoder.set_channel_layout(ChannelLayout::STEREO);
    encoder.set_format(FORMAT);
    encoder.set_bit_rate(BITRATE);
    encoder.set_time_base((1, SAMPLE_RATE as i32));
    let mut options = Dictionary::new();
    options.set("application", "lowdelay");
    options.set("frame_duration", "20");
    options.set("fec", "1");
    options.set("packet_loss", "10");
    options.set("vbr", "constrained");
    // FFmpeg 7's libopus wrapper has no dtx AVOption. Do not silently pass
    // an ignored option or claim that CELT supplies SILK in-band FEC.
    let encoder = encoder.open_as_with(codec, options)?;
    if encoder.frame_size() != FRAME_SAMPLES as u32 {
        return Err("libopus did not accept 20 ms frames".into());
    }
    Ok(encoder)
}

impl OpusEncoder {
    pub fn new(max_payload: usize) -> AudioResult<Self> {
        ffmpeg::init()?;
        Ok(Self {
            encoder: open_encoder()?,
            seq: 0,
            pts: 0,
            quiet: false,
            discontinuity: true,
            max_payload,
        })
    }

    pub fn encode(&mut self, pcm: &[(f32, f32)], capture_ts_us: u64) -> AudioResult<AudioPacket> {
        if pcm.len() != FRAME_SAMPLES || pcm.iter().any(|&(l, r)| !l.is_finite() || !r.is_finite())
        {
            return Err("Opus input must contain 960 finite stereo samples".into());
        }
        let quiet = pcm.iter().all(|&(l, r)| l == 0.0 && r == 0.0);
        if quiet && !self.quiet {
            // Clear the encoder's tonal history at digital silence. The new
            // stream's genuine Opus silence packets are three bytes. Tell the
            // receiver to reset its codec prediction, while preserving time.
            self.drain_tail();
            self.encoder = open_encoder()?;
            self.discontinuity = true;
        }
        self.quiet = quiet;
        let mut input = frame::Audio::new(FORMAT, FRAME_SAMPLES, ChannelLayout::STEREO);
        input.set_rate(SAMPLE_RATE);
        input.set_pts(Some(self.pts));
        input.plane_mut::<(f32, f32)>(0).copy_from_slice(pcm);
        self.encoder.send_frame(&input)?;
        let mut output = ffmpeg::Packet::empty();
        self.encoder.receive_packet(&mut output)?;
        let opus = output
            .data()
            .ok_or("libopus returned an empty packet")?
            .to_vec();
        if opus.is_empty() || opus.len() > self.max_payload {
            return Err(format!(
                "Opus packet {} exceeds audio payload limit {}",
                opus.len(),
                self.max_payload
            )
            .into());
        }
        let packet = AudioPacket {
            seq: self.seq,
            capture_ts_us,
            discontinuity: std::mem::take(&mut self.discontinuity),
            opus,
        };
        self.seq = self.seq.wrapping_add(1);
        self.pts += FRAME_SAMPLES as i64;
        Ok(packet)
    }

    pub fn reopen(&mut self) -> AudioResult<()> {
        self.drain_tail();
        self.encoder = open_encoder()?;
        self.quiet = false;
        self.discontinuity = true;
        Ok(())
    }

    fn drain_tail(&mut self) {
        // The low-delay lookahead leaves a partial frame at EOF. A live
        // restart replaces that tail with a marked discontinuity, rather
        // than inserting another 20 ms into the capture timeline.
        let _ = self.encoder.send_eof();
        let mut packet = ffmpeg::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {}
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        self.drain_tail();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::synthetic::{tone_db, SyntheticSource};

    fn decoder() -> codec::decoder::Audio {
        let codec = ffmpeg::decoder::find_by_name("opus").unwrap();
        let mut context = codec::context::Context::new_with_codec(codec);
        // Raw wire packets carry stereo/48 kHz by protocol, without an Ogg
        // OpusHead. Set those same parameters before opening the decoder.
        unsafe {
            let raw = context.as_mut_ptr();
            ffmpeg::ffi::av_channel_layout_default(&mut (*raw).ch_layout, 2);
            (*raw).sample_rate = SAMPLE_RATE as i32;
        }
        context.decoder().audio().unwrap()
    }

    #[test]
    fn tone_silence_transitions_decode_without_a_burst_and_fit_small_datagrams() {
        let mut encoder = OpusEncoder::new(576 - 28).unwrap();
        let mut decoder = decoder();
        let mut source = SyntheticSource::default();
        let mut output = Vec::new();
        let mut sizes = Vec::new();
        for index in 0..210 {
            let packet = encoder
                .encode(&source.read(FRAME_SAMPLES), index * 20_000)
                .unwrap();
            assert_eq!(packet.seq, index as u32);
            assert_eq!(packet.capture_ts_us, index * 20_000);
            if packet.discontinuity {
                decoder.flush();
            }
            assert_eq!(
                packet.discontinuity,
                index == 0 || index == 95 || index == 195
            );
            if (95..100).contains(&index) || (195..200).contains(&index) {
                assert!(
                    packet.opus.len() < 10,
                    "silence frame {index}: {} bytes",
                    packet.opus.len()
                );
            }
            sizes.push(packet.opus.len());
            decoder
                .send_packet(&ffmpeg::Packet::copy(&packet.opus))
                .unwrap();
            let mut decoded = frame::Audio::empty();
            decoder.receive_frame(&mut decoded).unwrap();
            assert_eq!(decoded.samples(), FRAME_SAMPLES);
            assert_eq!(
                decoded.format(),
                ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Planar)
            );
            output.extend_from_slice(decoded.plane::<f32>(0));
        }
        let peak = output.iter().copied().map(f32::abs).fold(0.0, f32::max);
        eprintln!(
            "audio packets={} min={} max={} peak={peak}",
            sizes.len(),
            sizes.iter().min().unwrap(),
            sizes.iter().max().unwrap()
        );
        assert!(peak < 0.35, "unexpected transition burst: {peak}");
        for start in [4800, 100 * FRAME_SAMPLES + 4800, 200 * FRAME_SAMPLES + 4800] {
            let pcm = &output[start..start + 4800];
            assert!((tone_db(pcm, 1000.0) + 12.0).abs() < 1.0);
            assert!(tone_db(pcm, 900.0) < -40.0);
        }
        assert!(output[96 * FRAME_SAMPLES..100 * FRAME_SAMPLES]
            .iter()
            .all(|s| s.abs() < 0.0001));
    }

    #[test]
    fn full_scale_noise_and_transients_fit_the_minimum_packet_size() {
        let mut encoder = OpusEncoder::new(576 - 28).unwrap();
        let mut random = 3_u32;
        for index in 0..300 {
            let pcm: Vec<_> = (0..FRAME_SAMPLES)
                .map(|sample| {
                    random ^= random << 13;
                    random ^= random >> 17;
                    random ^= random << 5;
                    let noise = (random as f64 / f64::from(u32::MAX) * 2.0 - 1.0) as f32;
                    match index % 4 {
                        0 => (noise, -noise),
                        1 => (0.0, 0.0),
                        2 => (if sample % 2 == 0 { 1.0 } else { -1.0 }, noise),
                        _ => (1.0, -1.0),
                    }
                })
                .collect();
            let packet = encoder.encode(&pcm, index * 20_000).unwrap();
            assert!(packet.opus.len() <= 548);
        }
    }
}
