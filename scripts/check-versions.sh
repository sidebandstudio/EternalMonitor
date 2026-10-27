#!/usr/bin/env bash
# Keep the host, wire crate, and iPad release versions together.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
python3 - "$ROOT" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
versions = {}
for name in ("host/Cargo.toml", "proto/Cargo.toml"):
    text = (root / name).read_text()
    package = re.search(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)", text)
    match = re.search(r'^version\s*=\s*"([^"]+)"', package[1], re.M) if package else None
    if not match:
        sys.exit(f"FAIL: no package version in {name}")
    versions[name] = match[1]

matches = re.findall(r'^\s*MARKETING_VERSION:\s*"([^\"]+)"\s*$',
                     (root / "ios/project.yml").read_text(), re.M)
if len(matches) != 1:
    sys.exit("FAIL: expected one MARKETING_VERSION in ios/project.yml")
versions["ios/project.yml"] = matches[0]
if len(set(versions.values())) != 1:
    sys.exit("FAIL: release versions differ: " + ", ".join(f"{k}={v}" for k, v in versions.items()))
version = matches[0]
if not re.fullmatch(r"\d+\.\d+\.\d+", version):
    sys.exit(f"FAIL: invalid release version {version!r}")
print(f"Release version: {version}")

# docs/latest.json tells both apps about updates. It must name exactly what
# docs/download.html offers, so the publish-after-TestFlight PR updates both.
import json

def fail(message):
    sys.exit(f"FAIL: docs/latest.json: {message}")

def element(html, element_id, attribute=None):
    tag = re.search(rf'<[^>]*\bid="{element_id}"[^>]*>(?:([^<]*)<)?', html)
    if not tag:
        fail(f"docs/download.html has no #{element_id}")
    if attribute is None:
        return tag[1].strip()
    value = re.search(rf'\b{attribute}="([^"]*)"', tag[0])
    return value[1] if value else None

html = (root / "docs/download.html").read_text()
try:
    latest = json.loads((root / "docs/latest.json").read_text())
except (OSError, ValueError) as error:
    fail(str(error))
windows, ipad = latest.get("windows") or {}, latest.get("ipad") or {}
if latest.get("schema") != 1:
    fail("schema must be 1")
published = windows.get("version", "")
if not re.fullmatch(r"\d+\.\d+\.\d+", published):
    fail(f"invalid Windows version {published!r}")
if tuple(map(int, published.split("."))) > tuple(map(int, version.split("."))):
    fail(f"publishes {published}, newer than this tree ({version})")
installer = f"https://github.com/sidebandstudio/EternalMonitor/releases/download/v{published}/EternalMonitor-Setup.exe"
checks = {
    "Windows version": (f"v{published}", element(html, "release-version")),
    "installer URL": (windows.get("installer"), installer),
    "download button": (windows.get("installer"), element(html, "download-btn", "href")),
    "SHA-256": (windows.get("sha256"), element(html, "sha256-value")),
    "TestFlight link": (ipad.get("testflight"), element(html, "testflight-link", "href")),
}
build = re.search(r'data-release="[^"]*\bbuild (\d+)', html)
checks["TestFlight build"] = (ipad.get("build"), int(build[1]) if build else None)
for name, (have, want) in checks.items():
    if have != want:
        fail(f"{name} is {have!r}; docs/download.html says {want!r}")
# The iPad entry may trail Windows while its TestFlight build awaits Apple's
# approval, but never lead it.
ipad_version = ipad.get("version", "")
if not re.fullmatch(r"\d+\.\d+\.\d+", ipad_version):
    fail(f"invalid iPad version {ipad_version!r}")
if tuple(map(int, ipad_version.split("."))) > tuple(map(int, published.split("."))):
    fail(f"iPad {ipad_version} is newer than Windows {published}")
shown = re.search(r"data-ipad-version>([^<]*)<", html)
if not shown or shown[1] != f"v{ipad_version}, build {ipad.get('build')}":
    fail(f"iPad {ipad_version} build {ipad.get('build')}; docs/download.html says {shown and shown[1]!r}")
if not re.fullmatch(r"[0-9a-f]{64}", windows["sha256"]):
    fail("SHA-256 must be 64 lowercase hex digits")
# The download page's script must take the release from latest.json, not a
# tag of its own, or the page could offer another version than this check saw.
script = (root / "docs/script.js").read_text()
if "fetch('latest.json'" not in script:
    fail("docs/script.js must read latest.json")
pinned = re.findall(r"releases/(?:tags|download)/v\d+\.\d+\.\d+|['\"]v\d+\.\d+\.\d+['\"]", script)
if pinned:
    fail(f"docs/script.js pins a release ({pinned[0]}); derive it from latest.json")
print(f"Published update: Windows {published}, iPad {ipad_version} build {ipad['build']}")
PY
