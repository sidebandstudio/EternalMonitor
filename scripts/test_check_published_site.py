"""The release must not ship an installer whose website links still 404."""
import contextlib
import io
import json
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import check_published_site as site  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
LATEST = json.dumps({"schema": 1, "windows": {"version": "0.3.1"}}).encode()


class Site:
    """A fake eternalmonitor.dev: each path answers its listed statuses in turn."""

    def __init__(self, known_issues, latest):
        self.paths = {"known-issues.html": list(known_issues), "latest.json": list(latest)}
        self.waits = []

    def get(self, url):
        path = url.split("/")[-1].split("?")[0]
        answers = self.paths[path]
        return answers.pop(0) if len(answers) > 1 else answers[0]

    def run(self, tag, strict=False):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = site.main(["--tag", tag] + (["--strict"] if strict else []), get=self.get, sleep=self.waits.append)
        return code, out.getvalue()


def published(latest=LATEST):
    return Site([(200, b"<html>")], [(200, latest)])


class CheckPublishedSiteTest(unittest.TestCase):
    def test_a_published_site_passes_a_final_tag(self):
        code, out = published().run("v0.3.2", strict=True)
        self.assertEqual(code, 0)
        self.assertIn("latest.json publishes 0.3.1; v0.3.2 replaces it", out)

    def test_a_missing_page_fails_a_final_tag_and_warns_otherwise(self):
        missing = Site([(404, b"")], [(200, LATEST)])
        code, out = missing.run("v0.3.2", strict=True)
        self.assertEqual(code, 1)
        self.assertIn("::error::https://eternalmonitor.dev/known-issues.html is not published", out)
        # Every try on the missing page, then latest.json answering at once.
        self.assertEqual(missing.waits, list(site.WAITS) + [0])
        code, out = Site([(404, b"")], [(200, LATEST)]).run("v0.3.2-rc.1")
        self.assertEqual(code, 0)
        self.assertIn("::warning::https://eternalmonitor.dev/known-issues.html is not published", out)

    def test_a_deploy_in_progress_is_retried(self):
        deploying = Site([(404, b""), (0, b""), (200, b"<html>")], [(200, LATEST)])
        code, _ = deploying.run("v0.3.2", strict=True)
        self.assertEqual(code, 0)
        self.assertEqual(deploying.waits[:3], list(site.WAITS[:3]))

    def test_a_slow_pages_deploy_within_ten_minutes_passes(self):
        # Pages finishing 240 s after the tag: the 180 s window failed this.
        clock = []

        def get(url):
            return (200, b"<html>" if url.split("?")[0].endswith(".html") else LATEST) if sum(clock) >= 240 else (404, b"")

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = site.main(["--tag", "v0.3.2", "--strict"], get=get, sleep=clock.append)
        self.assertEqual(code, 0, out.getvalue())
        self.assertGreaterEqual(sum(site.WAITS), 600)
        self.assertEqual(list(site.WAITS), sorted(site.WAITS))

    def test_latest_json_must_not_be_newer_than_the_tag(self):
        newer = json.dumps({"windows": {"version": "0.3.3"}}).encode()
        code, out = published(newer).run("v0.3.2-rc.1", strict=True)
        self.assertEqual(code, 1)
        self.assertIn("publishes 0.3.3, newer than v0.3.2-rc.1", out)
        self.assertEqual(published(json.dumps({"windows": {"version": "0.3.2"}}).encode()).run("v0.3.2", True)[0], 0)

    def test_latest_json_must_name_a_version(self):
        self.assertEqual(published(b"not json").run("v0.3.2", strict=True)[0], 1)
        self.assertEqual(published(json.dumps({"windows": {}}).encode()).run("v0.3.2", strict=True)[0], 1)

    def test_a_dry_run_on_a_branch_skips_the_version(self):
        code, out = published().run("main")
        self.assertEqual(code, 0)
        self.assertIn("main is not a version tag", out)


class ReleaseWorkflowTest(unittest.TestCase):
    def test_the_check_runs_before_the_installer_is_built_or_published(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        check = workflow.index("python scripts/check_published_site.py --tag \"$GITHUB_REF_NAME\"")
        self.assertLess(check, workflow.index("- name: Build the installer"))
        self.assertLess(check, workflow.index("- name: Publish GitHub release"))
        # Strict only for final tags; the publish step treats a "-" as a pre-release too.
        self.assertIn('[[ "$GITHUB_REF" == refs/tags/v* && "$GITHUB_REF_NAME" != *-* ]]', workflow)
        self.assertIn('$isPrerelease = $tag -match "-"', workflow)


if __name__ == "__main__":
    unittest.main()
