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
PY
