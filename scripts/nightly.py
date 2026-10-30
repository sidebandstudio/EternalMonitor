#!/usr/bin/env python3
"""Plan, stamp, describe and prune the nightly Beta channel builds (.github/workflows/nightly.yml).

  nightly.py plan [--branch NAME] [--run N] [--date YYYYMMDD]
      Fetch main and release/* from origin, choose the source branch and print
      key=value lines for $GITHUB_OUTPUT. --branch overrides the choice (the
      NIGHTLY_BRANCH repository variable); otherwise the newest
      release/X.Y.Z-candidate with commits main lacks, else main.
  nightly.py stamp VERSION
      Set the eternal-host version in host/Cargo.toml and Cargo.lock of the
      checkout in the current directory. The runner builds it; nothing is committed.
  nightly.py notes --version V --branch B --sha SHA --stable X.Y.Z --sha256 HASH
      Print the release notes.
  nightly.py publish --tag TAG --sha SHA --title T --notes-file F INSTALLER
      Publish the dated release, then point the fixed beta link at it (publish()).
      Uses gh with GH_TOKEN; the downloads are checked anonymously.
  nightly.py expired [--keep 7] < releases.json
      Print the beta-* tags to delete, oldest last, from `gh release list
      --json tagName,isPrerelease,createdAt`.

Users see these builds as the Beta channel. Each is a semver pre-release,
<next>-beta.<YYYYMMDD>.<run>, which semver orders below <next> and above a
hand-made <next>-beta.1. The apps never offer a pre-release, and a Beta user
is offered the next stable release as usual (host/src/updates.rs).
"""
import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

REPO = "sidebandstudio/EternalMonitor"
ASSET = "EternalMonitor-Setup-beta.exe"
MOVING_TAG = "beta"
# The moving release's asset while it is replaced: the new build goes up as
# NEXT, the old one steps aside as OLD, then NEXT takes ASSET's name.
NEXT = "EternalMonitor-Setup-beta.next.exe"
OLD = "EternalMonitor-Setup-beta.old.exe"
MARKER = re.compile(r"<!-- beta-commit: ([0-9a-f]{40}) -->")
CANDIDATE = re.compile(r"release/(\d+)\.(\d+)\.(\d+)-candidate")
DATED_TAG = re.compile(r"beta-\d{8}-[0-9a-f]{7,40}")
CORE = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:[-+].*)?")
VERSION = re.compile(r"(\d+\.\d+\.\d+)-beta\.(20\d{6})\.[1-9]\d*")


def core(version):
    match = CORE.fullmatch(version)
    if not match:
        raise ValueError(f"not a version: {version!r}")
    return tuple(map(int, match.groups()))


def next_version(host, stable):
    """The release a nightly leads up to: the host's own version when it is ahead of
    the published stable release, otherwise the stable release's next patch."""
    host, stable = core(host), core(stable)
    if host > stable:
        return "%d.%d.%d" % host
    return "%d.%d.%d" % (stable[0], stable[1], stable[2] + 1)


def nightly_version(base, date, run):
    if not re.fullmatch(r"20\d{6}", date) or int(run) < 1:
        raise ValueError(f"bad date or run number: {date!r}, {run!r}")
    return f"{base}-beta.{date}.{int(run)}"


def nightly_tag(date, sha):
    return f"beta-{date}-{sha[:7]}"


def title(version):
    """EternalMonitor 0.3.4 beta (Oct 30, 2026)"""
    match = VERSION.fullmatch(version)
    if not match:
        raise ValueError(f"not a nightly version: {version!r}")
    day = datetime.datetime.strptime(match[2], "%Y%m%d")
    return f"EternalMonitor {match[1]} beta ({day:%b} {day.day}, {day.year})"


def pick_branch(override, ahead):
    """override, else the newest candidate with commits main lacks, else main.
    ahead maps release branch names to their count of commits not on main."""
    if override:
        return override
    candidates = sorted((tuple(map(int, m.groups())), name)
                        for name in ahead if (m := CANDIDATE.fullmatch(name)))
    for _, name in reversed(candidates):
        if ahead[name] > 0:
            return name
    return "main"


def package_version(cargo_toml):
    package = re.search(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)", cargo_toml)
    match = package and re.search(r'(?m)^version\s*=\s*"([^"]+)"', package[1])
    if not match:
        raise ValueError("no [package] version in host/Cargo.toml")
    return match[1]


def stamp(cargo_toml, cargo_lock, version):
    """Both files with eternal-host's version set to version."""
    core(version)
    current = package_version(cargo_toml)
    toml, n = re.subn(r'(?ms)(^\[package\]\s*\n(?:(?!^\[).)*?^version\s*=\s*")[^"]+(")',
                      lambda m: m[1] + version + m[2], cargo_toml, count=1)
    lock, m = re.subn(r'(?m)(^name = "eternal-host"\nversion = ")' + re.escape(current) + '(")',
                      lambda g: g[1] + version + g[2], cargo_lock)
    if n != 1 or m != 1:
        raise ValueError("expected one eternal-host version in host/Cargo.toml and in Cargo.lock")
    return toml, lock


def notes(version, branch, sha, stable, sha256, repo=REPO):
    if not re.fullmatch(r"[0-9a-f]{40}", sha) or not re.fullmatch(r"[0-9A-Fa-f]{64}", sha256):
        raise ValueError("notes need the full commit and the installer's SHA-256")
    return (
        f"Version {version}, from the Beta channel: the newest fixes, built every night. "
        f"For testing; you can go back to Stable anytime.\n"
        f"\n"
        f"Built from `{branch}` at [`{sha[:7]}`](https://github.com/{repo}/commit/{sha}). "
        f"[Changes since v{stable}](https://github.com/{repo}/compare/v{stable}...{sha}).\n"
        f"\n"
        f"To go back to Stable, run the installer from https://eternalmonitor.dev/download.html "
        f"over this one. The app offers the next Stable version when it comes out. "
        f"iPad betas come through TestFlight: https://eternalmonitor.dev/download.html#ipad\n"
        f"\n"
        f"SHA-256 of {ASSET}:\n"
        f"\n"
        f"```\n{sha256.lower()}\n```\n"
    )


def expired(releases, keep=7):
    """Tags of the dated beta-* pre-releases beyond the newest keep. Anything else,
    including the moving beta release, hand-made betas and every v* release, is
    never selected."""
    dated = [r for r in releases
             if r.get("isPrerelease") is True and DATED_TAG.fullmatch(r.get("tagName", ""))]
    dated.sort(key=lambda r: (r.get("createdAt", ""), r["tagName"]), reverse=True)
    return [r["tagName"] for r in dated[keep:]]


def download_url(tag, name=ASSET, repo=REPO):
    return f"https://github.com/{repo}/releases/download/{tag}/{name}"


def marker(sha):
    """The beta release body's last line once a build has finished publishing."""
    return f"<!-- beta-commit: {sha} -->"


def published_commit(body):
    match = MARKER.search(body or "")
    return match[1] if match else ""


class PublishError(Exception):
    pass


class GitHub:
    """The calls publish() makes: gh with GH_TOKEN for writes, plain HTTPS without
    credentials for the checks, so they see what a visitor sees."""

    def __init__(self, repo=None, run=subprocess.run):
        self.repo, self.run = repo or os.environ.get("GH_REPO", REPO), run

    def gh(self, *args):
        return self.run(["gh", *args], check=True, capture_output=True, text=True).stdout

    def release(self, tag):
        """The release's API object, or None when there is none."""
        result = self.run(["gh", "api", f"repos/{self.repo}/releases/tags/{tag}"],
                          capture_output=True, text=True)
        if result.returncode == 0:
            return json.loads(result.stdout)
        if "HTTP 404" in result.stderr:
            return None
        raise PublishError(f"reading release {tag}: {result.stderr.strip()}")

    def create(self, tag, sha, title, notes, path=None):
        self.gh("release", "create", tag, *([path] if path else []), "--repo", self.repo, "--target", sha,
                "--title", title, "--notes", notes, "--prerelease", "--latest=false")

    def edit(self, tag, title, notes):
        self.gh("release", "edit", tag, "--repo", self.repo,
                "--title", title, "--notes", notes, "--prerelease", "--latest=false")

    def upload(self, tag, path, name):
        """path as asset name, replacing an asset of that name."""
        with tempfile.TemporaryDirectory() as tmp:
            named = os.path.join(tmp, name)
            shutil.copyfile(path, named)
            self.gh("release", "upload", tag, named, "--repo", self.repo, "--clobber")

    def rename(self, asset_id, name):
        self.gh("api", "-X", "PATCH", f"repos/{self.repo}/releases/assets/{asset_id}", "-f", f"name={name}")

    def delete(self, asset_id):
        self.gh("api", "-X", "DELETE", f"repos/{self.repo}/releases/assets/{asset_id}")

    def move_tag(self, tag, sha):
        self.gh("api", "-X", "PATCH", f"repos/{self.repo}/git/refs/tags/{tag}", "-f", f"sha={sha}", "-F", "force=true")

    def head(self, url):
        """(status, Content-Length) of an anonymous HEAD, following redirects."""
        try:
            with urllib.request.urlopen(urllib.request.Request(url, method="HEAD"), timeout=60) as response:
                return response.status, int(response.headers.get("Content-Length", -1))
        except urllib.error.HTTPError as error:
            return error.code, -1

    def sha256(self, url):
        digest = hashlib.sha256()
        with urllib.request.urlopen(url, timeout=300) as response:
            for chunk in iter(lambda: response.read(1 << 20), b""):
                digest.update(chunk)
        return digest.hexdigest()


def assets(release, name):
    return [a for a in (release or {}).get("assets", []) if a["name"] == name]


def verify(github, url, size, sha256=None, tries=6, wait=10, sleep=time.sleep):
    """url serves size bytes (with sha256, those exact bytes) to an anonymous visitor.
    A new or renamed asset can take a few seconds to show, so it retries."""
    for attempt in range(tries):
        try:
            status, length = github.head(url)
            seen = f"HTTP {status}, {length} bytes"
            if status == 200 and length == size:
                if sha256 is None:
                    return
                got = github.sha256(url)
                if got == sha256:
                    return
                seen = f"SHA-256 {got}"
        except OSError as error:
            seen = str(error)
        if attempt + 1 < tries:
            sleep(wait)
    raise PublishError(f"{url}: {seen}; expected {size} bytes" + (f", SHA-256 {sha256}" if sha256 else ""))


def reconcile(github, size=None, sha256=None, new=None, old=None, sleep=time.sleep):
    """Make the fixed link serve a whole build again, from the beta release's assets
    as GitHub lists them now, whatever a failed call did or did not do before it
    reported the failure. new and old are the ids of this run's upload and of the
    asset the link served before it. The start of a run knows neither and takes
    the asset named OLD as the old build.

    The link keeps the build it serves unless that is the new build and fails its
    check. Otherwise the new build takes the name if it checks out (size and
    SHA-256 of an anonymous download), else the old one; then the link is checked."""
    beta = github.release(MOVING_TAG)
    if beta is None:
        return
    found = {a["id"]: a for a in beta.get("assets", [])}
    served = next(iter(assets(beta, ASSET)), None)
    if old is None:
        old = next((a["id"] for a in assets(beta, OLD)), None)

    def whole(asset):
        try:
            verify(github, download_url(MOVING_TAG, asset["name"], github.repo), size, sha256, sleep=sleep)
            return True
        except PublishError:
            return False

    if served and (served["id"] != new or whole(served)):
        return
    if new in found and found[new] is not served and whole(found[new]):
        pick = found[new]
    elif old in found:
        pick = found[old]
    else:
        return  # no build to put back: the first one never went up whole
    if served:
        github.delete(served["id"])  # the new build, which failed its check
    github.rename(pick["id"], ASSET)
    verify(github, download_url(MOVING_TAG, repo=github.repo), pick["size"],
           sha256 if pick["id"] == new else None, sleep=sleep)


def publish(github, tag, sha, title, notes, path, sleep=time.sleep):
    """Publish the dated release, then point the fixed beta link at it.

    Every step is checked. The beta release body gets marker(sha) last, and plan()
    only skips a commit that carries it, so the next run finishes a run that
    stopped partway: it reuses a dated release that exists and replaces a
    leftover NEXT. The fixed link is missing only between the two renames, one
    API call (about a second). Any failure in the swap, including a call GitHub
    carried out before the request failed, ends in reconcile(), which puts a whole
    build back under the fixed name from the assets GitHub actually has; every run
    starts with reconcile() too, for a run that stopped inside it."""
    size = os.path.getsize(path)
    with open(path, "rb") as f:
        sha256 = hashlib.sha256(f.read()).hexdigest()
    reconcile(github, sleep=sleep)

    # 1. The dated release, checked from outside before anything links to it.
    if github.release(tag) is None:
        github.create(tag, sha, title, notes, path)
    else:
        github.edit(tag, title, notes)
        github.upload(tag, path, ASSET)
    verify(github, download_url(tag, repo=github.repo), size, sleep=sleep)

    # 2. The moving beta release: NEXT goes up beside the working asset, then
    # the two swap names.
    new = old = None
    try:
        if github.release(MOVING_TAG) is None:
            github.create(MOVING_TAG, sha, title, notes)
        github.upload(MOVING_TAG, path, NEXT)
        beta = github.release(MOVING_TAG)
        uploaded = assets(beta, NEXT)
        if not uploaded or uploaded[0]["size"] != size:
            raise PublishError(f"{NEXT} did not upload whole: {uploaded}")
        new = uploaded[0]["id"]
        old = next((a["id"] for a in assets(beta, ASSET)), None)
        for stale in assets(beta, OLD):
            github.delete(stale["id"])
        if old:
            github.rename(old, OLD)
        github.rename(new, ASSET)
        verify(github, download_url(MOVING_TAG, repo=github.repo), size, sha256, sleep=sleep)
        if old:
            github.delete(old)
    except Exception:
        reconcile(github, size, sha256, new, old, sleep=sleep)
        raise

    # 3. The tag, then the marker that says this commit is done.
    github.move_tag(MOVING_TAG, sha)
    github.edit(MOVING_TAG, title, f"{notes.rstrip()}\n\n{marker(sha)}\n")


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def plan(override, run, date):
    refs = ["+refs/heads/main:refs/remotes/origin/main",
            "+refs/heads/release/*:refs/remotes/origin/release/*"]
    if override:
        refs.append(f"+refs/heads/{override}:refs/remotes/origin/{override}")
    git("fetch", "--quiet", "--no-tags", "origin", *refs)
    branches = git("for-each-ref", "--format=%(refname:lstrip=3)", "refs/remotes/origin/release/").split()
    ahead = {name: int(git("rev-list", "--count", f"origin/main..origin/{name}"))
             for name in branches if CANDIDATE.fullmatch(name)}
    branch = pick_branch(override, ahead)
    sha = git("rev-parse", f"origin/{branch}^{{commit}}").strip()
    stable = json.loads(git("show", "origin/main:docs/latest.json"))["windows"]["version"]
    host = package_version(git("show", f"{sha}:host/Cargo.toml"))
    version = nightly_version(next_version(host, stable), date, run)
    # Not the tag: a build that stopped partway is published again.
    last = published_commit((GitHub().release(MOVING_TAG) or {}).get("body"))
    return {
        "branch": branch,
        "sha": sha,
        "revision": sha[:12],
        "stable": stable,
        "version": version,
        "title": title(version),
        "tag": nightly_tag(date, sha),
        "last": last,
        "changed": str(sha != last).lower(),
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("plan")
    p.add_argument("--branch", default="")
    p.add_argument("--run", type=int, default=1)
    p.add_argument("--date", default=datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d"))
    s = sub.add_parser("stamp")
    s.add_argument("version")
    n = sub.add_parser("notes")
    for name in ("version", "branch", "sha", "stable", "sha256"):
        n.add_argument(f"--{name}", required=True)
    u = sub.add_parser("publish")
    for name in ("tag", "sha", "title", "notes-file"):
        u.add_argument(f"--{name}", required=True)
    u.add_argument("installer")
    e = sub.add_parser("expired")
    e.add_argument("--keep", type=int, default=7)
    args = parser.parse_args(argv)

    if args.command == "plan":
        for key, value in plan(args.branch.strip(), args.run, args.date).items():
            print(f"{key}={value}")
    elif args.command == "stamp":
        with open("host/Cargo.toml", encoding="utf-8", newline="") as f:
            toml = f.read()
        with open("Cargo.lock", encoding="utf-8", newline="") as f:
            lock = f.read()
        toml, lock = stamp(toml, lock.replace("\r\n", "\n"), args.version)
        for path, text in (("host/Cargo.toml", toml), ("Cargo.lock", lock)):
            with open(path, "w", encoding="utf-8", newline="") as f:
                f.write(text)
        print(f"eternal-host version {args.version}")
    elif args.command == "notes":
        sys.stdout.write(notes(args.version, args.branch, args.sha, args.stable, args.sha256))
    elif args.command == "publish":
        with open(args.notes_file, encoding="utf-8") as f:
            text = f.read()
        publish(GitHub(), args.tag, args.sha, args.title, text, args.installer)
        print(f"Published {args.tag} and {download_url(MOVING_TAG)}")
    else:
        for tag in expired(json.load(sys.stdin), args.keep):
            print(tag)


if __name__ == "__main__":
    main()
