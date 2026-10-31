#!/usr/bin/env python3
"""Keep the current TestFlight test notes tied to the release notes."""
import argparse
from pathlib import Path
import re

LIMIT = 4000  # App Store Connect's limit for What to Test


def notes(version, source):
    """The TestFlight notes for `version` from RELEASE_NOTES.md's text.

    The "Released …" sentence is left out: testers get the build when it is
    uploaded, and before a tag that sentence still holds a placeholder."""
    section = re.search(rf'(?ms)^## EternalMonitor v{re.escape(version)}\s*\n(.*?)(?=^## EternalMonitor |\Z)', source)
    if not section:
        raise SystemExit(f"No release notes for {version}")
    body = re.sub(r'\AReleased [^.]*\.\s*', '', section[1].strip())
    text = f"# EternalMonitor {version}: What to Test\n\n" + body + "\n"
    if len(text) > LIMIT:
        raise SystemExit(f"TestFlight notes exceed {LIMIT:,} characters")
    return text


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    version = re.search(r'MARKETING_VERSION:\s*"([^"]+)"', (root / "ios/project.yml").read_text())[1]
    text = notes(version, (root / "RELEASE_NOTES.md").read_text())
    output = root / "ios/TESTFLIGHT_NOTES.md"
    if args.check:
        if not output.exists() or output.read_text() != text:
            raise SystemExit("Regenerate ios/TESTFLIGHT_NOTES.md with scripts/testflight_notes.py")
    else:
        output.write_text(text)
    print(f"TestFlight notes: {version}, {len(text)} characters")


if __name__ == "__main__":
    main()
