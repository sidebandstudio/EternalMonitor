#!/usr/bin/env python3
"""Measure streaming time from app milestones, excluding build and startup."""
import argparse
import json
from pathlib import Path
import re


def measure(log):
    samples = [dict((key, int(value)) for key, value in
                    re.findall(r"(\w+)=(\d+)", line.split("E2E_STATS", 1)[1]))
               for line in log.splitlines() if "E2E_STATS" in line]
    if not samples:
        return None
    first, last = samples[0], samples[-1]
    # A reconnect resets decoded. Never combine two sessions into a passing row.
    if any(b["decoded"] < a["decoded"] for a, b in zip(samples, samples[1:])):
        raise ValueError("decoded counter reset during the measurement")
    duration = (last.get("monotonic_ms", 0) - first.get("monotonic_ms", 0)) / 1000
    return dict(last, measured_seconds=round(duration, 3),
                average_fps=round((last["decoded"] - first["decoded"]) / duration, 2)
                if duration > 0 else last["fps"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--duration", type=float, default=0)
    parser.add_argument("--min-frames", type=int, default=120)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--scenario")
    parser.add_argument("--screenshot")
    parser.add_argument("--elapsed", type=int)
    parser.add_argument("--min-fps", type=float, default=55)
    parser.add_argument("--max-drop-ratio", type=float, default=0.02)
    parser.add_argument("--require-repairs", type=int, default=0)
    args = parser.parse_args()
    result = measure(args.log.read_text())
    if result is None or result["decoded"] < args.min_frames or result["measured_seconds"] < args.duration:
        return 1
    if args.output:
        errors = []
        if result["average_fps"] < args.min_fps:
            errors.append(f"average FPS {result['average_fps']} is below {args.min_fps}")
        if result["dropped"] / result["decoded"] >= args.max_drop_ratio:
            errors.append(f"dropped {result['dropped']} of {result['decoded']} decoded frames")
        if args.require_repairs and result["repaired"] == 0:
            errors.append("loss row did not exercise retransmission repair")
        result.update(status="FAIL" if errors else "PASS", errors=errors,
                      scenario=args.scenario, screenshot=args.screenshot, elapsed=args.elapsed)
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        if errors:
            raise ValueError("; ".join(errors))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
