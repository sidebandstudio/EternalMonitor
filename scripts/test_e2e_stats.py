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

    def test_takeover_measures_only_the_requested_link(self):
        log = """E2E_FIRST_FRAME link=udp monotonic_ms=1000
E2E_STATS decoded=660 fps=60 monotonic_ms=12000
E2E_LINK_SWITCH from=udp to=usb monotonic_ms=12050
E2E_FIRST_FRAME link=usb monotonic_ms=12200
E2E_STATS decoded=60 fps=60 monotonic_ms=13000
E2E_STATS decoded=360 fps=60 monotonic_ms=18000"""
        result = measure(log, link="usb")
        self.assertEqual(result["switch_ms"], 150)
        self.assertEqual(result["average_fps"], 60)
        self.assertEqual(result["measured_seconds"], 5)
        with self.assertRaises(ValueError):
            measure(log)

    def test_wifi_frames_cannot_satisfy_a_usb_row(self):
        self.assertIsNone(measure("""E2E_FIRST_FRAME link=udp monotonic_ms=1000
E2E_STATS decoded=660 fps=60 monotonic_ms=12000""", link="usb"))

    def test_reconnecting_usb_cannot_hide_a_counter_reset(self):
        with self.assertRaises(ValueError):
            measure("""E2E_FIRST_FRAME link=usb monotonic_ms=1000
E2E_STATS decoded=60 fps=60 monotonic_ms=2000
E2E_FIRST_FRAME link=usb monotonic_ms=3000
E2E_STATS decoded=120 fps=60 monotonic_ms=5000""", link="usb")


if __name__ == "__main__":
    unittest.main()
