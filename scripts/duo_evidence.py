#!/usr/bin/env python3
"""Prepare Duo CI evidence for a public artifact. This file lives only on
feat/iphone-duo.

  duo_evidence.py screens SHOTS_DIR OUT_DIR
      Copy the duo-* screenshots test_ios.sh exported, named after their
      attachments, keeping only the chunks that draw pixels.
  duo_evidence.py scan PATH...
      Fail when any file under PATH holds a secret value from the job's
      environment. Prints names, never values.

The repository is public and artifact files are not masked like the job log.
"""
import json
import os
import pathlib
import re
import struct
import sys

SECRETS = ["ETERNAL_DIAGNOSTICS_ENDPOINT", "ETERNAL_DIAGNOSTICS_INGEST_KEY", "ASC_KEY_ID", "ASC_ISSUER_ID",
           "ASC_KEY_P8", "IOS_DIST_P12_BASE64", "IOS_DIST_P12_PASSWORD", "IOS_DUO_PROVISION_PROFILE_BASE64"]
# Text, EXIF and other metadata chunks are dropped; these only draw pixels.
PIXEL_CHUNKS = {b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS"}


def pixel_png(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit("Not a PNG")
    kept, offset = [data[:8]], 8
    while offset < len(data):
        length, kind = struct.unpack(">I4s", data[offset:offset + 8])
        end = offset + 12 + length
        if kind in PIXEL_CHUNKS:
            kept.append(data[offset:end])
        offset = end
    return b"".join(kept)


def screens(shots, out):
    out.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((shots / "manifest.json").read_text())
    names = []
    for test in manifest:
        for item in test.get("attachments", []):
            name = item.get("suggestedHumanReadableName", "")
            match = re.match(r"(duo-[a-z0-9-]+)_", name)
            if match and item["exportedFileName"].endswith(".png"):
                target = out / f"{match[1]}.png"
                target.write_bytes(pixel_png((shots / item["exportedFileName"]).read_bytes()))
                names.append(target.name)
    if not names:
        sys.exit("No duo-* screenshots in " + str(shots))
    print(f"{len(names)} screenshots: " + ", ".join(sorted(names)))


def scan(paths):
    needles = []
    for name in SECRETS:
        value = os.environ.get(name, "").strip()
        if len(value) < 6:
            continue
        needles += [(name, value), (name + " prefix", value[:12])]
        host = re.match(r"https?://([^/:]+)", value)
        if host:
            needles.append((name + " host", host[1]))
    files = [f for p in paths for f in ([p] if p.is_file() else sorted(p.rglob("*"))) if f.is_file()]
    found = []
    for path in files:
        data = path.read_bytes()
        found += [f"{path}: {name}" for name, value in needles if value.encode() in data]
    if found:
        sys.exit("Evidence contains secret values; nothing was uploaded:\n" + "\n".join(found))
    print(f"No secret values in {len(files)} evidence files ({len(needles)} values checked)")


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "screens":
        screens(pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3]))
    elif len(sys.argv) >= 3 and sys.argv[1] == "scan":
        scan([pathlib.Path(p) for p in sys.argv[2:] if pathlib.Path(p).exists()])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
