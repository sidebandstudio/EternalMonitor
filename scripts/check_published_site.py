#!/usr/bin/env python3
"""Check that eternalmonitor.dev serves what an installer from this tag links to.

The installed QUICKSTART.txt links to known-issues.html, and both apps read
latest.json. GitHub Pages publishes them from main's docs/ folder, so a tag
built before that deploy would ship links that 404. The release workflow runs
this before building the installer:

    check_published_site.py --tag v0.3.2 --strict

Each URL gets tries with growing waits, about ten minutes in all, for a Pages
deploy still in progress. If that runs out, rerun the workflow once Pages
has finished.
latest.json may not name a version newer than the tag. It normally names the
previous release here: it carries this installer's SHA-256, so it is updated
only after this workflow publishes and Apple approves the matching iPad build.
--strict (final tags) exits 1 on a problem; otherwise each problem is printed
as a GitHub Actions warning and the step passes.
"""
import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request

SITE = "https://eternalmonitor.dev"
WAITS = (0, 15, 30, 60, 90, 120, 150, 150)  # seconds before each try: 615 s in all


def fetch(url):
    """(HTTP status, body). A network error counts as status 0."""
    request = urllib.request.Request(url, headers={"Cache-Control": "no-cache"})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, b""
    except (urllib.error.URLError, TimeoutError, OSError):
        return 0, b""


def fetch_ok(url, get=fetch, sleep=time.sleep, waits=WAITS):
    """The body once the URL answers 200, or None after the last try."""
    status = 0
    for attempt, wait in enumerate(waits, 1):
        sleep(wait)
        status, body = get(url)
        if status == 200:
            return body
        print(f"{url}: HTTP {status or 'no answer'} (try {attempt} of {len(waits)})")
    return None


def version(text):
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:-.+)?", text)
    return tuple(map(int, match.groups())) if match else None


def problems(tag, site=SITE, get=fetch, sleep=time.sleep, waits=WAITS):
    found = []
    # A query string keeps a cached copy from before the deploy out of the way.
    bust = f"?release={tag}"
    if fetch_ok(f"{site}/known-issues.html{bust}", get, sleep, waits) is None:
        found.append(f"{site}/known-issues.html is not published; the installed Quick Start links to it")
    body = fetch_ok(f"{site}/latest.json{bust}", get, sleep, waits)
    if body is None:
        found.append(f"{site}/latest.json is not published; both apps read it for updates")
        return found
    try:
        published = json.loads(body)["windows"]["version"]
    except (ValueError, KeyError, TypeError):
        return found + [f"{site}/latest.json has no windows.version"]
    have, want = version(published), version(tag)
    if have is None:
        found.append(f"{site}/latest.json names an invalid version {published!r}")
    elif want is None:
        print(f"{tag} is not a version tag; skipping the latest.json version check")
    elif have > want:
        found.append(f"{site}/latest.json publishes {published}, newer than {tag}")
    else:
        print(f"latest.json publishes {published}; {tag} replaces it after TestFlight approval")
    return found


def main(argv=None, get=fetch, sleep=time.sleep):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tag", required=True, help="the tag or ref name being built")
    parser.add_argument("--strict", action="store_true", help="fail instead of warning")
    args = parser.parse_args(argv)
    found = problems(args.tag, get=get, sleep=sleep)
    for problem in found:
        print(f"::{'error' if args.strict else 'warning'}::{problem}")
    if found and args.strict:
        return 1
    if not found:
        print("eternalmonitor.dev serves known-issues.html and latest.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
