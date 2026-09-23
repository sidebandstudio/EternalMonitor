//! Deterministic source: 1 kHz at -12 dBFS, with 100 ms of digital silence
//! at the end of each two-second period. Sample time never depends on polling.

use super::SAMPLE_RATE;

#[derive(Default)]
pub struct SyntheticSource {
    position: u64,
}

impl SyntheticSource {
    pub fn read(&mut self, count: usize) -> Vec<(f32, f32)> {
        (0..count)
            .map(|_| {
                let position = self.position;
                self.position += 1;
                let value = if position % (2 * u64::from(SAMPLE_RATE))
                    >= u64::from(SAMPLE_RATE) * 19 / 10
                {
                    0.0
                } else {
                    let phase = (position % 48) as f64 * std::f64::consts::TAU / 48.0;
                    (10.0_f64.powf(-12.0 / 20.0) * phase.sin()) as f32
                };
                (value, value)
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) fn tone_db(samples: &[f32], frequency: f64) -> f64 {
    let omega = std::f64::consts::TAU * frequency / f64::from(SAMPLE_RATE);
    let mut re = 0.0;
    let mut im = 0.0;
    for (index, &sample) in samples.iter().enumerate() {
        re += f64::from(sample) * (omega * index as f64).cos();
        im += f64::from(sample) * (omega * index as f64).sin();
    }
    20.0 * (2.0 * re.hypot(im) / samples.len() as f64)
        .max(1e-12)
        .log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_level_frequency_and_silence_are_sample_exact() {
        let mut source = SyntheticSource::default();
        let pcm = source.read(96_000);
        assert!(pcm.iter().all(|(left, right)| left == right));
        let left: Vec<_> = pcm[..4800].iter().map(|&(left, _)| left).collect();
        assert!((tone_db(&left, 1000.0) + 12.0).abs() < 0.01);
        assert!(tone_db(&left, 900.0) < -80.0);
        assert!(pcm[91_200..].iter().all(|&value| value == (0.0, 0.0)));
        assert!(source.read(960).iter().any(|&(value, _)| value.abs() > 0.2));
    }
}
