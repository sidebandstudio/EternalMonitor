"""latest_json.py must only ever point the apps at a release people can install."""
import contextlib
import io
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import latest_json  # noqa: E402
import testflight_review  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
FILES = ["scripts/check-versions.sh", "scripts/manifest_fields.py", "host/Cargo.toml", "proto/Cargo.toml",
         "ios/project.yml", "docs/download.html", "docs/latest.json", "docs/script.js"]
# The optional fields the Rust and Swift tests read too.
EXAMPLE = json.loads((ROOT / "proto/testdata/latest_remote_config.json").read_text())
SHA = {"0.3.2": "2" * 64, "0.3.3": "3" * 64, "0.3.4": "4" * 64}


def release(version, sha=None, digest=True, **extra):
    sha = sha or SHA[version]
    asset = {"name": "EternalMonitor-Setup.exe", "size": 62849158,
             "browser_download_url": latest_json.installer_url(version)}
    if digest:
        asset["digest"] = "sha256:" + (digest if isinstance(digest, str) else sha)
    return {"tag_name": f"v{version}", "draft": False, "prerelease": False, "assets": [asset],
            "body": f"**EternalMonitor-Setup.exe SHA-256:**\n\n```\n{sha}\n```\n", **extra}


def manifest(windows, ipad, build, **windows_extra):
    return {"schema": 1,
            "windows": {"version": windows, "installer": latest_json.installer_url(windows), "sha256": SHA[windows],
                        **windows_extra},
            "ipad": {"version": ipad, "build": build, "testflight": "https://testflight.apple.com/join/ppc9QfXc"}}


class FakeTestFlight:
    """App Store Connect's answer for one build lookup."""

    def __init__(self, review="APPROVED", expired=False, external=True):
        self.review, self.expired, self.external, self.lookups = review, expired, external, []

    def __call__(self, method, url, headers, data):
        self.lookups.append(url)
        version = re.search(r"filter%5Bversion%5D=(\d+)", url)[1]
        doc = {"data": [{"type": "builds", "id": "b", "attributes": {"version": version, "expired": self.expired},
                         "relationships": {"betaAppReviewSubmission": {"data": {"type": "betaAppReviewSubmissions", "id": "b"}},
                                           "betaGroups": {"data": [{"type": "betaGroups", "id": "g"}]}}}],
               "included": [{"type": "betaAppReviewSubmissions", "id": "b", "attributes": {"betaReviewState": self.review}},
                            {"type": "betaGroups", "id": "g", "attributes": {"isInternalGroup": not self.external}}]}
        return 200, json.dumps(doc).encode()


class LatestJsonTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        for name in FILES:
            (self.tmp / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / name, self.tmp / name)
        self.releases = {f"v{v}": release(v) for v in SHA}
        self.downloads = []
        self.asc = None
        # The tree at the published 0.3.3, whatever the repository's version.
        self.bump_tree("0.3.3")
        self.git("init", "-q")
        # History: 0.3.2 everywhere, then Windows 0.3.3 and iPad 0.3.3 build 364.
        self.publish_state(manifest("0.3.2", "0.3.2", 316))
        self.git("add", ".")
        self.git("commit", "-qm", "0.3.2")
        self.publish_state(manifest("0.3.3", "0.3.3", 364, note="Fixes Second screen."))
        self.git("commit", "-qam", "0.3.3")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.tmp), "-c", "user.name=t", "-c", "user.email=t@example.com", *args],
                       check=True, capture_output=True)

    def publish_state(self, latest):
        page = self.tmp / "docs/download.html"
        page.write_text(latest_json.update_page(page.read_text(), latest, None))
        (self.tmp / "docs/latest.json").write_text(json.dumps(latest, indent=2) + "\n")

    def bump_tree(self, version):
        for name in ("host/Cargo.toml", "proto/Cargo.toml", "ios/project.yml"):
            path = self.tmp / name
            text = path.read_text()
            current = re.search(r'MARKETING_VERSION:\s*"([^"]+)"', (self.tmp / "ios/project.yml").read_text())[1]
            path.write_text(text.replace(f'"{current}"', f'"{version}"', 1) if name != "ios/project.yml"
                            else text.replace(f'MARKETING_VERSION: "{current}"', f'MARKETING_VERSION: "{version}"'))

    def connect(self):
        return self.asc and testflight_review.AppStoreConnect("K1", "issuer", b"pem", transport=self.asc,
                                                              sign=lambda pem, message: b"\1" * 64)

    def run_tool(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            latest_json.main(list(argv), root=self.tmp, release=self.releases.__getitem__,
                             download=lambda url, size: self.downloads.append(url) or self.download_sha,
                             connect=self.connect)
        return out.getvalue()

    def fails(self, *argv):
        before = {name: (self.tmp / name).read_text() for name in ("docs/latest.json", "docs/download.html")}
        with self.assertRaises(SystemExit) as raised, contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(io.StringIO()):
            self.run_tool(*argv)
        for name, text in before.items():
            self.assertEqual((self.tmp / name).read_text(), text, name)
        return str(raised.exception.code)

    def latest(self):
        return json.loads((self.tmp / "docs/latest.json").read_text())

    def test_rollback_restores_the_previous_release(self):
        out = self.run_tool("rollback", "--windows", "0.3.2", "--ipad", "0.3.2")
        self.assertEqual(self.latest(), manifest("0.3.2", "0.3.2", 316))
        page = (self.tmp / "docs/download.html").read_text()
        for text in ("data-win-version>v0.3.2<", "releases/download/v0.3.2/EternalMonitor-Setup.exe\" id=\"download-btn\"",
                     "releases/tag/v0.3.2\"", f">{SHA['0.3.2']}<", "data-ipad-version>v0.3.2, build 316<",
                     "EternalMonitor-Setup.exe · 59.9 MB", "Windows v0.3.2 · iPad v0.3.2, TestFlight build 316"):
            self.assertIn(text, page)
        self.assertIn("Published update: Windows 0.3.2, iPad 0.3.2 build 316", out)
        self.assertIn("git switch -c rollback-windows-0.3.2-ipad-0.3.2-build-316", out)
        self.assertIn("gh pr create --base main", out)
        self.assertIn("App Store Connect was not checked", out)
        self.assertEqual(self.downloads, [])

    def test_windows_only_rollback_keeps_the_ipad(self):
        self.publish_state(manifest("0.3.3", "0.3.2", 316, note="Fixes Second screen.", recommended=True))
        self.run_tool("rollback", "--windows", "0.3.2")
        self.assertEqual(self.latest(), manifest("0.3.2", "0.3.2", 316))

    def test_rollback_that_would_leave_the_ipad_ahead_restores_both_files(self):
        message = self.fails("rollback", "--windows", "0.3.2")
        self.assertIn("check-versions.sh failed", message)
        self.assertIn("iPad 0.3.3 is newer than Windows 0.3.2", message)

    def test_missing_digest_downloads_the_installer(self):
        self.releases["v0.3.2"] = release("0.3.2", digest=False)
        self.download_sha = SHA["0.3.2"]
        self.run_tool("rollback", "--windows", "0.3.2", "--ipad", "0.3.2")
        self.assertEqual(self.downloads, [latest_json.installer_url("0.3.2")])
        self.download_sha = "9" * 64
        self.publish_state(manifest("0.3.3", "0.3.3", 364))
        self.assertIn("differs from its release body", self.fails("rollback", "--windows", "0.3.2", "--ipad", "0.3.2"))

    def test_unpublishable_releases_are_refused(self):
        cases = {
            "draft": release("0.3.2", draft=True),
            "prerelease": release("0.3.2", prerelease=True),
            "digest": release("0.3.2", digest="8" * 64),
            "body": {**release("0.3.2"), "body": "no hash"},
            "asset": {**release("0.3.2"), "assets": []},
        }
        for name, data in cases.items():
            with self.subTest(name):
                self.releases["v0.3.2"] = data
                self.fails("rollback", "--windows", "0.3.2", "--ipad", "0.3.2")

    def test_rollback_must_go_back_to_something_published(self):
        self.assertIn("not older", self.fails("rollback", "--windows", "0.3.3"))
        self.assertIn("never published iPad 0.3.1", self.fails("rollback", "--ipad", "0.3.1"))
        self.assertIn("not older", self.fails("rollback", "--ipad", "0.3.3"))
        self.assertIn("use rollback", self.fails("publish", "--windows", "0.3.2"))

    def test_rollback_checks_testflight_when_it_can(self):
        self.asc = FakeTestFlight(expired=True)
        self.assertIn("expired", self.fails("rollback", "--windows", "0.3.2", "--ipad", "0.3.2"))
        self.asc = FakeTestFlight()
        out = self.run_tool("rollback", "--windows", "0.3.2", "--ipad", "0.3.2")
        self.assertNotIn("not checked", out)
        self.assertIn("filter%5Bversion%5D=316", self.asc.lookups[0])

    def test_publish_moves_forward_with_an_approved_build(self):
        self.bump_tree("0.3.4")
        self.asc = FakeTestFlight()
        self.run_tool("publish", "--windows", "0.3.4", "--ipad", "0.3.4", "--build", "401",
                      "--note", "  Faster first picture.  ", "--not-recommended")
        latest = self.latest()
        self.assertEqual(latest["windows"], {**manifest("0.3.4", "0.3.4", 401)["windows"],
                                             "note": "Faster first picture.", "recommended": False})
        self.assertEqual(latest["ipad"]["build"], 401)
        self.assertEqual((latest["ipad"]["note"], latest["ipad"]["recommended"]), ("Faster first picture.", False))

    def test_publish_refuses_unapproved_or_unchecked_builds(self):
        self.bump_tree("0.3.4")
        self.assertIn("checks App Store Connect", self.fails("publish", "--ipad", "0.3.4", "--build", "401"))
        for asc, why in ((FakeTestFlight(review="WAITING_FOR_REVIEW"), "not approved"),
                         (FakeTestFlight(external=False), "no external group")):
            self.asc = asc
            with self.subTest(why):
                self.assertIn(why, self.fails("publish", "--windows", "0.3.4", "--ipad", "0.3.4", "--build", "401"))
        self.asc = FakeTestFlight()
        self.assertIn("use rollback", self.fails("publish", "--ipad", "0.3.4", "--build", "300"))

    def test_publish_refuses_an_older_ipad_version_with_a_newer_build(self):
        # The iPad app compares versions before builds, so 0.3.2 build 401 is older than 0.3.3 build 364.
        self.bump_tree("0.3.4")
        self.asc = FakeTestFlight()
        self.assertIn("use rollback", self.fails("publish", "--ipad", "0.3.2", "--build", "401"))

    def test_a_version_check_that_cannot_start_restores_both_files(self):
        self.bump_tree("0.3.4")
        with mock.patch.object(latest_json.subprocess, "run", side_effect=FileNotFoundError("bash")):
            self.assertIn("both files were restored", self.fails("publish", "--windows", "0.3.4"))

    def test_publish_newer_than_the_tree_is_refused(self):
        self.assertIn("newer than this tree", self.fails("publish", "--windows", "0.3.4"))

    def test_bad_arguments(self):
        for argv in (["rollback"], ["publish", "--ipad", "0.3.4"], ["rollback", "--ipad", "0.3.2", "--build", "316"],
                     ["publish", "--windows", "v0.3.4"], ["publish", "--windows", "0.3.3", "--note", "two\nlines"],
                     ["publish", "--windows", "0.3.3", "--note", "x" * 201]):
            with self.subTest(argv):
                self.fails(*argv)

    def test_manifest_holds_only_fields_the_apps_accept(self):
        latest_json.safe_manifest(manifest("0.3.3", "0.3.3", 364, note="ok", recommended=True))
        for change in ({"minimum": "0.3.3"}, {"recommended": "yes"}, {"note": " padded "}, {"sha256": "A" * 64}):
            with self.subTest(change), self.assertRaises(latest_json.ManifestError):
                bad = manifest("0.3.3", "0.3.3", 364)
                bad["windows"].update(change)
                latest_json.safe_manifest(bad)
        bad = manifest("0.3.3", "0.3.3", 364)
        bad["ipad"]["required"] = True
        with self.assertRaises(latest_json.ManifestError):
            latest_json.safe_manifest(bad)


    # ── Optional fields (docs/remote-config.md), checked as check-versions.sh does ──

    def soon(self, days=1):
        import datetime
        when = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=days)
        return when.strftime("%Y-%m-%dT%H:%M:%SZ")

    def config_file(self, name, config):
        path = self.tmp / name
        path.write_text(json.dumps(config))
        return str(path)

    def with_configs(self):
        latest = self.latest()
        latest["windows"]["config"] = EXAMPLE["windows"]["config"]
        latest["ipad"]["config"] = EXAMPLE["ipad"]["config"]
        self.publish_state(latest)

    def test_publish_stages_the_prompt_and_keeps_each_config(self):
        self.with_configs()
        self.bump_tree("0.3.4")
        self.asc = FakeTestFlight()
        start = self.soon()
        self.run_tool("publish", "--windows", "0.3.4", "--ipad", "0.3.4", "--build", "401",
                      "--rollout-start", start, "--rollout-days", "7")
        latest = self.latest()
        for platform in ("windows", "ipad"):
            self.assertEqual(latest[platform]["rollout"], {"start": start, "days": 7})
            self.assertEqual(latest[platform]["config"], EXAMPLE[platform]["config"], platform)
        # The next publish without a rollout drops it, as it drops the note.
        self.bump_tree("0.3.5")
        self.releases["v0.3.5"] = release("0.3.5", sha="5" * 64)
        self.run_tool("publish", "--windows", "0.3.5")
        self.assertNotIn("rollout", self.latest()["windows"])
        self.assertEqual(self.latest()["windows"]["config"], EXAMPLE["windows"]["config"])

    def test_a_config_can_change_without_a_release(self):
        config = {"schema": 1, "attach_timeout_s": 45}
        self.run_tool("publish", "--windows-config", self.config_file("w.json", config))
        latest = self.latest()
        self.assertEqual(latest["windows"]["config"], config)
        self.assertEqual(latest["windows"]["version"], "0.3.3")
        self.assertEqual(latest["windows"]["note"], "Fixes Second screen.", "nothing else changes")
        out = self.run_tool("publish", "--windows-config", "none",
                            "--ipad-config", self.config_file("i.json", EXAMPLE["ipad"]["config"]))
        self.assertNotIn("config", self.latest()["windows"])
        self.assertEqual(self.latest()["ipad"]["config"], EXAMPLE["ipad"]["config"])
        self.assertIn("Windows config and iPad config", out)

    def test_bad_rollouts_configs_and_notes_are_refused(self):
        self.bump_tree("0.3.4")
        bad_rollouts = [(self.soon(), "15"), (self.soon(15), "7"), ("2026-11-02", "7"),
                        ("2026-11-02T00:00:00+01:00", "7"), (self.soon(), "-1")]
        for start, days in bad_rollouts:
            with self.subTest(start=start, days=days):
                self.assertIn("rollout", self.fails("publish", "--windows", "0.3.4",
                                                    "--rollout-start", start, "--rollout-days", days))
        narrow = EXAMPLE["windows"]["config"]["second_screen_rules"][0]
        broad = {"gpus": ["nvidia"], "gpu_driver": {"gpu": "nvidia", "min": "32.0", "max": "32.4294967295"},
                 "windows": {"min": "26100.0", "max": "26100.99999"}}
        bad_configs = [
            ("--windows-config", {"schema": 1, "attach_timeout_s": 20}),
            ("--windows-config", {"schema": 2}),
            ("--windows-config", {"schema": 1, "second_screen_rules": [broad]}),
            ("--windows-config", {"schema": 1, "second_screen_rules": [narrow] * 9}),
            ("--windows-config", {"schema": 1, "second_screen_rules": [{**narrow, "cpu": "arm64"}]}),
            ("--windows-config", {"schema": 1, "kill_switch": True}),
            ("--ipad-config", {"schema": 1, "first_frame_timeout_s": 8}),
            ("--ipad-config", {"schema": 1, "first_frame_timeout_s": 21}),
            ("--ipad-config", ["schema", 1]),
        ]
        for index, (flag, config) in enumerate(bad_configs):
            with self.subTest(index=index):
                self.fails("publish", flag, self.config_file(f"bad{index}.json", config))
        self.fails("publish", "--windows-config", str(self.tmp / "missing.json"))
        for note in ("two\u2028lines", "zero\u200bwidth", "reversed \u202etext", "\ufeffhidden"):
            with self.subTest(note=note):
                self.assertIn("--note", self.fails("publish", "--windows", "0.3.4", "--note", note))

    def test_rollout_arguments_go_together_on_a_release(self):
        for argv in (["publish", "--windows", "0.3.3", "--rollout-start", self.soon()],
                     ["publish", "--windows", "0.3.3", "--rollout-days", "3"],
                     ["rollback", "--windows", "0.3.2", "--rollout-start", self.soon(), "--rollout-days", "3"],
                     ["publish", "--windows-config", "none", "--rollout-start", self.soon(), "--rollout-days", "3"]):
            with self.subTest(argv):
                self.fails(*argv)

    def test_the_example_fields_are_fields_the_apps_accept(self):
        latest = manifest("0.3.3", "0.3.3", 364)
        for platform in ("windows", "ipad"):
            latest[platform].update({key: EXAMPLE[platform][key] for key in ("note", "config")})
            latest[platform]["rollout"] = {"start": self.soon(), "days": 3}
        latest_json.safe_manifest(latest)
        latest["ipad"]["config"]["attach_timeout_s"] = 45
        with self.assertRaises(latest_json.ManifestError):
            latest_json.safe_manifest(latest)


if __name__ == "__main__":
    unittest.main()
