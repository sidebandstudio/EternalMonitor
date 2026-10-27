#!/usr/bin/env python3
"""Prepare TestFlight build evidence for a public artifact.

  testflight_evidence.py tail LOG OUT
      Write the last 70 lines of LOG to OUT with every secret value from the
      job's environment replaced by ***.
  testflight_evidence.py scan PATH...
      Fail when any file under PATH holds a secret value from the job's
      environment. Prints names, never values.

The repository is public and artifact files are not masked like the job log.
"""
import os
import pathlib
import re
import sys

SECRETS = ["ETERNAL_DIAGNOSTICS_ENDPOINT", "ETERNAL_DIAGNOSTICS_INGEST_KEY", "ASC_KEY_ID", "ASC_ISSUER_ID",
           "ASC_KEY_P8", "IOS_DIST_P12_BASE64", "IOS_DIST_P12_PASSWORD", "IOS_PROVISION_PROFILE_BASE64"]
TAIL_LINES = 70


def needles():
    found = []
    for name in SECRETS:
        value = os.environ.get(name, "").strip()
        if len(value) < 6:
            continue
        found += [(name, value), (name + " prefix", value[:12])]
        host = re.match(r"https?://([^/:]+)", value)
        if host:
            found.append((name + " host", host[1]))
    return found


def tail(log, out):
    lines = log.read_bytes().splitlines(keepends=True)[-TAIL_LINES:] if log.is_file() else []
    text = b"".join(lines)
    # Longest first, so a prefix or host never splits a full value.
    for _, value in sorted(needles(), key=lambda item: -len(item[1])):
        text = text.replace(value.encode(), b"***")
    out.write_bytes(text)
    print(f"Kept the last {len(lines)} lines of {log.name}")


def scan(paths):
    checked = needles()
    files = [f for p in paths for f in ([p] if p.is_file() else sorted(p.rglob("*"))) if f.is_file()]
    found = []
    for path in files:
        data = path.read_bytes()
        found += [f"{path}: {name}" for name, value in checked if value.encode() in data]
    if found:
        sys.exit("Evidence contains secret values; nothing was uploaded:\n" + "\n".join(found))
    print(f"No secret values in {len(files)} evidence files ({len(checked)} values checked)")


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "tail":
        tail(pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3]))
    elif len(sys.argv) >= 3 and sys.argv[1] == "scan":
        scan([pathlib.Path(p) for p in sys.argv[2:] if pathlib.Path(p).exists()])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
