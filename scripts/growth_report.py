#!/usr/bin/env python3
"""Weekly growth numbers for the EternalMonitor GitHub repository.

    growth_report.py                 # plain text
    growth_report.py --json          # the same numbers as JSON

Read-only: every call is a `gh api -X GET`, using your gh login. Traffic needs
push access to the repository; without it those parts say so and the rest of
the report still prints. GitHub keeps traffic for 14 days only, so save a
report before and after a launch to compare them.

The eternalmonitor.dev referrer counts views of GitHub pages reached from the
website, which is the website-to-GitHub click count. GitHub lists only the top
ten referrers, so when ten are listed without the website its count is unknown
(null in --json). Draft releases are left out: they have no downloads or date.
"""
import argparse
import datetime
import json
import subprocess
import sys

REPO = "sidebandstudio/EternalMonitor"
STAR_JSON = "application/vnd.github.star+json"
WEBSITE = "eternalmonitor.dev"
REFERRER_LIMIT = 10  # GitHub returns at most the top ten referrers


class GhError(Exception):
    pass


def gh_get(path, accept=None):
    """The parsed JSON of one GET through the gh CLI."""
    command = ["gh", "api", "-X", "GET", path]
    if accept:
        command[2:2] = ["-H", f"Accept: {accept}"]
    result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8")
    if result.returncode != 0:
        raise GhError(result.stderr.strip() or f"gh api {path} failed")
    return json.loads(result.stdout)


def pages(get, path, accept=None):
    """Every item of a list endpoint, 100 at a time."""
    items, page = [], 1
    while True:
        batch = get(f"{path}?per_page=100&page={page}", accept)
        items += batch
        if len(batch) < 100:
            return items
        page += 1


def parse_time(value):
    return datetime.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)


def stars(get, repo, now):
    starred = [parse_time(s["starred_at"]) for s in pages(get, f"repos/{repo}/stargazers", STAR_JSON)]
    return {
        "listed": len(starred),
        "last_7_days": sum(now - t <= datetime.timedelta(days=7) for t in starred),
        "last_28_days": sum(now - t <= datetime.timedelta(days=28) for t in starred),
    }


def downloads(get, repo):
    releases = []
    for release in pages(get, f"repos/{repo}/releases"):
        if release["draft"]:
            continue
        assets = [{"name": a["name"], "downloads": a["download_count"]} for a in release["assets"]]
        releases.append({
            "tag": release["tag_name"],
            "prerelease": release["prerelease"],
            "published_at": release["published_at"],
            "downloads": sum(a["downloads"] for a in assets),
            "assets": assets,
        })
    return {
        "stable": sum(r["downloads"] for r in releases if not r["prerelease"]),
        "prerelease": sum(r["downloads"] for r in releases if r["prerelease"]),
        "releases": releases,
    }


def traffic(get, repo):
    """Views, clones, referrers and paths, or {"error": ...} without push access."""
    try:
        views = get(f"repos/{repo}/traffic/views")
        clones = get(f"repos/{repo}/traffic/clones")
        referrers = get(f"repos/{repo}/traffic/popular/referrers")
        paths = get(f"repos/{repo}/traffic/popular/paths")
    except GhError as error:
        return {"error": str(error)}
    return {
        "views": views["count"],
        "unique_visitors": views["uniques"],
        "clones": clones["count"],
        "unique_cloners": clones["uniques"],
        "referrers": [{"referrer": r["referrer"], "views": r["count"], "unique": r["uniques"]} for r in referrers],
        "paths": [{"path": p["path"], "title": p["title"], "views": p["count"], "unique": p["uniques"]} for p in paths],
        "website_clicks": website_clicks(referrers),
    }


def website_clicks(referrers):
    """Views referred by the website; None when it may be below GitHub's top ten."""
    for referrer in referrers:
        if referrer["referrer"] == WEBSITE:
            return referrer["count"]
    return None if len(referrers) >= REFERRER_LIMIT else 0


def collect(get, repo, now):
    info = get(f"repos/{repo}")
    return {
        "repo": repo,
        "generated_at": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "stars": {"total": info["stargazers_count"], **stars(get, repo, now)},
        "forks": info["forks_count"],
        "watchers": info["subscribers_count"],
        "open_issues": info["open_issues_count"],
        "downloads": downloads(get, repo),
        "traffic": traffic(get, repo),
    }


def text(report):
    s, d, t = report["stars"], report["downloads"], report["traffic"]
    lines = [
        f"{report['repo']} growth report, {report['generated_at'].replace('T', ' ').replace('Z', ' UTC')}",
        "",
        f"Stars      {s['total']}  (+{s['last_7_days']} in 7 days, +{s['last_28_days']} in 28 days)",
        f"Forks      {report['forks']}",
        f"Watchers   {report['watchers']}",
        f"Open issues and PRs  {report['open_issues']}",
        "",
        f"Downloads, all time: {d['stable']} stable, {d['prerelease']} pre-release",
    ]
    for release in d["releases"]:
        kind = "pre-release" if release["prerelease"] else "stable"
        lines.append(f"  {release['tag']:<28} {kind:<11} {release['published_at'][:10]}  {release['downloads']:>6}")
        for asset in release["assets"]:
            lines.append(f"      {asset['name']:<44} {asset['downloads']:>6}")
    lines.append("")
    if "error" in t:
        lines.append(f"Traffic: not available ({t['error']})")
        return "\n".join(lines) + "\n"
    website = t["website_clicks"]
    if website is None:
        website = f"not listed in the top {REFERRER_LIMIT}"
    lines += [
        "Traffic, last 14 days",
        f"  Views    {t['views']}  ({t['unique_visitors']} unique visitors)",
        f"  Clones   {t['clones']}  ({t['unique_cloners']} unique)",
        f"  Website-to-GitHub views ({WEBSITE} referrer): {website}",
        "",
        "Top referrers                  views  unique",
    ]
    lines += [f"  {r['referrer']:<28} {r['views']:>5}  {r['unique']:>6}" for r in t["referrers"]]
    lines += ["", "Popular paths                                                views  unique"]
    lines += [f"  {p['path'][:58]:<58} {p['views']:>5}  {p['unique']:>6}" for p in t["paths"]]
    return "\n".join(lines) + "\n"


def main(argv=None, get=gh_get, now=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", default=REPO)
    parser.add_argument("--json", action="store_true", help="print JSON instead of text")
    args = parser.parse_args(argv)
    now = now or datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)
    try:
        report = collect(get, args.repo, now)
    except GhError as error:
        print(f"growth_report: {error}", file=sys.stderr)
        return 1
    sys.stdout.write(json.dumps(report, indent=2) + "\n" if args.json else text(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
