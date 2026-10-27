"""check-versions.sh must reject a publish that the download page would not show."""
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
        for name in FILES[1:6]:
            path = self.tmp / name
            path.write_text(path.read_text().replace(self.version, new))
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


if __name__ == "__main__":
    unittest.main()
