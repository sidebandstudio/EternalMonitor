#!/usr/bin/env python3
"""Point docs/latest.json and docs/download.html at a published release.

  latest_json.py publish [--windows VERSION] [--ipad VERSION --build N] [--note TEXT] [--not-recommended]
                         [--rollout-start YYYY-MM-DDTHH:MM:SSZ --rollout-days N]
                         [--windows-config FILE|none] [--ipad-config FILE|none] [--skip-live-check]
  latest_json.py rollback [--windows VERSION] [--ipad VERSION] [--note TEXT] [--not-recommended]
                          [--windows-config FILE|none] [--ipad-config FILE|none] [--skip-live-check]

Windows takes the installer's SHA-256 from GitHub release vVERSION: the
release must be published and stable, and its asset digest (or the installer,
downloaded to a temporary folder when GitHub lists no digest) must match the
hash in the release body, which the in-app updater also checks.

publish names a TestFlight build at least as new as the current one; Apple
must have approved it for an external group, checked through App Store
Connect with the ASC_KEY_ID, ASC_ISSUER_ID and ASC_KEY_P8 or ASC_KEY_PATH that
testflight_review.py takes. rollback takes an older version that
docs/latest.json published before, with the iPad build from its git history,
and checks App Store Connect too when those variables are set.

This checkout's docs/latest.json can be behind the one the apps read, so both
commands also compare with https://eternalmonitor.dev/latest.json: publish
refuses a Windows version, iPad version or iPad build older than the live
one, and rollback refuses one that is not older. A platform the command
leaves alone must not be older than the live one either, or the branch needs
updating first. If the live manifest cannot be read, both commands stop
unless --skip-live-check is given.

Both apps only offer a strictly newer version than the one installed, so a
rollback never prompts anyone already on the bad release; it stops new
prompts for it. --note, --not-recommended and the --rollout pair (publish
only: stage the automatic prompt over 0 to 14 days from a start at most 14
days ahead) apply to each platform given; without them the old note and
rollout go. A platform's config (docs/remote-config.md) is kept unless
--windows-config or --ipad-config names a JSON file to use instead, or
"none" to remove it; either may be given without a release to change only
the config. Every optional field is checked by scripts/manifest_fields.py,
as check-versions.sh checks it. scripts/check-versions.sh must pass, or both
files are restored. Then the git commands for the pull request are printed.
"""
import argparse
import hashlib
import json
import os
import pathlib
import re
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import manifest_fields  # noqa: E402
import testflight_review  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
REPO = "sidebandstudio/EternalMonitor"
INSTALLER = "EternalMonitor-Setup.exe"
MAX_INSTALLER_BYTES = 512 * 1024 * 1024  # host/src/updates.rs
TESTFLIGHT = re.compile(r"https://testflight\.apple\.com/join/[A-Za-z0-9]+")
VERSION = re.compile(r"\d+\.\d+\.\d+")
LIVE = "https://eternalmonitor.dev/latest.json"


class ManifestError(Exception):
    pass


def version_key(version):
    return tuple(map(int, version.split(".")))


def installer_url(version):
    return f"https://github.com/{REPO}/releases/download/v{version}/{INSTALLER}"


def github_release(tag):
    headers = {"Accept": "application/vnd.github+json", "User-Agent": "EternalMonitor-latest-json"}
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request(f"https://api.github.com/repos/{REPO}/releases/tags/{tag}", headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except OSError as error:
        raise ManifestError(f"GitHub release {tag}: {error}") from None


def download_sha256(url, size):
    """The SHA-256 of a download, kept in a temporary folder that is removed."""
    digest, total = hashlib.sha256(), 0
    with tempfile.TemporaryDirectory(prefix="latest-json-") as tmp:
        with urllib.request.urlopen(url, timeout=120) as response, open(pathlib.Path(tmp) / INSTALLER, "wb") as out:
            while chunk := response.read(1 << 20):
                digest.update(chunk)
                out.write(chunk)
                total += len(chunk)
    if total != size:
        raise ManifestError(f"{url} downloaded {total} bytes; GitHub lists {size}")
    return digest.hexdigest()


def windows_release(version, release=github_release, download=download_sha256):
    """(SHA-256, size) of a published stable release, as the updater checks it."""
    tag = f"v{version}"
    data = release(tag)
    if data.get("tag_name") != tag or data.get("draft") or data.get("prerelease"):
        raise ManifestError(f"{tag} is not a published stable release")
    body = re.search(r"(?is)SHA-256.*?\b([0-9a-f]{64})\b", data.get("body") or "")
    if not body:
        raise ManifestError(f"{tag}'s release body has no SHA-256")
    sha256 = body[1].lower()
    asset = next((a for a in data.get("assets", []) if a.get("name") == INSTALLER), None)
    if not asset or asset.get("browser_download_url") != installer_url(version):
        raise ManifestError(f"{tag} has no {INSTALLER}")
    if not 0 < asset.get("size", 0) <= MAX_INSTALLER_BYTES:
        raise ManifestError(f"{tag}'s installer size is out of range")
    digest = (asset.get("digest") or "").lower()
    actual = digest.removeprefix("sha256:") if digest.startswith("sha256:") else download(
        asset["browser_download_url"], asset["size"])
    if actual != sha256:
        raise ManifestError(f"{tag}'s installer SHA-256 differs from its release body")
    return sha256, asset["size"]


def live_manifest():
    """The docs/latest.json GitHub Pages serves to the apps now."""
    # A query string keeps a cached copy from before the last deploy out of the way.
    request = urllib.request.Request(f"{LIVE}?checked={int(time.time())}",
                                     headers={"Cache-Control": "no-cache", "User-Agent": "EternalMonitor-latest-json"})
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)
    except (OSError, ValueError) as error:
        raise ManifestError(f"could not read {LIVE} ({error}); retry, or pass --skip-live-check") from None


def check_live(args, latest, live):
    """Refuse to move a platform the wrong way past what the apps read now."""
    try:
        windows, ipad, build = live["windows"]["version"], live["ipad"]["version"], live["ipad"]["build"]
        if not (VERSION.fullmatch(windows) and VERSION.fullmatch(ipad)) or type(build) is not int:
            raise TypeError
    except (KeyError, TypeError):
        raise ManifestError(f"{LIVE} has no valid Windows version, iPad version and iPad build") from None
    def older(named, what, than):
        # A platform the command leaves alone comes from this checkout, which may be behind.
        if named:
            return ManifestError(f"{what} is older than the live {than}; use rollback")
        return ManifestError(f"this checkout's docs/latest.json names {what}, older than the live {than}; "
                             "update the branch")

    rollback = args.command == "rollback"
    new = latest["windows"]["version"]
    if rollback and args.windows:
        if version_key(new) >= version_key(windows):
            raise ManifestError(f"Windows {new} is not older than the live {windows}; use publish")
    elif version_key(new) < version_key(windows):
        raise older(args.windows, f"Windows {new}", windows)
    new, new_build = latest["ipad"]["version"], latest["ipad"]["build"]
    if rollback and args.ipad:
        if new_build >= build:
            raise ManifestError(f"iPad {new} build {new_build} is not older than the live build {build}; use publish")
    elif new_build < build:
        raise older(args.ipad, f"iPad {new} build {new_build}", f"build {build}")
    elif version_key(new) < version_key(ipad):
        raise older(args.ipad, f"iPad {new} build {new_build}", f"iPad {ipad}")


def published_ipad_builds(root, version):
    """Every iPad build docs/latest.json has named for a version on this branch."""
    log = subprocess.run(["git", "-C", str(root), "log", "--format=%H", "--", "docs/latest.json"],
                         capture_output=True, text=True, check=True).stdout.split()
    builds = set()
    for commit in log:
        shown = subprocess.run(["git", "-C", str(root), "show", f"{commit}:docs/latest.json"],
                               capture_output=True, text=True)
        try:
            ipad = json.loads(shown.stdout).get("ipad") or {}
        except ValueError:
            continue
        if shown.returncode == 0 and ipad.get("version") == version and isinstance(ipad.get("build"), int):
            builds.add(ipad["build"])
    return builds


def check_testflight(asc, version, build):
    """Apple approved this build and an external group can install it."""
    found = testflight_review.find_build(asc, version, build)
    if not found:
        raise ManifestError(f"App Store Connect has no iPad {version} build {build}")
    doc, item = found
    if item["attributes"].get("expired"):
        raise ManifestError(f"TestFlight build {build} has expired")
    review = testflight_review.related(doc, item, "betaAppReviewSubmission")
    state = review[0].get("attributes", {}).get("betaReviewState") if review else None
    if state != "APPROVED":
        raise ManifestError(f"TestFlight build {build} is not approved for external testing ({state})")
    groups = testflight_review.related(doc, item, "betaGroups")
    if not any(g.get("attributes", {}).get("isInternalGroup") is False for g in groups):
        raise ManifestError(f"TestFlight build {build} is in no external group")


def short_note(note):
    """The rule both apps apply: one trimmed line of at most 200 characters,
    without control, format or line and paragraph separator characters."""
    note = note.strip()
    try:
        manifest_fields.check_note("--note", note)
    except ValueError:
        raise ManifestError("--note must be one line of 1 to 200 characters") from None
    return note


def read_config(value):
    """--windows-config/--ipad-config: a JSON object from a file, or None for "none"."""
    if value == "none":
        return None
    try:
        config = json.loads(pathlib.Path(value).read_text())
    except (OSError, ValueError) as error:
        raise ManifestError(f"config {value}: {error}") from None
    if not isinstance(config, dict):
        raise ManifestError(f"config {value} must hold a JSON object")
    return config


def with_config(entry, current, change):
    """`entry` with the platform's config: `change` when given, else `current`'s."""
    config = current.get("config") if change is ... else change
    entry = {key: value for key, value in entry.items() if key != "config"}
    return entry if config is None else {**entry, "config": config}


def safe_manifest(latest):
    """Only fields the apps know, each one they accept. Nothing can force or block an update."""
    if set(latest) != {"schema", "windows", "ipad"} or latest["schema"] != 1:
        raise ManifestError("latest.json must hold schema 1, windows and ipad only")
    windows, ipad = latest["windows"], latest["ipad"]
    optional = {"recommended", "note", "rollout", "config"}
    if not {"version", "installer", "sha256"} <= set(windows) <= {"version", "installer", "sha256"} | optional:
        raise ManifestError("unexpected Windows fields")
    if not {"version", "build", "testflight"} <= set(ipad) <= {"version", "build", "testflight"} | optional:
        raise ManifestError("unexpected iPad fields")
    for entry in (windows, ipad):
        if not VERSION.fullmatch(entry["version"]):
            raise ManifestError(f"invalid version {entry['version']!r}")
        if "recommended" in entry and not isinstance(entry["recommended"], bool):
            raise ManifestError("recommended must be true or false")
        if "note" in entry and short_note(entry["note"]) != entry["note"]:
            raise ManifestError("note must be one trimmed line")
    if windows["installer"] != installer_url(windows["version"]) or not re.fullmatch(r"[0-9a-f]{64}", windows["sha256"]):
        raise ManifestError("Windows installer URL or SHA-256 is invalid")
    if not isinstance(ipad["build"], int) or isinstance(ipad["build"], bool) or ipad["build"] <= 0:
        raise ManifestError("iPad build must be a positive integer")
    if not TESTFLIGHT.fullmatch(ipad["testflight"]):
        raise ManifestError("iPad TestFlight link is invalid")
    try:
        manifest_fields.check(windows, ipad)
    except ValueError as error:
        raise ManifestError(str(error)) from None


def replace_once(html, pattern, value, what):
    updated, count = re.subn(pattern, lambda m: m[1] + value + m[2], html)
    if count != 1:
        raise ManifestError(f"docs/download.html: expected one {what}, found {count}")
    return updated


def update_page(html, latest, size):
    windows, ipad = latest["windows"], latest["ipad"]
    tag = "v" + windows["version"]
    edits = [
        (r'(<[^>]*\bid="release-version"[^>]*>)[^<]*(<)', tag, "#release-version"),
        (r'(<a href=")[^"]*(" id="download-btn")', windows["installer"], "#download-btn"),
        (r'(<a id="release-notes-link" href=")[^"]*(")', f"https://github.com/{REPO}/releases/tag/{tag}",
         "#release-notes-link"),
        (r'(<code id="sha256-value">)[^<]*(<)', windows["sha256"], "#sha256-value"),
        (r'(<span data-ipad-version>)[^<]*(<)', f"v{ipad['version']}, build {ipad['build']}", "[data-ipad-version]"),
        (r'(<form id="feedback-form"[^>]*\bdata-release=")[^"]*(")',
         f"Windows {tag} · iPad v{ipad['version']}, TestFlight build {ipad['build']}", "feedback data-release"),
    ]
    if size is not None:
        # docs/script.js formatBytes.
        shown = f"{size / 1024:.0f} KB" if size < 1048576 else f"{size / 1048576:.1f} MB"
        edits.append((r'(<span id="release-meta">)[^<]*(<)', f"{INSTALLER} · {shown}", "#release-meta"))
    for pattern, value, what in edits:
        html = replace_once(html, pattern, value, what)
    return html


def build_manifest(args, current, release, download, connect, root):
    latest = json.loads(json.dumps(current))
    size, changes = None, []
    flags = {}
    if args.note is not None:
        flags["note"] = short_note(args.note)
    if args.not_recommended:
        flags["recommended"] = False
    if args.rollout_start is not None:
        flags["rollout"] = {"start": args.rollout_start, "days": args.rollout_days}
    configs = {platform: ... if value is None else read_config(value)
               for platform, value in (("windows", args.windows_config), ("ipad", args.ipad_config))}
    if args.windows:
        old = current["windows"]["version"]
        if args.command == "rollback" and version_key(args.windows) >= version_key(old):
            raise ManifestError(f"Windows {args.windows} is not older than the published {old}; use publish")
        if args.command == "publish" and version_key(args.windows) < version_key(old):
            raise ManifestError(f"Windows {args.windows} is older than the published {old}; use rollback")
        sha256, size = windows_release(args.windows, release, download)
        latest["windows"] = with_config({"version": args.windows, "installer": installer_url(args.windows),
                                         "sha256": sha256, **flags}, current["windows"], configs["windows"])
        changes.append(f"Windows {args.windows}")
    elif configs["windows"] is not ...:
        latest["windows"] = with_config(latest["windows"], current["windows"], configs["windows"])
        changes.append("Windows config")
    if args.ipad:
        asc = connect()
        old = current["ipad"]["build"]
        if args.command == "rollback":
            builds = published_ipad_builds(root, args.ipad)
            if not builds:
                raise ManifestError(f"docs/latest.json never published iPad {args.ipad} on this branch")
            build = max(builds)
            if build >= old:
                raise ManifestError(f"iPad {args.ipad} build {build} is not older than the published build {old}")
        else:
            build = args.build
            if build < old:
                raise ManifestError(f"build {build} is older than the published build {old}; use rollback")
            # The iPad app compares versions before builds, so an older version is never offered.
            if version_key(args.ipad) < version_key(current["ipad"]["version"]):
                raise ManifestError(f"iPad {args.ipad} is older than the published {current['ipad']['version']}; "
                                    "use rollback")
            if asc is None:
                raise ManifestError("publish --ipad checks App Store Connect: set ASC_KEY_ID and ASC_ISSUER_ID")
        if asc is not None:
            check_testflight(asc, args.ipad, build)
        else:
            print("App Store Connect was not checked (no ASC_KEY_ID): confirm the build has not expired in TestFlight")
        latest["ipad"] = with_config({"version": args.ipad, "build": build,
                                      "testflight": current["ipad"]["testflight"], **flags},
                                     current["ipad"], configs["ipad"])
        changes.append(f"iPad {args.ipad} build {build}")
    elif configs["ipad"] is not ...:
        latest["ipad"] = with_config(latest["ipad"], current["ipad"], configs["ipad"])
        changes.append("iPad config")
    safe_manifest(latest)
    return latest, size, changes


def pull_request_commands(command, changes, latest):
    verb = "Roll back the update manifest to" if command == "rollback" else "Publish"
    title = f"{verb} {' and '.join(changes)}"
    slug = re.sub(r"[^a-z0-9.]+", "-", f"{command} {' '.join(changes)}".lower()).strip("-")
    windows = latest["windows"]
    body = (f"Points docs/latest.json and docs/download.html at Windows {windows['version']} "
            f"(SHA-256 {windows['sha256']}, from the GitHub release) and iPad {latest['ipad']['version']} "
            f"build {latest['ipad']['build']}. Written by scripts/latest_json.py; scripts/check-versions.sh passes.")
    return [f"git switch -c {slug}", "git add docs/latest.json docs/download.html",
            f"git commit -m {shlex.quote(title)}", f"git push -u origin {slug}",
            f"gh pr create --base main --title {shlex.quote(title)} --body {shlex.quote(body)}"]


def asc_from_env():
    return testflight_review.AppStoreConnect.from_env() if os.environ.get("ASC_KEY_ID") else None


def main(argv=None, root=ROOT, release=github_release, download=download_sha256, connect=asc_from_env,
         live=live_manifest):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=("publish", "rollback"))
    parser.add_argument("--windows", metavar="VERSION")
    parser.add_argument("--ipad", metavar="VERSION")
    parser.add_argument("--build", type=int, help="TestFlight build number (publish --ipad)")
    parser.add_argument("--note", help="one line for the update popup, at most 200 characters")
    parser.add_argument("--not-recommended", action="store_true", help="soften the popup's wording")
    parser.add_argument("--rollout-start", metavar="TIME", help="stage the automatic prompt from this UTC time")
    parser.add_argument("--rollout-days", type=int, metavar="N", help="over this many days, 0 to 14")
    parser.add_argument("--windows-config", metavar="FILE", help="windows.config as a JSON file, or none")
    parser.add_argument("--ipad-config", metavar="FILE", help="ipad.config as a JSON file, or none")
    parser.add_argument("--skip-live-check", action="store_true",
                        help=f"do not compare with {LIVE} (when it cannot be read)")
    args = parser.parse_args(argv)
    if not (args.windows or args.ipad or args.windows_config or args.ipad_config):
        parser.error("name --windows, --ipad, a config, or several")
    if (args.rollout_start is None) != (args.rollout_days is None):
        parser.error("--rollout-start and --rollout-days go together")
    if args.rollout_start is not None and (args.command != "publish" or not (args.windows or args.ipad)):
        parser.error("a rollout stages the prompt for a release: publish --windows or --ipad")
    for value in (args.windows, args.ipad):
        if value and not VERSION.fullmatch(value):
            parser.error(f"invalid version {value!r}")
    if args.command == "publish" and bool(args.ipad) != (args.build is not None):
        parser.error("publish takes --ipad and --build together")
    if args.command == "rollback" and args.build is not None:
        parser.error("rollback takes the iPad build from docs/latest.json's history")

    manifest, page = root / "docs/latest.json", root / "docs/download.html"
    before = {manifest: manifest.read_text(), page: page.read_text()}
    try:
        latest, size, changes = build_manifest(args, json.loads(before[manifest]), release, download, connect, root)
        if args.skip_live_check:
            print(f"{LIVE} was not checked (--skip-live-check): confirm nothing goes older than it by mistake")
        else:
            check_live(args, latest, live())
        html = update_page(before[page], latest, size)
    except (ManifestError, testflight_review.AscError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(f"latest_json.py: {error}")
    try:
        manifest.write_text(json.dumps(latest, indent=2, ensure_ascii=False) + "\n")
        page.write_text(html)
        check = subprocess.run(["bash", str(root / "scripts/check-versions.sh")], capture_output=True, text=True)
        problem = None if check.returncode == 0 else \
            f"failed, so both files were restored:\n{check.stdout}{check.stderr}"
    except OSError as error:
        problem = f"could not run ({error}), so both files were restored"
    if problem is not None:
        for path, text in before.items():
            path.write_text(text)
        sys.exit(f"scripts/check-versions.sh {problem}")
    print(check.stdout, end="")
    print(f"docs/latest.json and docs/download.html now name {' and '.join(changes)}. Open the pull request:\n")
    print("\n".join(pull_request_commands(args.command, changes, latest)))


if __name__ == "__main__":
    main()
