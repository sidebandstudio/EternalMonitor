"""The growth report reads GitHub with GETs only and adds the numbers up right."""
import contextlib
import datetime
import io
import json
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import growth_report as gr  # noqa: E402

REPO = "sidebandstudio/EternalMonitor"
NOW = datetime.datetime(2026, 10, 31, 12, 0, tzinfo=datetime.timezone.utc)


def star(when, n):
    return {"starred_at": when, "user": {"login": f"user{n}"}}


# Excerpts of real responses from 2026-10-31, with stargazers made up.
STARS = [star("2026-06-12T09:42:33Z", n) for n in range(95)] + [
    star("2026-10-04T12:00:01Z", 95),  # 27 days ago
    star("2026-10-03T11:59:59Z", 96),  # just over 28 days ago
    star("2026-10-24T12:00:00Z", 97),  # exactly 7 days ago
    star("2026-10-25T08:00:00Z", 98),
    star("2026-10-30T23:00:00Z", 99),
    star("2026-10-31T09:00:00Z", 100),
]
RELEASES = [
    # Drafts reach maintainer tokens, with no date and nothing downloaded yet.
    {"tag_name": "v0.3.4", "draft": True, "prerelease": False, "published_at": None,
     "assets": [{"name": "EternalMonitor-Setup.exe", "download_count": 0}]},
    {"tag_name": "beta", "draft": False, "prerelease": True, "published_at": "2026-10-30T20:42:57Z",
     "assets": [{"name": "EternalMonitor-Setup-beta.exe", "download_count": 4}]},
    {"tag_name": "windows-0.3.4-beta.1", "draft": False, "prerelease": True, "published_at": "2026-10-30T02:13:09Z",
     "assets": [{"name": "EternalMonitor-0.3.4-beta.1-Setup.exe", "download_count": 6},
                {"name": "SHA256SUMS.txt", "download_count": 2}]},
    {"tag_name": "v0.3.3", "draft": False, "prerelease": False, "published_at": "2026-10-27T14:43:03Z",
     "assets": [{"name": "EternalMonitor-Setup.exe", "download_count": 470}]},
    {"tag_name": "v0.3.2", "draft": False, "prerelease": False, "published_at": "2026-10-23T16:55:35Z",
     "assets": [{"name": "EternalMonitor-Setup.exe", "download_count": 313}]},
]
RESPONSES = {
    f"repos/{REPO}": {"stargazers_count": 101, "forks_count": 10, "subscribers_count": 0,
                      "watchers_count": 101, "open_issues_count": 11},
    f"repos/{REPO}/stargazers?per_page=100&page=1": STARS[:100],
    f"repos/{REPO}/stargazers?per_page=100&page=2": STARS[100:],
    f"repos/{REPO}/releases?per_page=100&page=1": RELEASES,
    f"repos/{REPO}/traffic/views": {"count": 707, "uniques": 261, "views": [
        {"timestamp": "2026-10-17T00:00:00Z", "count": 62, "uniques": 22}]},
    f"repos/{REPO}/traffic/clones": {"count": 6146, "uniques": 762, "clones": [
        {"timestamp": "2026-10-17T00:00:00Z", "count": 10, "uniques": 6}]},
    f"repos/{REPO}/traffic/popular/referrers": [
        {"referrer": "eternalmonitor.dev", "count": 101, "uniques": 67},
        {"referrer": "github.com", "count": 50, "uniques": 37},
        {"referrer": "Google", "count": 24, "uniques": 18}],
    f"repos/{REPO}/traffic/popular/paths": [
        {"path": "/sidebandstudio/EternalMonitor", "title": "Overview", "count": 288, "uniques": 170},
        {"path": "/sidebandstudio/EternalMonitor/blob/main/SETUP.md", "title": "/blob/main/SETUP.md",
         "count": 71, "uniques": 52}],
}


class Recorded:
    """Answers from RESPONSES; anything else is an error, as gh would give."""

    def __init__(self, responses=RESPONSES, refuse=()):
        self.responses, self.refuse, self.calls = responses, refuse, []

    def __call__(self, path, accept=None):
        self.calls.append((path, accept))
        if any(path.startswith(f"repos/{REPO}/{part}") for part in self.refuse):
            raise gr.GhError("HTTP 403: Must have push access to repository")
        return json.loads(json.dumps(self.responses[path]))


def run(*argv, get=None):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        code = gr.main(list(argv), get=get or Recorded(), now=NOW)
    return code, out.getvalue()


class Report(unittest.TestCase):
    def test_numbers(self):
        get = Recorded()
        report = gr.collect(get, REPO, NOW)
        self.assertEqual(report["stars"], {"total": 101, "listed": 101, "last_7_days": 4, "last_28_days": 5})
        self.assertEqual((report["forks"], report["watchers"], report["open_issues"]), (10, 0, 11))
        downloads = report["downloads"]
        self.assertEqual((downloads["stable"], downloads["prerelease"]), (783, 12))
        self.assertEqual([r["downloads"] for r in downloads["releases"]], [4, 8, 470, 313])
        traffic = report["traffic"]
        self.assertEqual((traffic["views"], traffic["unique_visitors"]), (707, 261))
        self.assertEqual((traffic["clones"], traffic["unique_cloners"]), (6146, 762))
        self.assertEqual(traffic["website_clicks"], 101)
        self.assertEqual(traffic["paths"][1]["path"], "/sidebandstudio/EternalMonitor/blob/main/SETUP.md")
        # Stargazers page through with the header that adds starred_at.
        star_calls = [c for c in get.calls if "/stargazers" in c[0]]
        self.assertEqual(len(star_calls), 2)
        self.assertTrue(all(accept == gr.STAR_JSON for _, accept in star_calls))

    def test_text(self):
        code, out = run()
        self.assertEqual(code, 0)
        self.assertIn("Stars      101  (+4 in 7 days, +5 in 28 days)", out)
        self.assertIn("Downloads, all time: 783 stable, 12 pre-release", out)
        self.assertRegex(out, r"v0\.3\.3 +stable +2026-10-27 +470")
        self.assertRegex(out, r"SHA256SUMS\.txt +2")
        self.assertIn("Views    707  (261 unique visitors)", out)
        self.assertIn("Website-to-GitHub views (eternalmonitor.dev referrer): 101", out)
        self.assertRegex(out, r"Google +24 +18")

    def test_json(self):
        code, out = run("--json")
        self.assertEqual(code, 0)
        report = json.loads(out)
        self.assertEqual(report["generated_at"], "2026-10-31T12:00:00Z")
        self.assertEqual(report["stars"]["last_28_days"], 5)

    def test_drafts_are_left_out(self):
        report = gr.collect(Recorded(), REPO, NOW)
        self.assertEqual([r["tag"] for r in report["downloads"]["releases"]],
                         ["beta", "windows-0.3.4-beta.1", "v0.3.3", "v0.3.2"])
        code, out = run()
        self.assertEqual(code, 0)
        self.assertNotRegex(out, r"(?m)^  v0\.3\.4 ")

    def test_website_below_the_top_ten_is_unknown(self):
        others = [{"referrer": f"site{n}.example", "count": 50 - n, "uniques": 10} for n in range(10)]
        responses = dict(RESPONSES)
        responses[f"repos/{REPO}/traffic/popular/referrers"] = others
        report = gr.collect(Recorded(responses), REPO, NOW)
        self.assertIsNone(report["traffic"]["website_clicks"])
        code, out = run("--json", get=Recorded(responses))
        self.assertIsNone(json.loads(out)["traffic"]["website_clicks"])
        code, out = run(get=Recorded(responses))
        self.assertIn("Website-to-GitHub views (eternalmonitor.dev referrer): not listed in the top 10", out)
        # With fewer than ten listed, a missing website had no views at all.
        responses[f"repos/{REPO}/traffic/popular/referrers"] = others[:3]
        self.assertEqual(gr.collect(Recorded(responses), REPO, NOW)["traffic"]["website_clicks"], 0)

    def test_without_push_access_the_rest_still_prints(self):
        code, out = run(get=Recorded(refuse=("traffic",)))
        self.assertEqual(code, 0)
        self.assertIn("Traffic: not available (HTTP 403", out)
        self.assertIn("Stars      101", out)

    def test_a_failed_call_elsewhere_exits_1(self):
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code, _ = run(get=Recorded(refuse=("releases",)))
        self.assertEqual(code, 1)
        self.assertIn("403", err.getvalue())

    def test_gh_calls_are_gets(self):
        commands = []

        class Done:
            returncode, stdout, stderr = 0, "[]", ""

        def fake_run(command, **_):
            commands.append(command)
            return Done()

        real = gr.subprocess.run
        gr.subprocess.run = fake_run
        try:
            gr.gh_get("repos/x/y/stargazers?per_page=100&page=1", gr.STAR_JSON)
            gr.gh_get("repos/x/y")
        finally:
            gr.subprocess.run = real
        self.assertEqual(commands[0], ["gh", "api", "-H", f"Accept: {gr.STAR_JSON}", "-X", "GET",
                                       "repos/x/y/stargazers?per_page=100&page=1"])
        self.assertEqual(commands[1], ["gh", "api", "-X", "GET", "repos/x/y"])


if __name__ == "__main__":
    unittest.main()
