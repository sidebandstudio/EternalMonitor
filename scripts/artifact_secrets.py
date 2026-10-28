#!/usr/bin/env python3
"""Check workflow artifacts before they upload from this public repository.

  artifact_secrets.py scan --secrets NAME[,NAME...] PATH...
      Fail when any file under PATH holds the value of a named environment
      variable, its first 12 characters or its URL host, as written or
      escaped for JSON or a shell. Name every secret the job has. Outside
      pull requests, also fail when any named secret is missing or empty.
      Paths that do not exist are skipped. Prints names, never values.

Artifact files are not masked like the job log.
"""
import json
import os
import pathlib
import re
import shlex
import sys


def forms(value):
    """The value as written, JSON-escaped and shell-escaped. xcodebuild echoes
    its invocation with quotes and backslashes escaped."""
    return {value, json.dumps(value)[1:-1], json.dumps(value, ensure_ascii=False)[1:-1],
            re.sub(r'(["\'\\])', r"\\\1", value), shlex.quote(value)}


def needles(names):
    missing, found = [], []
    for name in names:
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


def scan(names, paths):
    missing, checked = needles(names)
    if missing and os.environ.get("GITHUB_EVENT_NAME") != "pull_request":
        sys.exit("Secrets are missing or empty, so the artifact cannot be checked; nothing was uploaded:\n"
                 + "\n".join(missing))
    files = [f for p in paths for f in ([p] if p.is_file() else sorted(p.rglob("*"))) if f.is_file()]
    found = []
    for path in files:
        data = path.read_bytes()
        found += sorted({f"{path}: {name}" for name, value in checked if value.encode() in data})
    if found:
        sys.exit("Artifact contains secret values; nothing was uploaded:\n" + "\n".join(found))
    print(f"No secret values in {len(files)} artifact files ({len(checked)} forms of {len(names)} secrets checked)")


def main():
    args = sys.argv[1:]
    if len(args) >= 4 and args[0] == "scan" and args[1] == "--secrets":
        names = [name for name in args[2].split(",") if name]
        if names and all(re.fullmatch(r"[A-Z][A-Z0-9_]*", name) for name in names):
            scan(names, [pathlib.Path(p) for p in args[3:] if pathlib.Path(p).exists()])
            return
    sys.exit(__doc__)


if __name__ == "__main__":
    main()
