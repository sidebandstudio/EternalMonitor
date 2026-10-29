"""artifact_secrets.py must keep every secret value out of the public artifacts."""
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent / "artifact_secrets.py"
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
OPTIONAL = {
    "AZURE_CLIENT_ID": "11111111-2222-3333-4444-555555555555",
    "AZURE_TENANT_URL": "https://tenant-test.example.org/t",
}


class ArtifactSecretsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def run_scan(self, *paths, event="push", names=",".join(ENV), optional=None, **overrides):
        env = {k: v for k, v in os.environ.items()
               if k not in ENV and k not in OPTIONAL and k != "GITHUB_EVENT_NAME"}
        env |= ENV | overrides | {"GITHUB_EVENT_NAME": event}
        flags = ["--optional", optional] if optional else []
        return subprocess.run([sys.executable, str(SCRIPT), "scan", "--secrets", names, *flags, *map(str, paths)],
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
        result = subprocess.run([sys.executable, str(SCRIPT), "scan", "--secrets", ",".join(ENV), str(clean)],
                                capture_output=True, text=True,
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

    def test_only_the_named_secrets_are_required_and_checked(self):
        names = "ETERNAL_DIAGNOSTICS_ENDPOINT,ETERNAL_DIAGNOSTICS_INGEST_KEY"
        others = {name: "" for name in ENV if name not in names.split(",")}
        self.assertEqual(self.run_scan(self.evidence("K1 issuer-0000-test"), names=names, **others).returncode, 0)
        result = self.run_scan(self.evidence("diag-test.example.net"), names=names, **others)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ETERNAL_DIAGNOSTICS_ENDPOINT host", result.stderr)

    def test_empty_optional_secrets_are_not_required(self):
        clean = self.evidence("{}\n")
        for values in ({}, {name: "" for name in OPTIONAL}, {name: "  \n" for name in OPTIONAL}):
            with self.subTest(values):
                result = self.run_scan(clean, optional=",".join(OPTIONAL), **values)
                self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_scan(clean, optional=",".join(OPTIONAL), ASC_ISSUER_ID="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ASC_ISSUER_ID", result.stderr)
        self.assertNotIn("AZURE", result.stderr)

    def test_set_optional_secrets_are_checked(self):
        leaks = {
            "AZURE_CLIENT_ID": "client 11111111-2222-3333-4444-555555555555",
            "AZURE_CLIENT_ID prefix": "client 11111111-222...",
            "AZURE_TENANT_URL host": "signed via tenant-test.example.org",
        }
        for name, text in leaks.items():
            with self.subTest(name):
                result = self.run_scan(self.evidence(text), optional=",".join(OPTIONAL), **OPTIONAL)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(name, result.stderr)
                self.assertNotIn("11111111", result.stdout + result.stderr)
        clean = self.run_scan(self.evidence("{}\n"), optional=",".join(OPTIONAL), **OPTIONAL)
        self.assertEqual(clean.returncode, 0, clean.stderr)

    def test_directories_are_scanned(self):
        (self.tmp / "logs").mkdir()
        (self.tmp / "logs" / "a.log").write_text("token cDEyLWJhc2U2NC10ZXN0")
        result = self.run_scan(self.tmp)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("IOS_DIST_P12_BASE64", result.stderr)

    def test_secret_names_are_required(self):
        clean = self.evidence("{}\n")
        for argv in (["scan", str(clean)], ["scan", "--secrets", "", str(clean)],
                     ["scan", "--secrets", "lower", str(clean)],
                     ["scan", "--secrets", "ASC_KEY_ID", "--optional", str(clean)],
                     ["scan", "--secrets", "ASC_KEY_ID", "--optional", "", str(clean)],
                     ["scan", "--secrets", "ASC_KEY_ID", "--optional", "lower", str(clean)]):
            with self.subTest(argv):
                result = subprocess.run([sys.executable, str(SCRIPT), *argv], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
