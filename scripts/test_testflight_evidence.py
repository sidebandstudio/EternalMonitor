"""testflight_evidence.py must keep every secret value out of the public artifact."""
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent / "testflight_evidence.py"
ENV = {
    "ETERNAL_DIAGNOSTICS_ENDPOINT": "https://diag-test.example.net/ingest",
    "ETERNAL_DIAGNOSTICS_INGEST_KEY": "ingest-key-0123456789abcdef",
    "IOS_DIST_P12_PASSWORD": "p12-password-test",
    "ASC_KEY_ID": "",
}


class TestFlightEvidenceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def run_script(self, *args):
        env = {k: v for k, v in os.environ.items() if k not in ENV} | ENV
        return subprocess.run([sys.executable, str(SCRIPT), *map(str, args)],
                              capture_output=True, text=True, env=env)

    def test_tail_redacts_values_and_scan_passes(self):
        log = self.tmp / "archive.log"
        lines = [f"line {i}\n" for i in range(100)]
        lines[-3] = ("    ETERNAL_DIAGNOSTICS_ENDPOINT=https://diag-test.example.net/ingest "
                     "ETERNAL_DIAGNOSTICS_INGEST_KEY=ingest-key-0123456789abcdef\n")
        lines[-2] = "host diag-test.example.net, password p12-password-test\n"
        log.write_text("".join(lines))
        out = self.tmp / "archive-tail.log"
        self.assertEqual(self.run_script("tail", log, out).returncode, 0)
        text = out.read_text()
        self.assertEqual(len(text.splitlines()), 70)
        self.assertNotIn("diag-test", text)
        self.assertNotIn("ingest-key", text)
        self.assertNotIn("p12-password", text)
        result = self.run_script("scan", out)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_scan_fails_without_printing_the_value(self):
        leak = self.tmp / "result.json"
        leak.write_text('{"key": "ingest-key-0123456789abcdef"}\n')
        result = self.run_script("scan", leak, self.tmp / "missing.log")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ETERNAL_DIAGNOSTICS_INGEST_KEY", result.stderr)
        self.assertNotIn("ingest-key-0123", result.stdout + result.stderr)

    def test_tail_of_missing_log_is_empty(self):
        out = self.tmp / "archive-tail.log"
        self.assertEqual(self.run_script("tail", self.tmp / "archive.log", out).returncode, 0)
        self.assertEqual(out.read_bytes(), b"")


if __name__ == "__main__":
    unittest.main()
