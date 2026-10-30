"""Beta channel builds must sort below the release they lead to and never touch other releases."""
import copy
import hashlib
import itertools
import os
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


class FakeGitHub:
    """GitHub's releases in memory, with the methods of nightly.GitHub. Assets are
    served by name, as releases/download serves them. Calls whose index (counted
    from 0 in each run) is in fail raise instead of acting; writes whose index is
    in after act, then raise, as when GitHub applies a change but the request
    reports a connection failure."""

    repo = nightly.REPO

    def __init__(self):
        self.releases, self.tags = {}, {}
        self.ids = itertools.count(1)
        self.fail, self.after, self.calls = set(), set(), []
        self.corrupt = False  # the fixed link serves the wrong bytes
        self.served = []      # after each write, whether the fixed link serves a file

    def call(self, name, *args):
        index = len(self.calls)
        self.calls.append((name, *args))
        if index in self.fail:
            raise subprocess.CalledProcessError(1, ["gh", name])
        return index

    def wrote(self, index):
        beta = self.releases.get(nightly.MOVING_TAG)
        if beta is not None:
            self.served.append(any(a["name"] == nightly.ASSET for a in beta["assets"]))
        if index in self.after:
            raise subprocess.CalledProcessError(1, ["gh", self.calls[index][0]], stderr="connection reset")

    def find(self, asset_id):
        for release in self.releases.values():
            for asset in release["assets"]:
                if asset["id"] == asset_id:
                    return release, asset
        raise AssertionError(f"no asset {asset_id}")

    def add(self, release, path, name):
        data = pathlib.Path(path).read_bytes()
        release["assets"] = [a for a in release["assets"] if a["name"] != name]
        release["assets"].append({"id": next(self.ids), "name": name, "size": len(data), "data": data})

    def release(self, tag):
        self.call("release", tag)
        release = self.releases.get(tag)
        return None if release is None else {
            "body": release["body"],
            "assets": [{k: a[k] for k in ("id", "name", "size")} for a in release["assets"]]}

    def create(self, tag, sha, title, notes, path=None):
        index = self.call("create", tag)
        assert tag not in self.releases
        self.tags.setdefault(tag, sha)
        self.releases[tag] = {"title": title, "body": notes, "assets": []}
        if path:
            self.add(self.releases[tag], path, os.path.basename(path))
        self.wrote(index)

    def edit(self, tag, title, notes):
        index = self.call("edit", tag)
        self.releases[tag].update(title=title, body=notes)
        self.wrote(index)

    def upload(self, tag, path, name):
        index = self.call("upload", tag, name)
        self.add(self.releases[tag], path, name)
        self.wrote(index)

    def rename(self, asset_id, name):
        index = self.call("rename", name)
        release, asset = self.find(asset_id)
        if any(a["name"] == name for a in release["assets"]):
            raise subprocess.CalledProcessError(1, ["gh", "api"], stderr="HTTP 422 already_exists")
        asset["name"] = name
        self.wrote(index)

    def delete(self, asset_id):
        index = self.call("delete", asset_id)
        release, asset = self.find(asset_id)
        release["assets"].remove(asset)
        self.wrote(index)

    def move_tag(self, tag, sha):
        index = self.call("move_tag", tag, sha)
        self.tags[tag] = sha
        self.wrote(index)

    def served_asset(self, url):
        prefix = f"https://github.com/{self.repo}/releases/download/"
        assert url.startswith(prefix), url
        tag, name = url[len(prefix):].split("/")
        release = self.releases.get(tag, {"assets": []})
        return next((a for a in release["assets"] if a["name"] == name), None)

    def head(self, url):
        self.call("head", url)
        asset = self.served_asset(url)
        return (200, asset["size"]) if asset else (404, -1)

    def sha256(self, url):
        self.call("sha256", url)
        data = self.served_asset(url)["data"]
        return hashlib.sha256(data + (b"x" if self.corrupt else b"")).hexdigest()


OLD_SHA, NEW_SHA = "1" * 40, "2" * 40
WRITES = ("create", "edit", "upload", "rename", "delete", "move_tag")


class PublishTest(unittest.TestCase):
    """publish() checks each step, and the next run finishes one that stopped."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = pathlib.Path(tmp.name)

    def installer(self, data):
        path = pathlib.Path(tempfile.mkdtemp(dir=self.tmp)) / nightly.ASSET
        path.write_bytes(data)
        return str(path)

    def publish(self, github, sha, data, fail=(), after=()):
        github.fail, github.after, github.calls = set(fail), set(after), []
        date = "20261030" if sha == OLD_SHA else "20261031"
        version = f"0.3.4-beta.{date}.1"
        nightly.publish(github, nightly.nightly_tag(date, sha), sha, nightly.title(version),
                        f"Version {version}\n", self.installer(data), sleep=lambda _: None)

    def published(self):
        """A repository where the old commit's build is the newest Beta."""
        github = FakeGitHub()
        self.publish(github, OLD_SHA, b"old build")
        github.served = []
        return github

    def assert_done(self, github, sha, data):
        tag = nightly.nightly_tag("20261031" if sha == NEW_SHA else "20261030", sha)
        self.assertEqual([(a["name"], a["data"]) for a in github.releases[tag]["assets"]],
                         [(nightly.ASSET, data)])
        beta = github.releases[nightly.MOVING_TAG]
        self.assertEqual([(a["name"], a["data"]) for a in beta["assets"]], [(nightly.ASSET, data)])
        self.assertEqual(github.tags[nightly.MOVING_TAG], sha)
        self.assertEqual(nightly.published_commit(beta["body"]), sha)
        self.assertTrue(beta["body"].startswith(f"Version 0.3.4-beta."))
        self.assertTrue(beta["body"].endswith(nightly.marker(sha) + "\n"))

    def assert_link_kept(self, github):
        """The fixed link is never missing across two writes in a row."""
        gaps = [i for i in range(1, len(github.served)) if not github.served[i - 1] and not github.served[i]]
        self.assertEqual(gaps, [], github.served)

    def test_the_first_build_creates_both_releases(self):
        github = FakeGitHub()
        self.publish(github, OLD_SHA, b"old build")
        self.assert_done(github, OLD_SHA, b"old build")

    def test_the_order_of_an_update(self):
        github = self.published()
        self.publish(github, NEW_SHA, b"new build")
        self.assert_done(github, NEW_SHA, b"new build")
        self.assert_link_kept(github)
        writes = [c for c in github.calls if c[0] not in ("release", "head", "sha256")]
        dated = nightly.download_url(nightly.nightly_tag("20261031", NEW_SHA))
        fixed = nightly.download_url(nightly.MOVING_TAG)
        self.assertEqual(writes[0], ("create", "beta-20261031-2222222"))
        # The dated build answers anonymously before anything touches the beta release.
        self.assertEqual(github.calls[github.calls.index(writes[0]) + 1], ("head", dated))
        self.assertEqual(writes[1:], [
            ("upload", "beta", nightly.NEXT),
            ("rename", nightly.OLD),
            ("rename", nightly.ASSET),
            ("delete", writes[4][1]),
            ("move_tag", "beta", NEW_SHA),
            ("edit", "beta"),
        ])
        # The swap is two calls in a row; the fixed link is checked, bytes and all,
        # before the old build goes.
        swap = github.calls.index(("rename", nightly.ASSET))
        self.assertEqual(github.calls[swap - 1], ("rename", nightly.OLD))
        self.assertEqual(github.calls[swap + 1:swap + 3], [("head", fixed), ("sha256", fixed)])

    def link(self, github):
        """The bytes the fixed link serves, or None."""
        asset = github.served_asset(nightly.download_url(nightly.MOVING_TAG))
        return asset and asset["data"]

    def fault(self, github, sha, data, before, fault):
        """Publish with fault ({mode: call indices}) and check the fixed link
        serves a whole build (one of before) once publish fails, or at the latest
        once the next run's reconcile() has run; then the next run finishes.
        Returns "on failure" or "after reconcile"."""
        with self.assertRaises((subprocess.CalledProcessError, nightly.PublishError)):
            self.publish(github, sha, data, **fault)
        when = "on failure"
        if self.link(github) not in before:
            github.fail, github.after, github.calls = set(), set(), []
            nightly.reconcile(github, sleep=lambda _: None)
            when = "after reconcile"
        self.assertIn(self.link(github), before, github.calls)
        beta = github.releases.get(nightly.MOVING_TAG, {"body": ""})
        if nightly.published_commit(beta["body"]) != sha:
            # Not marked done, so the next run's plan builds this commit again.
            self.publish(github, sha, data)
        self.assert_done(github, sha, data)
        return when

    def published_with_stale(self):
        """published(), with an OLD a stopped run left behind."""
        github = self.published()
        github.add(github.releases[nightly.MOVING_TAG], self.installer(b"older build"), nightly.OLD)
        return github

    def matrix(self, mode, first=False, stale=False):
        """{(index, call): when the link serves a build} for one fault at each call
        of a clean run: in fail mode at every call, in after mode at every write."""
        repository = FakeGitHub if first else self.published_with_stale if stale else self.published
        if first:
            sha, data, before = OLD_SHA, b"old build", (None, b"old build")
        else:
            sha, data, before = NEW_SHA, b"new build", (b"old build", b"new build")
        github = repository()
        self.publish(github, sha, data)
        steps = [(i, c) for i, c in enumerate(github.calls) if mode == "fail" or c[0] in WRITES]
        result = {}
        for index, call in steps:
            with self.subTest(mode=mode, step=index, call=call):
                github = repository()
                result[index, call] = self.fault(github, sha, data, before, {mode: {index}})
                if not first:  # before the first build the link has nothing to keep
                    self.assert_link_kept(github)
        return result

    def test_a_failure_at_any_step_leaves_the_link_working(self):
        for mode in ("fail", "after"):
            for stale in (False, True):
                result = self.matrix(mode, stale=stale)
                self.assertEqual(set(result.values()), {"on failure"}, result)
                # The stale OLD's delete is a step too, beside the old build's.
                self.assertEqual([call[0] for _, call in result].count("delete"), 2 if stale else 1)

    def test_a_change_applied_then_reported_failed_is_recovered(self):
        # Sol's case: the old build is renamed away, then the request fails.
        github = self.published()
        self.publish(github, NEW_SHA, b"new build")
        step = github.calls.index(("rename", nightly.OLD))
        github = self.published()
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish(github, NEW_SHA, b"new build", after={step})
        # The new build checked out, so it took the fixed name.
        self.assertEqual(self.link(github), b"new build")
        self.assertEqual(sorted(a["name"] for a in github.releases[nightly.MOVING_TAG]["assets"]),
                         [nightly.ASSET, nightly.OLD])
        self.assertEqual(nightly.published_commit(github.releases[nightly.MOVING_TAG]["body"]), OLD_SHA)

    def test_a_failed_recovery_is_put_right_by_the_next_runs_reconcile(self):
        # A second fault in reconcile()'s own calls, after each first one.
        github = self.published()
        self.publish(github, NEW_SHA, b"new build")
        firsts = [i for i, c in enumerate(github.calls) if c[0] in WRITES]
        seen = set()
        for first in firsts:
            for mode in ("fail", "after"):
                github = self.published()
                with self.assertRaises(subprocess.CalledProcessError):
                    self.publish(github, NEW_SHA, b"new build", after={first})
                recovery = [i for i, c in enumerate(github.calls) if i > first and c[0] in WRITES
                            and c[0] not in ("move_tag", "edit")]
                for second in recovery:
                    with self.subTest(first=first, second=second, mode=mode):
                        github = self.published()
                        fault = {"after": {first, second}} if mode == "after" else {"after": {first}, "fail": {second}}
                        seen.add(self.fault(github, NEW_SHA, b"new build", (b"old build", b"new build"), fault))
        self.assertEqual(seen, {"on failure", "after reconcile"})

    def test_the_first_build_recovers_too(self):
        for mode in ("fail", "after"):
            self.matrix(mode, first=True)

    def test_a_failed_rollback_is_put_right_first_on_the_next_run(self):
        github = self.published()
        self.publish(github, NEW_SHA, b"new build")
        swap = github.calls.index(("rename", nightly.ASSET))
        github = self.published()
        # The second rename fails, and so does every call after it, reconcile()'s too.
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish(github, NEW_SHA, b"new build", fail=range(swap, swap + 100))
        names = sorted(a["name"] for a in github.releases[nightly.MOVING_TAG]["assets"])
        self.assertEqual(names, [nightly.NEXT, nightly.OLD])
        self.publish(github, NEW_SHA, b"new build")
        writes = [c for c in github.calls if c[0] not in ("release", "head", "sha256")]
        # The old build gets its name back before anything else.
        self.assertEqual(writes[:3], [("rename", nightly.ASSET),
                                      ("edit", "beta-20261031-2222222"),
                                      ("upload", "beta-20261031-2222222", nightly.ASSET)])
        self.assert_done(github, NEW_SHA, b"new build")

    def test_wrong_bytes_at_the_fixed_link_put_the_old_build_back(self):
        github = self.published()
        github.corrupt = True
        with self.assertRaises(nightly.PublishError):
            self.publish(github, NEW_SHA, b"new build")
        beta = github.releases[nightly.MOVING_TAG]
        served = [a for a in beta["assets"] if a["name"] == nightly.ASSET]
        self.assertEqual(served[0]["data"], b"old build")
        self.assertEqual(github.tags[nightly.MOVING_TAG], OLD_SHA)
        self.assertEqual(nightly.published_commit(beta["body"]), OLD_SHA)
        # It retried before giving up, and reconcile() checked again before putting
        # the old build back.
        self.assertEqual(len([c for c in github.calls if c[0] == "sha256"]), 12)
        self.assertEqual(sorted(a["name"] for a in beta["assets"]), [nightly.ASSET])
        github.corrupt = False
        self.publish(github, NEW_SHA, b"new build")
        self.assert_done(github, NEW_SHA, b"new build")

    def test_a_dated_build_that_never_appears_stops_before_the_beta_release(self):
        github = self.published()
        github.head = lambda url: (404, -1)
        with self.assertRaises(nightly.PublishError):
            self.publish(github, NEW_SHA, b"new build")
        beta = github.releases[nightly.MOVING_TAG]
        self.assertEqual([(a["name"], a["data"]) for a in beta["assets"]], [(nightly.ASSET, b"old build")])

    def test_the_marker(self):
        self.assertEqual(nightly.published_commit(f"notes\n\n{nightly.marker(NEW_SHA)}\n"), NEW_SHA)
        for body in (None, "", "Built from `main` at 2222222", f"/commit/{NEW_SHA}"):
            self.assertEqual(nightly.published_commit(body), "")


class GitHubCommandTest(unittest.TestCase):
    """The real gh commands: pre-releases that never become latest, never a v tag."""

    def setUp(self):
        self.commands = []

        def run(command, check=False, capture_output=False, text=False):
            self.commands.append(command)
            return subprocess.CompletedProcess(command, self.code, self.stdout, self.stderr)
        self.code, self.stdout, self.stderr = 0, "{}", ""
        self.github = nightly.GitHub("o/r", run)

    def test_writes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "EternalMonitor-Setup.exe")
            pathlib.Path(path).write_bytes(b"x")
            self.github.create("beta-20261031-2222222", NEW_SHA, "T", "N", path)
            self.github.create("beta", NEW_SHA, "T", "N")
            self.github.edit("beta", "T", "N")
            self.github.upload("beta", path, nightly.NEXT)
        self.github.rename(7, nightly.ASSET)
        self.github.delete(7)
        self.github.move_tag("beta", NEW_SHA)
        releases = [c for c in self.commands if c[1] == "release" and c[2] in ("create", "edit")]
        self.assertEqual(len(releases), 3)
        for command in releases:
            self.assertIn("--prerelease", command)
            self.assertIn("--latest=false", command)
        self.assertTrue(self.commands[3][4].endswith(os.sep + nightly.NEXT))
        self.assertIn("--clobber", self.commands[3])
        self.assertEqual(self.commands[4], ["gh", "api", "-X", "PATCH", "repos/o/r/releases/assets/7",
                                            "-f", f"name={nightly.ASSET}"])
        self.assertEqual(self.commands[6], ["gh", "api", "-X", "PATCH", "repos/o/r/git/refs/tags/beta",
                                            "-f", f"sha={NEW_SHA}", "-F", "force=true"])
        for command in self.commands:
            self.assertFalse(any(arg.startswith(("v", "refs/tags/v", "repos/o/r/git/refs/tags/v"))
                                 for arg in command[1:]), command)

    def test_reading_a_release(self):
        self.stdout = '{"body": "b", "assets": []}'
        self.assertEqual(self.github.release("beta"), {"body": "b", "assets": []})
        self.assertEqual(self.commands[-1], ["gh", "api", "repos/o/r/releases/tags/beta"])
        self.code, self.stderr = 1, "gh: Not Found (HTTP 404)"
        self.assertIsNone(self.github.release("beta"))
        self.stderr = "gh: Bad credentials (HTTP 401)"
        with self.assertRaises(nightly.PublishError):
            self.github.release("beta")


class WorkflowTest(unittest.TestCase):
    """The nightly build must match release.yml and never publish from a dry run or branch."""

    text = (ROOT / ".github/workflows/nightly.yml").read_text()
    installer = (ROOT / ".github/workflows/beta-installer.yml").read_text()
    release = (ROOT / ".github/workflows/release.yml").read_text()

    @staticmethod
    def pins(text):
        head = text.partition("\njobs:\n")[0]
        return dict(re.findall(r"(?m)^  ([A-Z][A-Z0-9_]*(?:VERSION|SHA256|TAG|ASSET)): \"([^\"]+)\"", head))

    def test_pins_match_release_yml(self):
        ours, theirs = self.pins(self.installer), self.pins(self.release)
        self.assertIn("VDD_SHA256", ours)
        for name in theirs.keys() & ours.keys():
            self.assertEqual(ours[name], theirs[name], name)
        self.assertLessEqual(theirs.keys(), ours.keys(), "beta-installer.yml lacks a pin release.yml has")

    def test_publishing_needs_main_and_no_dry_run(self):
        self.assertIn('"$GITHUB_REF" != refs/heads/main ]]; then dry=true', self.text)
        self.assertIn("inputs.dry_run", self.text)
        # Signing needs the release environment, which only main gets.
        self.assertIn("environment: ${{ github.ref == 'refs/heads/main' && 'release' || '' }}", self.installer)
        publish = self.text.partition("\n  publish:\n")[2]
        self.assertIn("if: needs.plan.outputs.publish == 'true'", publish)
        # Only the publishing job can write.
        self.assertEqual(self.text.count("contents: write"), 1)
        self.assertIn("contents: write", publish)

    def test_releases_are_published_by_nightly_py(self):
        # GitHubCommandTest checks its commands are pre-releases that never become latest.
        self.assertNotRegex(self.text, r"gh release (create|edit|upload)|releases/assets|git/refs")
        self.assertIn('python3 scripts/nightly.py publish --tag "$TAG"', self.text)
        self.assertNotRegex(self.text, r"gh release delete +\"?v")


if __name__ == "__main__":
    unittest.main()
