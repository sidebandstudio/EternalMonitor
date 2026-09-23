#!/usr/bin/env python3
"""Measure streaming time from app milestones, excluding build and startup."""
import argparse
import json
from pathlib import Path
import re


def measure(log, link=None):
    samples = []
    active_link = None
    selected_sessions = 0
    switch = None
    switch_ms = None
    for line in log.splitlines():
        fields = dict(re.findall(r"(\w+)=(\w+)", line))
        if "E2E_LINK_SWITCH" in line:
            switch = fields
        if "E2E_FIRST_FRAME" in line:
            active_link = fields.get("link", "udp")
            if active_link == link:
                selected_sessions += 1
                if selected_sessions > 1:
                    raise ValueError("multiple sessions on the measured link")
                if switch and switch.get("to") == link:
                    switch_ms = int(fields["monotonic_ms"]) - int(switch["monotonic_ms"])
        if "E2E_STATS" in line and (link is None or active_link == link):
            samples.append({key: int(value) for key, value in fields.items() if value.isdigit()})
    if not samples:
        return None
    first, last = samples[0], samples[-1]
    # A reconnect resets decoded. Never combine two sessions into a passing row.
    if any(b["decoded"] < a["decoded"] for a, b in zip(samples, samples[1:])):
        raise ValueError("decoded counter reset during the measurement")
    duration = (last.get("monotonic_ms", 0) - first.get("monotonic_ms", 0)) / 1000
    return dict(last, link=link, switch_ms=switch_ms, measured_seconds=round(duration, 3),
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
    # `record` keeps the measured FPS in the result without failing on it,
    # for runners too small to encode and decode in software at 55 FPS.
    parser.add_argument("--fps-gate", choices=["enforce", "record"], default="enforce")
    parser.add_argument("--max-drop-ratio", type=float, default=0.02)
    parser.add_argument("--require-repairs", type=int, default=0)
    parser.add_argument("--link", choices=["udp", "usb"])
    parser.add_argument("--max-switch-ms", type=int)
    args = parser.parse_args()
    result = measure(args.log.read_text(), link=args.link)
    if result is None or result["decoded"] < args.min_frames or result["measured_seconds"] < args.duration:
        return 1
    if args.output:
        errors = []
        notes = []
        if result["average_fps"] < args.min_fps:
            message = f"average FPS {result['average_fps']} is below {args.min_fps}"
            (errors if args.fps_gate == "enforce" else notes).append(message)
        if result["dropped"] / result["decoded"] >= args.max_drop_ratio:
            errors.append(f"dropped {result['dropped']} of {result['decoded']} decoded frames")
        if args.require_repairs and result["repaired"] == 0:
            errors.append("loss row did not exercise retransmission repair")
        if args.max_switch_ms is not None:
            delay = result["switch_ms"]
            if delay is None or not 0 <= delay <= args.max_switch_ms:
                errors.append(f"USB takeover took {delay} ms; expected at most {args.max_switch_ms} ms")
        result.update(status="FAIL" if errors else "PASS", errors=errors, notes=notes,
                      fps_gate=args.fps_gate,
                      scenario=args.scenario, screenshot=args.screenshot, elapsed=args.elapsed)
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        if errors:
            raise ValueError("; ".join(errors))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
