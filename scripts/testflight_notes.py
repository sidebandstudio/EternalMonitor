#!/usr/bin/env python3
"""Keep the current TestFlight test notes tied to the release notes."""
import argparse
from pathlib import Path
import re


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    version = re.search(r'MARKETING_VERSION:\s*"([^"]+)"', (root / "ios/project.yml").read_text())[1]
    source = (root / "RELEASE_NOTES.md").read_text()
    section = re.search(rf'(?ms)^## EternalMonitor v{re.escape(version)}\s*\n(.*?)(?=^## EternalMonitor |\Z)', source)
    if not section:
        raise SystemExit(f"No release notes for {version}")
    notes = f"# EternalMonitor {version}: What to Test\n\n" + section[1].strip() + "\n"
    if len(notes) > 4000:
        raise SystemExit("TestFlight notes exceed 4,000 characters")
    output = root / "ios/TESTFLIGHT_NOTES.md"
    if args.check:
        if not output.exists() or output.read_text() != notes:
            raise SystemExit("Regenerate ios/TESTFLIGHT_NOTES.md with scripts/testflight_notes.py")
    else:
        output.write_text(notes)
    print(f"TestFlight notes: {version}, {len(notes)} characters")


if __name__ == "__main__":
    main()
