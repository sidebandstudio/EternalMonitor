"""check-versions.sh must reject a publish that the download page would not show."""
import copy
import json
import pathlib
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
FILES = [
    "scripts/check-versions.sh",
    "host/Cargo.toml",
    "proto/Cargo.toml",
    "ios/project.yml",
    "docs/download.html",
    "docs/latest.json",
    "docs/script.js",
    "scripts/manifest_fields.py",
]
OLD_SCRIPT_FETCH = (
    "  fetch('https://api.github.com/repos/sidebandstudio/EternalMonitor/releases/tags/v0.3.1', {\n"
    "    headers: { 'Accept': 'application/vnd.github.v3+json' }\n"
    "  });\n"
)


class CheckVersionsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        for name in FILES:
            target = self.tmp / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / name, target)
        self.version = re.search(r'MARKETING_VERSION:\s*"([^"]+)"', (ROOT / "ios/project.yml").read_text())[1]

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def run_check(self):
        return subprocess.run(["bash", str(self.tmp / "scripts/check-versions.sh")],
                              capture_output=True, text=True)

    def publish_next_release(self):
        """What the release procedure changes: versions, the page and latest.json."""
        major, minor, patch = map(int, self.version.split("."))
        new = f"{major}.{minor}.{patch + 1}"
        # The tree runs ahead of the published release until it is published.
        published = json.loads((self.tmp / "docs/latest.json").read_text())["windows"]["version"]
        for name in FILES[1:6]:
            path = self.tmp / name
            old = published if name.startswith("docs/") else self.version
            path.write_text(path.read_text().replace(old, new))
        return new

    def test_current_tree_passes(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_next_release_passes_without_touching_the_script(self):
        self.publish_next_release()
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def set_ipad_version(self, version):
        """Point latest.json and the download page at another iPad version."""
        latest = self.tmp / "docs/latest.json"
        old = re.search(r'"ipad": \{\s*"version": "([^"]+)"', latest.read_text())[1]
        latest.write_text(latest.read_text().replace(
            f'"version": "{old}",\n    "build"', f'"version": "{version}",\n    "build"'))
        page = self.tmp / "docs/download.html"
        page.write_text(page.read_text().replace(f"iPad v{old}", f"iPad v{version}")
                        .replace(f"data-ipad-version>v{old}", f"data-ipad-version>v{version}"))

    def test_windows_ahead_of_the_ipad_passes(self):
        new = self.publish_next_release()
        self.set_ipad_version(self.version)
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"Windows {new}, iPad {self.version}", result.stdout)

    def test_ipad_ahead_of_windows_fails(self):
        major, minor, patch = map(int, self.version.split("."))
        self.set_ipad_version(f"{major}.{minor + 1}.0")
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("newer than Windows", result.stdout + result.stderr)

    def test_page_showing_another_ipad_version_fails(self):
        latest = self.tmp / "docs/latest.json"
        text = latest.read_text()
        old = re.search(r'"ipad": \{\s*"version": "([^"]+)"', text)[1]
        latest.write_text(text.replace(f'"version": "{old}",\n    "build"', '"version": "0.0.1",\n    "build"'))
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("docs/download.html says", result.stdout + result.stderr)

    def test_script_pinned_to_an_old_tag_fails(self):
        self.publish_next_release()
        script = self.tmp / "docs/script.js"
        script.write_text(script.read_text().replace("})();", OLD_SCRIPT_FETCH + "})();"))
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("pins a release", result.stdout + result.stderr)

    def test_script_that_ignores_latest_json_fails(self):
        script = self.tmp / "docs/script.js"
        script.write_text(script.read_text().replace("fetch('latest.json'", "fetch('other.json'"))
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0, result.stdout)


    # The optional fields both apps read (docs/remote-config.md), taken from
    # the example manifest the Rust and Swift tests also read.
    EXAMPLE = json.loads((ROOT / "proto/testdata/latest_remote_config.json").read_text())

    def with_fields(self, platform, change):
        latest = self.tmp / "docs/latest.json"
        manifest = json.loads(latest.read_text())
        change(manifest[platform])
        latest.write_text(json.dumps(manifest, indent=2))
        return self.run_check()

    def example_fields(self, platform):
        def change(entry):
            for key in ("rollout", "config", "note"):
                entry[key] = copy.deepcopy(self.EXAMPLE[platform][key])
        return change

    def test_the_example_fields_pass(self):
        for platform in ("windows", "ipad"):
            result = self.with_fields(platform, self.example_fields(platform))
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_rejected(self, platform, change, message):
        def both(entry):
            self.example_fields(platform)(entry)
            change(entry)
        result = self.with_fields(platform, both)
        self.assertNotEqual(result.returncode, 0, f"{message}: {result.stdout}")
        self.assertIn("FAIL", result.stdout + result.stderr, message)
        self.tearDown()
        self.setUp()

    @staticmethod
    def days_from_now(days):
        import datetime
        when = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=days)
        return when.strftime("%Y-%m-%dT%H:%M:%SZ")

    def test_rollouts_up_to_two_weeks_ahead_pass(self):
        for start in [self.days_from_now(13), self.days_from_now(-400), "2026-11-02T00:00:00Z"]:
            result = self.with_fields("ipad", lambda e: e.update(rollout={"start": start, "days": 14}))
            self.assertEqual(result.returncode, 0, start + result.stdout + result.stderr)
            self.tearDown()
            self.setUp()

    def test_bad_rollouts_are_refused(self):
        bad = [
            "2026-11-02",
            {"start": "2026-11-02T00:00:00Z"},
            {"start": "2026-11-02T00:00:00+01:00", "days": 7},
            {"start": "2026-02-30T00:00:00Z", "days": 7},
            {"start": "2026-11-02T00:00:00Z", "days": 15},
            {"start": "2026-11-02T00:00:00Z", "days": -1},
            {"start": "2026-11-02T00:00:00Z", "days": 2.5},
            {"start": "2026-11-02T00:00:00Z", "days": True},
            {"start": "2026-11-02T00:00:00Z", "days": 7, "percent": 10},
            {"start": self.days_from_now(15), "days": 7},
            {"start": "2099-01-01T00:00:00Z", "days": 7},
        ]
        for platform in ("windows", "ipad"):
            for rollout in bad:
                self.assert_rejected(platform, lambda e, r=rollout: e.update(rollout=r), f"{platform} {rollout}")

    def test_bad_config_values_are_refused(self):
        cases = [
            ("windows", lambda c: c.update(schema=2)),
            ("windows", lambda c: c.update(attach_timeout_s=29)),
            ("windows", lambda c: c.update(attach_timeout_s=20)),
            ("windows", lambda c: c.update(attach_timeout_s=61)),
            ("windows", lambda c: c.update(attach_timeout_s="45")),
            ("windows", lambda c: c.update(attach_seconds=45)),
            ("windows", lambda c: c.update(second_screen_rules=[c["second_screen_rules"][0]] * 9)),
            ("windows", lambda c: c["second_screen_rules"].append({"gpus": ["intel", "nvidia"]})),
            ("windows", lambda c: c["second_screen_rules"].append(
                {"gpus": ["intel"], "windows": {"min": "22000.0", "max": "26100.0"}})),
            ("windows", lambda c: c["second_screen_rules"].append(
                {"gpus": ["intel"], "windows": {"min": "26100.9", "max": "26100.1"}})),
            ("windows", lambda c: c["second_screen_rules"].append(
                {"gpus": ["Intel"], "windows": {"min": "26100.0", "max": "26100.9"}})),
            ("windows", lambda c: c["second_screen_rules"].append(
                {"gpus": ["intel"], "gpu_driver": {"gpu": "nvidia", "min": "32.0", "max": "32.1"}})),
            ("windows", lambda c: c["second_screen_rules"].append(
                {"gpus": ["intel"], "windows": {"min": "26100.0", "max": "26100.9"}, "cpu": "arm"})),
            ("ipad", lambda c: c.update(first_frame_timeout_s=9)),
            ("ipad", lambda c: c.update(first_frame_timeout_s=8)),
            ("ipad", lambda c: c.update(first_frame_timeout_s=21)),
            ("ipad", lambda c: c.update(attach_timeout_s=45)),
            ("ipad", lambda c: c.update(schema="1")),
        ]
        for index, (platform, change) in enumerate(cases):
            self.assert_rejected(platform, lambda e, f=change: f(e["config"]), f"case {index}")

    NARROW = {
        "gpus": ["intel", "nvidia"],
        "gpu_driver": {"gpu": "nvidia", "min": "32.0.15.6000", "max": "32.0.15.6099"},
        "display_driver": {"min": "25.7.0", "max": "25.7.99"},
        "windows": {"min": "26100.0", "max": "26100.2999"},
    }

    def rule_check(self, change):
        rule = copy.deepcopy(self.NARROW)
        change(rule)
        result = self.with_fields("windows", lambda e: e.update(
            config={"schema": 1, "second_screen_rules": [rule]}))
        self.tearDown()
        self.setUp()
        return result

    def test_narrow_rules_pass(self):
        exact = {"gpu_driver": {"gpu": "nvidia", "min": "32.0.15.6094", "max": "32.0.15.6094"},
                 "display_driver": {"min": "25.7.23", "max": "25.7.23"},
                 "windows": {"min": "26100.2894", "max": "26100.2894"}}
        for change in [lambda r: None, lambda r: r.update(exact), lambda r: r.pop("windows"),
                       lambda r: r.pop("display_driver")]:
            result = self.rule_check(change)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_broad_rules_are_refused(self):
        def replace(rule):
            rule.clear()
            rule.update({"gpus": ["nvidia"],
                         "gpu_driver": {"gpu": "nvidia", "min": "32.0", "max": "32.4294967295"},
                         "windows": {"min": "26100.0", "max": "26100.99999"}})
        def only_windows(rule):
            rule.pop("gpu_driver")
            rule.pop("display_driver")
            rule["windows"] = {"min": "26100.0", "max": "26100.99999"}
        cases = [
            replace,  # every NVIDIA-only PC
            only_windows,  # every PC on 24H2 with these cards
            lambda r: r.pop("gpu_driver"),
            lambda r: (r.pop("display_driver"), r.pop("windows")),
            lambda r: r["gpu_driver"].update(max="32.0.15.6100"),
            lambda r: r["gpu_driver"].update(min="32.0.15.0", max="32.0.15.9999"),
            lambda r: r["gpu_driver"].update(max="32.0.16.6000"),
            lambda r: r["gpu_driver"].update(min="32.0.15", max="32.0.15"),
            lambda r: r["gpu_driver"].update(gpu="amd"),
            lambda r: r.update(display_driver={"min": "25.7.0", "max": "25.8.0"}),
            lambda r: r.update(display_driver={"min": "25", "max": "25"}),
            lambda r: r.update(display_driver={"min": "25.7", "max": "25.7.99"}),
            lambda r: r.update(windows={"min": "22631.0", "max": "26100.9999"}),
            lambda r: r.update(windows={"min": "26100", "max": "26100"}),
            lambda r: r.update(windows={"min": "26100.9", "max": "26100.1"}),
            lambda r: r.update(gpus=["Intel", "nvidia"]),
            lambda r: r.update(cpu="arm64"),
        ]
        for index, change in enumerate(cases):
            result = self.rule_check(change)
            self.assertNotEqual(result.returncode, 0, f"case {index}: {result.stdout}")
            self.assertIn("FAIL", result.stdout + result.stderr, f"case {index}")

    def test_bad_notes_and_unknown_keys_are_refused(self):
        for platform in ("windows", "ipad"):
            for change in [
                lambda e: e.update(note="x" * 201),
                lambda e: e.update(note="Two\nlines"),
                lambda e: e.update(note="Two\u2028lines"),
                lambda e: e.update(note="Two\u2029paragraphs"),
                lambda e: e.update(note="Zero\u200bwidth"),
                lambda e: e.update(note="Reversed \u202etext"),
                lambda e: e.update(note="\ufeffFixes audio."),
                lambda e: e.update(note=""),
                lambda e: e.update(note=7),
                lambda e: e.update(recommended="yes"),
                lambda e: e.update(rolout={"start": "2026-11-02T00:00:00Z", "days": 7}),
            ]:
                self.assert_rejected(platform, change, platform)


if __name__ == "__main__":
    unittest.main()
