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

# The optional fields both apps read (docs/remote-config.md). The apps fall
# back to their defaults on a bad value; this check keeps one from being
# published at all.
import datetime

def integer(value):
    return type(value) is int

def check_note(where, note):
    if note is None:
        return
    if (not isinstance(note, str) or not note.strip() or len(note.strip()) > 200
            or any(ord(c) < 32 or 127 <= ord(c) < 160 for c in note)):
        fail(f"{where}.note must be one line of at most 200 characters")

def check_rollout(where, rollout):
    if rollout is None:
        return
    if not isinstance(rollout, dict) or set(rollout) != {"start", "days"}:
        fail(f"{where}.rollout must be {{\"start\": ..., \"days\": ...}}")
    start = rollout["start"]
    try:
        if not isinstance(start, str) or not re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", start):
            raise ValueError
        datetime.datetime.strptime(start, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        fail(f"{where}.rollout.start must be a UTC time like 2026-11-02T00:00:00Z")
    if not integer(rollout["days"]) or not 0 <= rollout["days"] <= 14:
        fail(f"{where}.rollout.days must be a whole number from 0 to 14")

def check_config(where, config, values):
    if config is None:
        return
    if not isinstance(config, dict) or config.get("schema") != 1 or not integer(config["schema"]):
        fail(f"{where}.config must be an object with \"schema\": 1")
    unknown = set(config) - {"schema", *values}
    if unknown:
        fail(f"{where}.config has unknown keys {sorted(unknown)}")
    for key, bounds in values.items():
        if bounds and key in config and (not integer(config[key])
                                         or not bounds[0] <= config[key] <= bounds[1]):
            fail(f"{where}.config.{key} must be a whole number from {bounds[0]} to {bounds[1]}")

def version_parts(text):
    if not isinstance(text, str) or not re.fullmatch(r"\d+(\.\d+){0,3}", text):
        return None
    parts = [int(p) for p in text.split(".")]
    if any(p > 0xFFFFFFFF for p in parts):
        return None
    return parts + [0] * (4 - len(parts))

def check_range(where, value, keys):
    if not isinstance(value, dict) or set(value) != keys:
        fail(f"{where} must have exactly {sorted(keys)}")
    low, high = version_parts(value["min"]), version_parts(value["max"])
    if not low or not high or low > high or low[0] != high[0]:
        fail(f"{where} must be a min <= max version range within one major version")

def check_rules(rules):
    if rules is None:
        return
    if not isinstance(rules, list) or len(rules) > 16:
        fail("windows.config.second_screen_rules must be a list of at most 16 rules")
    vendors = {"amd", "intel", "nvidia", "qualcomm"}
    for index, rule in enumerate(rules):
        where = f"windows.config.second_screen_rules[{index}]"
        if not isinstance(rule, dict):
            fail(f"{where} must be an object")
        unknown = set(rule) - {"gpus", "gpu_driver", "display_driver", "windows"}
        if unknown:
            fail(f"{where} has unknown keys {sorted(unknown)}")
        gpus = rule.get("gpus")
        if (not isinstance(gpus, list) or not gpus or len(set(map(str, gpus))) != len(gpus)
                or not all(isinstance(g, str) and g in vendors for g in gpus)):
            fail(f"{where}.gpus must list distinct vendors from {sorted(vendors)}")
        if not {"gpu_driver", "display_driver", "windows"} & set(rule):
            fail(f"{where} must constrain a version range besides the graphics vendors")
        if "gpu_driver" in rule:
            check_range(f"{where}.gpu_driver", rule["gpu_driver"], {"gpu", "min", "max"})
            if rule["gpu_driver"]["gpu"] not in gpus:
                fail(f"{where}.gpu_driver.gpu must be one of its gpus")
        for key in ("display_driver", "windows"):
            if key in rule:
                check_range(f"{where}.{key}", rule[key], {"min", "max"})

known = {
    "windows": {"version", "installer", "sha256", "recommended", "note", "rollout", "config"},
    "ipad": {"version", "build", "testflight", "recommended", "note", "rollout", "config"},
}
for where, entry in (("windows", windows), ("ipad", ipad)):
    unknown = set(entry) - known[where]
    if unknown:
        fail(f"{where} has unknown keys {sorted(unknown)}")
    if "recommended" in entry and not isinstance(entry["recommended"], bool):
        fail(f"{where}.recommended must be true or false")
    check_note(where, entry.get("note"))
    check_rollout(where, entry.get("rollout"))
check_config("windows", windows.get("config"),
             {"attach_timeout_s": (20, 60), "second_screen_rules": None})
check_rules((windows.get("config") or {}).get("second_screen_rules"))
check_config("ipad", ipad.get("config"), {"first_frame_timeout_s": (8, 20)})
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
