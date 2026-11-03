#!/usr/bin/env python3
"""Check workflow artifacts before they upload from this public repository.

  artifact_secrets.py scan --secrets NAME[,NAME...] [--optional NAME[,NAME...]] PATH...
      Fail when any file under PATH holds the value of a named environment
      variable, its first 12 characters or its URL host, as written or
      escaped for JSON or a shell. Name every secret the job has. Outside
      pull requests, also fail when any --secrets name is missing or empty.
      --optional names are checked the same way when set and are never
      required, for secrets a job has only once a feature is configured.
      A name cannot be in both lists.
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


def scan(names, optional, paths):
    missing, checked = needles(names + optional)
    missing = [name for name in missing if name not in optional]
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
    print(f"No secret values in {len(files)} artifact files "
          f"({len(checked)} forms of {len(names + optional)} secrets checked)")


def main():
    args = sys.argv[1:]
    if len(args) >= 4 and args[0] == "scan" and args[1] == "--secrets":
        names = [name for name in args[2].split(",") if name]
        optional, paths = [], args[3:]
        if paths[0] == "--optional":
            optional = [name for name in "".join(paths[1:2]).split(",") if name]
            paths = paths[2:] if optional else []
        if (names and paths and not set(names) & set(optional)
                and all(re.fullmatch(r"[A-Z][A-Z0-9_]*", name) for name in names + optional)):
            scan(names, optional, [pathlib.Path(p) for p in paths if pathlib.Path(p).exists()])
            return
    sys.exit(__doc__)


if __name__ == "__main__":
    main()
