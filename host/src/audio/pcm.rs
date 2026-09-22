//! Conversion of the interleaved PCM container formats returned by WASAPI.

use super::AudioResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleEncoding {
    Float32,
    Pcm16,
    Pcm24,
    Pcm32,
}

impl SampleEncoding {
    pub fn bytes(self) -> usize {
        match self {
            Self::Pcm16 => 2,
            Self::Pcm24 => 3,
            _ => 4,
        }
    }

    pub fn convert(self, bytes: &[u8]) -> AudioResult<Vec<f32>> {
        if !bytes.len().is_multiple_of(self.bytes()) {
            return Err("partial PCM sample".into());
        }
        bytes
            .chunks_exact(self.bytes())
            .map(|sample| {
                let value = match self {
                    Self::Float32 => f32::from_le_bytes(sample.try_into().unwrap()),
                    Self::Pcm16 => {
                        f32::from(i16::from_le_bytes(sample.try_into().unwrap())) / 32768.0
                    }
                    Self::Pcm24 => {
                        (i32::from_le_bytes([0, sample[0], sample[1], sample[2]]) >> 8) as f32
                            / 8388608.0
                    }
                    Self::Pcm32 => {
                        i32::from_le_bytes(sample.try_into().unwrap()) as f32 / 2147483648.0
                    }
                };
                if value.is_finite() {
                    Ok(value.clamp(-1.0, 1.0))
                } else {
                    Err("non-finite audio sample".into())
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_pcm_and_float_cover_zero_full_scale_and_unaligned_reads() {
        for (encoding, min, max) in [
            (SampleEncoding::Pcm16, vec![0, 128], vec![255, 127]),
            (SampleEncoding::Pcm24, vec![0, 0, 128], vec![255, 255, 127]),
            (
                SampleEncoding::Pcm32,
                vec![0, 0, 0, 128],
                vec![255, 255, 255, 127],
            ),
            (
                SampleEncoding::Float32,
                (-1.0_f32).to_le_bytes().to_vec(),
                1.0_f32.to_le_bytes().to_vec(),
            ),
        ] {
            let mut input = vec![99];
            input.extend(min);
            input.extend(vec![0; encoding.bytes()]);
            input.extend(max);
            let values = encoding.convert(&input[1..]).unwrap();
            assert_eq!(values[0], -1.0);
            assert_eq!(values[1], 0.0);
            assert!(values[2] > 0.999);
            assert!(encoding.convert(&input).is_err());
        }
        assert!(SampleEncoding::Float32
            .convert(&f32::NAN.to_le_bytes())
            .is_err());
        assert_eq!(
            SampleEncoding::Float32
                .convert(&2.0_f32.to_le_bytes())
                .unwrap(),
            vec![1.0]
        );
    }
}
