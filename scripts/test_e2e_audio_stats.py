import unittest
from e2e_audio_stats import measure


def sample(decoded=100, lost=0, tone=-12, rms=0.177):
    return f'E2E_AUDIO packets={decoded} decoded={decoded} lost={lost} buffer_ms=60 rms={rms} tone1k_db={tone}\n'


class AudioMeasurementTests(unittest.TestCase):
    def test_accepts_sustained_tone_with_deliberate_quiet_window(self):
        result = measure(sample(50) + sample(100) + sample(150, tone=-120, rms=0))
        self.assertEqual(result['audio_decoded'], 150)
        self.assertEqual(result['audio_tone1k_db'], -12)

    def test_requires_rendered_tone_and_packet_count(self):
        for log in ('', sample(50) * 2, sample(tone=-120, rms=0) * 2, sample()):
            with self.subTest(log=log), self.assertRaises(ValueError):
                measure(log)

    def test_loss_must_not_be_hidden_by_a_later_counter_reset(self):
        with self.assertRaises(ValueError):
            measure(sample(lost=3) + sample(lost=0))


if __name__ == '__main__':
    unittest.main()
