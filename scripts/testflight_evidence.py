#!/usr/bin/env python3
"""Check TestFlight build evidence before it uploads as a public artifact.

  testflight_evidence.py scan PATH...
      Fail when any file under PATH holds a secret value from the job's
      environment, its first 12 characters or its URL host, as written or
      escaped for JSON or a shell. Outside pull requests, also fail when any
      secret is missing or empty. Prints names, never values.

The repository is public and artifact files are not masked like the job log.
"""
import json
import os
import pathlib
import re
import shlex
import sys

SECRETS = ["ETERNAL_DIAGNOSTICS_ENDPOINT", "ETERNAL_DIAGNOSTICS_INGEST_KEY", "ASC_KEY_ID", "ASC_ISSUER_ID",
           "ASC_KEY_P8", "IOS_DIST_P12_BASE64", "IOS_DIST_P12_PASSWORD", "IOS_PROVISION_PROFILE_BASE64"]


def forms(value):
    """The value as written, JSON-escaped and shell-escaped. xcodebuild echoes
    its invocation with quotes and backslashes escaped."""
    return {value, json.dumps(value)[1:-1], json.dumps(value, ensure_ascii=False)[1:-1],
            re.sub(r'(["\'\\])', r"\\\1", value), shlex.quote(value)}


def needles():
    missing, found = [], []
    for name in SECRETS:
        value = os.environ.get(name, "").strip()
        if not value:
            missing.append(name)
            continue
        parts = [(name, value), (name + " prefix", value[:12])]
        host = re.match(r"https?://([^/:]+)", value)
        if host:
            parts.append((name + " host", host[1]))
        found += [(label, form) for label, part in parts for form in forms(part)]
    return missing, found


def scan(paths):
    missing, checked = needles()
    if missing and os.environ.get("GITHUB_EVENT_NAME") != "pull_request":
        sys.exit("Secrets are missing or empty, so the evidence cannot be checked; nothing was uploaded:\n"
                 + "\n".join(missing))
    files = [f for p in paths for f in ([p] if p.is_file() else sorted(p.rglob("*"))) if f.is_file()]
    found = []
    for path in files:
        data = path.read_bytes()
        found += sorted({f"{path}: {name}" for name, value in checked if value.encode() in data})
    if found:
        sys.exit("Evidence contains secret values; nothing was uploaded:\n" + "\n".join(found))
    print(f"No secret values in {len(files)} evidence files ({len(checked)} forms checked)")


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "scan":
        scan([pathlib.Path(p) for p in sys.argv[2:] if pathlib.Path(p).exists()])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
