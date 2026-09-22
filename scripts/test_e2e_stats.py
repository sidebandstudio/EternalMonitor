import unittest
from e2e_stats import measure


class StreamMeasurements(unittest.TestCase):
    def test_uses_streaming_interval_instead_of_startup_or_last_fps(self):
        result = measure("""E2E_STATS decoded=60 fps=60 monotonic_ms=10000 dropped=0
E2E_STATS decoded=660 fps=60 monotonic_ms=30000 dropped=0""")
        self.assertEqual(result["average_fps"], 30)
        self.assertEqual(result["measured_seconds"], 20)

    def test_counter_reset_cannot_pass_as_one_session(self):
        with self.assertRaises(ValueError):
            measure("""E2E_STATS decoded=100 fps=60 monotonic_ms=10000
E2E_STATS decoded=60 fps=60 monotonic_ms=12000""")

    def test_empty_log_waits_for_first_sample(self):
        self.assertIsNone(measure("E2E_FIRST_FRAME w=640 h=360"))


if __name__ == "__main__":
    unittest.main()
