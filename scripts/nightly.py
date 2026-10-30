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
import json
import re
import subprocess
import sys

REPO = "sidebandstudio/EternalMonitor"
ASSET = "EternalMonitor-Setup-beta.exe"
MOVING_TAG = "beta"
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
    # The moving tag marks the commit the newest published build came from.
    last = ""
    for line in git("ls-remote", "--tags", "origin", f"refs/tags/{MOVING_TAG}", f"refs/tags/{MOVING_TAG}^{{}}").splitlines():
        last = line.split()[0]  # the peeled line, when present, comes last
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
    else:
        for tag in expired(json.load(sys.stdin), args.keep):
            print(tag)


if __name__ == "__main__":
    main()
