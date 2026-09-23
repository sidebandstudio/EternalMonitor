import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from e2e_stats import measure

SCRIPT = pathlib.Path(__file__).with_name("e2e_stats.py")
SLOW = """E2E_STATS decoded=60 fps=50 dropped=0 repaired=0 monotonic_ms=1000
E2E_STATS decoded=560 fps=50 dropped={dropped} repaired=0 monotonic_ms=11000"""


def gate(log, *args):
    with tempfile.TemporaryDirectory() as tmp:
        path = pathlib.Path(tmp, "app.log")
        path.write_text(log)
        output = pathlib.Path(tmp, "result.json")
        run = subprocess.run([sys.executable, str(SCRIPT), str(path), "--output", str(output), *args],
                             capture_output=True, text=True)
        return run.returncode, json.loads(output.read_text())


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


class FpsGate(unittest.TestCase):
    def test_default_gate_fails_below_55_fps(self):
        status, result = gate(SLOW.format(dropped=0))
        self.assertNotEqual(status, 0)
        self.assertEqual(result["status"], "FAIL")
        self.assertEqual(result["fps_gate"], "enforce")

    def test_recorded_gate_keeps_the_measurement_without_failing(self):
        status, result = gate(SLOW.format(dropped=0), "--fps-gate", "record")
        self.assertEqual(status, 0)
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(result["average_fps"], 50)
        self.assertEqual(result["notes"], ["average FPS 50.0 is below 55"])

    def test_recorded_gate_still_fails_dropped_frames(self):
        status, result = gate(SLOW.format(dropped=20), "--fps-gate", "record")
        self.assertNotEqual(status, 0)
        self.assertEqual(result["errors"], ["dropped 20 of 560 decoded frames"])


if __name__ == "__main__":
    unittest.main()
