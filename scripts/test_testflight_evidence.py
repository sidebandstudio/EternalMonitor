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
    "ETERNAL_DIAGNOSTICS_INGEST_KEY": 'ing"est\\key-0123456789abcdef',
    "ASC_KEY_ID": "K1",
    "ASC_ISSUER_ID": "issuer-0000-test",
    "ASC_KEY_P8": "-----BEGIN PRIVATE KEY-----\nMIGTAgEAtest\n-----END PRIVATE KEY-----\n",
    "IOS_DIST_P12_BASE64": "cDEyLWJhc2U2NC10ZXN0",
    "IOS_DIST_P12_PASSWORD": "p12 pass'word",
    "IOS_PROVISION_PROFILE_BASE64": "cHJvZmlsZS1iYXNlNjQ=",
}


class TestFlightEvidenceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def run_scan(self, *paths, event="push", **overrides):
        env = {k: v for k, v in os.environ.items() if k not in ENV and k != "GITHUB_EVENT_NAME"}
        env |= ENV | overrides | {"GITHUB_EVENT_NAME": event}
        return subprocess.run([sys.executable, str(SCRIPT), "scan", *map(str, paths)],
                              capture_output=True, text=True, env=env)

    def evidence(self, text):
        path = self.tmp / "result.json"
        path.write_text(text)
        return path

    def test_clean_evidence_passes(self):
        clean = self.evidence('{"mode": "upload", "version": "0.3.3", "build": "316"}\n')
        result = self.run_scan(clean, self.tmp / "missing.json")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_scan_fails_without_printing_the_value(self):
        leak = self.evidence('{"key": "ing\\"est\\\\key-0123456789abcdef"}\n')
        result = self.run_scan(leak)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ETERNAL_DIAGNOSTICS_INGEST_KEY", result.stderr)
        self.assertNotIn("key-0123", result.stdout + result.stderr)

    def test_escaped_and_partial_forms_fail(self):
        leaks = {
            "ETERNAL_DIAGNOSTICS_INGEST_KEY": 'ETERNAL_DIAGNOSTICS_INGEST_KEY=ing\\"est\\\\key-01 ...',
            "ASC_KEY_P8": '"p8": "-----BEGIN PRIVATE KEY-----\\nMIGTAgEAtest\\n"',
            "IOS_DIST_P12_PASSWORD": "--password 'p12 pass'\"'\"'word'",
            "ETERNAL_DIAGNOSTICS_ENDPOINT host": "posted to diag-test.example.net",
        }
        for name, text in leaks.items():
            with self.subTest(name):
                result = self.run_scan(self.evidence(text))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(name, result.stderr)

    def test_short_values_are_checked(self):
        result = self.run_scan(self.evidence('{"asc": "K1"}\n'))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ASC_KEY_ID", result.stderr)

    def test_missing_or_empty_secret_fails_closed(self):
        clean = self.evidence("{}\n")
        for value in ("", "  \n"):
            result = self.run_scan(clean, ASC_ISSUER_ID=value)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("ASC_ISSUER_ID", result.stderr)
        env_without = {k: v for k, v in ENV.items() if k != "ASC_KEY_P8"}
        result = subprocess.run([sys.executable, str(SCRIPT), "scan", str(clean)], capture_output=True, text=True,
                                env={k: v for k, v in os.environ.items() if k not in ENV and k != "GITHUB_EVENT_NAME"}
                                | env_without)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ASC_KEY_P8", result.stderr)

    def test_pull_requests_scan_the_secrets_they_have(self):
        empty = {name: "" for name in ENV if name != "ASC_ISSUER_ID"}
        self.assertEqual(self.run_scan(self.evidence("{}\n"), event="pull_request", **empty).returncode, 0)
        result = self.run_scan(self.evidence("issuer-0000-test"), event="pull_request", **empty)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ASC_ISSUER_ID", result.stderr)


if __name__ == "__main__":
    unittest.main()
