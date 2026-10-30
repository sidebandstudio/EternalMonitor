"""Beta channel builds must sort below the release they lead to and never touch other releases."""
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import nightly  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
SHA = "62b9d9d" + "0" * 33
HASH = "ab" * 32


class VersionTest(unittest.TestCase):
    def test_the_host_version_wins_when_it_is_ahead_of_stable(self):
        self.assertEqual(nightly.next_version("0.3.4", "0.3.3"), "0.3.4")
        self.assertEqual(nightly.next_version("0.4.0", "0.3.3"), "0.4.0")
        self.assertEqual(nightly.next_version("1.0.0-rc.1", "0.9.9"), "1.0.0")

    def test_otherwise_the_next_patch_after_stable(self):
        self.assertEqual(nightly.next_version("0.3.3", "0.3.3"), "0.3.4")
        self.assertEqual(nightly.next_version("0.3.2", "0.3.3"), "0.3.4")
        self.assertEqual(nightly.next_version("0.3.3+local", "0.3.9"), "0.3.10")

    def test_nightly_version_is_a_semver_pre_release(self):
        version = nightly.nightly_version("0.3.4", "20261029", 12)
        self.assertEqual(version, "0.3.4-beta.20261029.12")
        # Numeric identifiers, no leading zeros, so semver orders them by date and run.
        self.assertRegex(version, r"^\d+\.\d+\.\d+-beta\.[1-9]\d{7}\.[1-9]\d*$")
        for date, run in (("2026-10-29", 1), ("261029", 1), ("20261029", 0)):
            with self.subTest(date=date, run=run), self.assertRaises(ValueError):
                nightly.nightly_version("0.3.4", date, run)

    def test_dated_betas_sort_above_the_hand_made_beta_and_below_the_release(self):
        def key(version):
            base, _, pre = version.partition("-")
            ids = [(0, int(i), "") if i.isdigit() else (1, 0, i) for i in pre.split(".")] if pre else None
            return (nightly.core(base), ids is None, ids or [])
        ordered = ["0.3.3", "0.3.4-beta.1", "0.3.4-beta.20261030.12", "0.3.4-beta.20261031.13", "0.3.4"]
        self.assertEqual(sorted(reversed(ordered), key=key), ordered)

    def test_titles_read_as_a_dated_beta(self):
        self.assertEqual(nightly.title("0.3.4-beta.20261030.12"), "EternalMonitor 0.3.4 beta (Oct 30, 2026)")
        self.assertEqual(nightly.title("0.4.0-beta.20270105.3"), "EternalMonitor 0.4.0 beta (Jan 5, 2027)")
        with self.assertRaises(ValueError):
            nightly.title("0.3.4-beta.1")

    def test_bad_versions_are_refused(self):
        for bad in ("", "0.3", "v0.3.3", "garbage"):
            with self.subTest(bad), self.assertRaises(ValueError):
                nightly.next_version(bad, "0.3.3")


class TagTest(unittest.TestCase):
    def test_tags_never_start_with_v(self):
        # release.yml and testflight.yml run on v* tags.
        self.assertEqual(nightly.MOVING_TAG, "beta")
        self.assertEqual(nightly.ASSET, "EternalMonitor-Setup-beta.exe")
        tag = nightly.nightly_tag("20261029", SHA)
        self.assertEqual(tag, "beta-20261029-62b9d9d")
        self.assertFalse(tag.startswith("v"))
        self.assertFalse(nightly.MOVING_TAG.startswith("v"))
        self.assertTrue(nightly.DATED_TAG.fullmatch(tag))


class BranchTest(unittest.TestCase):
    def test_the_variable_wins(self):
        self.assertEqual(nightly.pick_branch("main", {"release/0.3.4-candidate": 3}), "main")

    def test_the_newest_candidate_ahead_of_main(self):
        ahead = {"release/0.3.4-candidate": 5, "release/0.3.10-candidate": 2, "release/0.3.9-candidate": 9}
        self.assertEqual(nightly.pick_branch("", ahead), "release/0.3.10-candidate")

    def test_a_merged_candidate_is_skipped(self):
        ahead = {"release/0.3.4-candidate": 5, "release/0.3.5-candidate": 0}
        self.assertEqual(nightly.pick_branch("", ahead), "release/0.3.4-candidate")

    def test_main_when_no_candidate_is_ahead(self):
        self.assertEqual(nightly.pick_branch("", {}), "main")
        self.assertEqual(nightly.pick_branch("", {"release/0.3.4-candidate": 0}), "main")
        self.assertEqual(nightly.pick_branch("", {"release/0.3.4": 4, "release/next-candidate": 4}), "main")


class StampTest(unittest.TestCase):
    def test_stamps_the_repository_files(self):
        toml = (ROOT / "host/Cargo.toml").read_text()
        lock = (ROOT / "Cargo.lock").read_text()
        new_toml, new_lock = nightly.stamp(toml, lock, "0.3.4-beta.20261029.12")
        self.assertEqual(nightly.package_version(new_toml), "0.3.4-beta.20261029.12")
        self.assertIn('name = "eternal-host"\nversion = "0.3.4-beta.20261029.12"\n', new_lock)
        # Only the one line in each file changes; dependency versions stay.
        for old, new in ((toml, new_toml), (lock, new_lock)):
            changed = [(a, b) for a, b in zip(old.splitlines(), new.splitlines()) if a != b]
            self.assertEqual(len(changed), 1, changed)
            self.assertEqual(len(old.splitlines()), len(new.splitlines()))

    def test_windows_line_endings(self):
        toml = (ROOT / "host/Cargo.toml").read_text().replace("\n", "\r\n")
        lock = (ROOT / "Cargo.lock").read_text()
        new_toml, _ = nightly.stamp(toml, lock, "0.3.4-beta.20261029.1")
        self.assertIn('\r\nversion = "0.3.4-beta.20261029.1"\r\n', new_toml)

    def test_the_cli_edits_a_checkout(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = pathlib.Path(tmp)
            (tmp / "host").mkdir()
            (tmp / "host/Cargo.toml").write_text((ROOT / "host/Cargo.toml").read_text())
            (tmp / "Cargo.lock").write_text((ROOT / "Cargo.lock").read_text())
            subprocess.run([sys.executable, str(ROOT / "scripts/nightly.py"), "stamp", "9.8.7-beta.20261029.3"],
                           cwd=tmp, check=True, capture_output=True)
            self.assertEqual(nightly.package_version((tmp / "host/Cargo.toml").read_text()), "9.8.7-beta.20261029.3")
            self.assertIn('name = "eternal-host"\nversion = "9.8.7-beta.20261029.3"', (tmp / "Cargo.lock").read_text())

    def test_missing_versions_fail(self):
        with self.assertRaises(ValueError):
            nightly.stamp('[package]\nname = "x"\n', "", "0.3.4-beta.20261029.1")
        with self.assertRaises(ValueError):
            nightly.stamp('[package]\nversion = "0.3.3"\n', 'name = "other"\nversion = "0.3.3"\n',
                          "0.3.4-beta.20261029.1")


class NotesTest(unittest.TestCase):
    def test_notes_name_the_build_and_the_way_back(self):
        text = nightly.notes("0.3.4-beta.20261029.12", "release/0.3.4-candidate", SHA, "0.3.3", HASH.upper())
        self.assertIn("0.3.4-beta.20261029.12", text)
        self.assertIn("`release/0.3.4-candidate`", text)
        self.assertIn(f"/commit/{SHA})", text)
        self.assertIn(f"/compare/v0.3.3...{SHA})", text)
        self.assertIn("Beta channel", text)
        self.assertIn("For testing", text)
        self.assertIn("go back to Stable", text)
        self.assertNotIn("ightly", text)
        self.assertIn("TestFlight", text)
        self.assertIn(f"```\n{HASH}\n```", text)

    def test_notes_need_the_full_commit_and_hash(self):
        with self.assertRaises(ValueError):
            nightly.notes("v", "b", SHA[:7], "0.3.3", HASH)
        with self.assertRaises(ValueError):
            nightly.notes("v", "b", SHA, "0.3.3", "")


def release(tag, created, prerelease=True):
    return {"tagName": tag, "isPrerelease": prerelease, "createdAt": created}


class RetentionTest(unittest.TestCase):
    def test_keeps_the_newest_seven(self):
        releases = [release(f"beta-202610{day:02d}-abcdef{day % 10}", f"2026-10-{day:02d}T06:30:00Z")
                    for day in range(1, 11)]
        self.assertEqual(nightly.expired(releases),
                         ["beta-20261003-abcdef3", "beta-20261002-abcdef2", "beta-20261001-abcdef1"])
        self.assertEqual(nightly.expired(releases[:7]), [])

    def test_only_dated_nightly_pre_releases_are_ever_selected(self):
        old = "2020-01-01T00:00:00Z"
        others = [
            release("beta", old),                             # the moving download link
            release("windows-0.3.4-beta.1", old),             # the hand-made beta
            release("v0.3.3", old, prerelease=False),
            release("v0.3.4-rc.1", old),
            release("vbeta-20200101-abcdef1", old),
            release("nightly-20200101-abcdef1", old),
            release("beta-20200101-abcdef1", old, prerelease=False),
            release("beta-2020-01-01-abcdef1", old),
            release("beta-20200101-ABCDEF1", old),
            release("beta-20200101-abcdef1-extra", old),
            release("old-beta-20200101-abcdef1", old),
        ]
        self.assertEqual(nightly.expired(others, keep=0), [])

    def test_the_cli_reads_gh_release_list_json(self):
        data = '[{"tagName":"beta-20261001-abcdef1","isPrerelease":true,"createdAt":"2026-10-01T06:30:00Z"},' \
               '{"tagName":"v0.3.3","isPrerelease":false,"createdAt":"2026-10-02T06:30:00Z"}]'
        out = subprocess.run([sys.executable, str(ROOT / "scripts/nightly.py"), "expired", "--keep", "0"],
                             input=data, check=True, capture_output=True, text=True).stdout
        self.assertEqual(out, "beta-20261001-abcdef1\n")


class WorkflowTest(unittest.TestCase):
    """The nightly build must match release.yml and never publish from a dry run or branch."""

    text = (ROOT / ".github/workflows/nightly.yml").read_text()
    release = (ROOT / ".github/workflows/release.yml").read_text()

    @staticmethod
    def pins(text):
        head = text.partition("\njobs:\n")[0]
        return dict(re.findall(r"(?m)^  ([A-Z][A-Z0-9_]*(?:VERSION|SHA256|TAG|ASSET)): \"([^\"]+)\"", head))

    def test_pins_match_release_yml(self):
        ours, theirs = self.pins(self.text), self.pins(self.release)
        self.assertIn("VDD_SHA256", ours)
        for name in theirs.keys() & ours.keys():
            self.assertEqual(ours[name], theirs[name], name)
        self.assertLessEqual(theirs.keys(), ours.keys(), "nightly.yml lacks a pin release.yml has")

    def test_publishing_needs_main_and_no_dry_run(self):
        self.assertIn("github.ref == 'refs/heads/main'", self.text)
        self.assertIn("inputs.dry_run", self.text)
        publish = self.text.partition("\n  publish:\n")[2]
        self.assertIn("if: needs.plan.outputs.publish == 'true'", publish)
        # Only the publishing job can write.
        self.assertEqual(self.text.count("contents: write"), 1)
        self.assertIn("contents: write", publish)

    def test_releases_are_pre_releases_that_never_become_latest(self):
        commands = re.findall(r"gh release (?:create|edit) [^\n]*", self.text)
        self.assertEqual(len(commands), 3)
        for create in commands:
            self.assertIn("--prerelease", create)
            self.assertIn("--latest=false", create)
        self.assertNotRegex(self.text, r"gh release (create|edit|delete) +\"?v")


if __name__ == "__main__":
    unittest.main()
