#!/usr/bin/env python3
"""Measure memory and delivered frames without hiding stalls between milestones."""
import argparse
import json
import math
from pathlib import Path

# The host's first line in its session log (host/src/main.rs).
HOST_START = "Session log file initialized"


def session_log_parts(path):
    """The host session log's parts, oldest first. host/src/logging.rs moves .1
    to .2 (dropping the old .2) and the live log to .1, at every host start and
    whenever the next write would take the live log past 2 MiB."""
    path = Path(path)
    parts = (path.with_name(path.name + ".2"), path.with_name(path.name + ".1"), path)
    return [part for part in parts if part.exists()]


def host_run_log(path):
    """The last host run's lines across its rotated parts, or None if the part it
    started in has rotated away. A start always begins a new part, so earlier
    parts hold only earlier runs."""
    texts = [part.read_text(encoding="utf-8", errors="replace") for part in session_log_parts(path)]
    starts = [i for i, text in enumerate(texts) if HOST_START in text]
    return "".join(texts[starts[-1]:]) if starts else None


def assess_host_log(text):
    """A clean soak is one session the host ends at shutdown, without a reconnect
    or a liveness expiry. Sessions held through a silence and kept are counted,
    not failed: the iPad never noticed them."""
    if text is None:
        return dict(errors=["The host session log is missing, or the part with the host's start rotated away"])
    lines = text.splitlines()
    reconnects = sum("Client reconnected" in line for line in lines)
    expiries = sum("Client liveness expired" in line for line in lines)
    holds = sum("Client silent — holding the session" in line for line in lines)
    holds_kept = sum("Client heard again — session kept" in line for line in lines)
    summaries = [line for line in lines if "session_summary v=1 role=pc " in line]
    errors = []
    if reconnects:
        errors.append(f"The host logged {reconnects} client reconnects")
    if expiries:
        errors.append(f"The host logged {expiries} client liveness expiries")
    if len(summaries) != 1 or " end=shutdown " not in summaries[0] + " ":
        errors.append(f"The host logged {len(summaries)} session summaries; a clean soak has one, ending at shutdown")
    return dict(errors=errors, reconnects=reconnects, liveness_expiries=expiries, holds=holds,
                holds_kept=holds_kept, session_summaries=len(summaries))


def assess(samples, seconds, processes=("host", "app")):
    errors = []
    if seconds < 330:
        errors.append("At least 330 seconds are needed for the minute-five memory baseline")
    if len(samples) < 2:
        return dict(status="FAIL", errors=errors + ["Resource samples are missing"])
    first, last = samples[0], samples[-1]
    duration = last["elapsed"] - first["elapsed"]
    if duration < seconds - 2:
        errors.append(f"Only {duration:.1f} of {seconds} requested seconds were sampled")
    intervals = []
    for before, after in zip(samples, samples[1:]):
        elapsed = after["elapsed"] - before["elapsed"]
        if elapsed <= 0 or elapsed > 40:
            errors.append("Resource samples must be continuous at thirty-second intervals")
            continue
        if after["decoded"] < before["decoded"]:
            errors.append("The decoded counter reset during the soak")
        # A final partial interval under five seconds adds no independent sample.
        if elapsed >= 5:
            intervals.append((after["decoded"] - before["decoded"]) / elapsed)
    good = sum(fps >= 55 for fps in intervals)
    fraction = good / len(intervals) if intervals else 0
    if fraction < .95:
        errors.append(f"Only {good}/{len(intervals)} samples reached 55 fps")
    memory = {}
    for name in processes:
        key = name + "_rss_kib"
        if any(not isinstance(s.get(key), (int, float)) or s[key] <= 0 for s in samples):
            errors.append(f"{name} memory or process identity was missing")
            continue
        baseline = next((s for s in samples if s["elapsed"] >= 300), None)
        if baseline is None or baseline is last:
            errors.append(f"{name} has no samples after its minute-five baseline")
            continue
        growth = (last[key] - baseline[key]) / baseline[key]
        memory[name] = dict(baseline_kib=baseline[key], final_kib=last[key], growth_percent=100*growth)
        if growth >= .2:
            errors.append(f"{name} RSS grew {100*growth:.2f}%, limit is below 20%")
    if len(intervals) < math.floor(seconds / 30):
        errors.append("Too few thirty-second measurements")
    return dict(status="FAIL" if errors else "PASS", errors=errors, requested_seconds=seconds,
                measured_seconds=duration, samples=len(intervals), samples_at_55_fps=good,
                fps_pass_fraction=fraction, memory=memory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("samples", type=Path)
    parser.add_argument("--seconds", type=int, required=True)
    parser.add_argument("--stream-result", type=Path, required=True)
    parser.add_argument("--host-log", type=Path, help="the host's live session log; rotated parts are read too")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    samples = [json.loads(line) for line in args.samples.read_text().splitlines()] if args.samples.exists() else []
    result = assess(samples, args.seconds)
    stream = json.loads(args.stream_result.read_text()) if args.stream_result.exists() else {}
    if stream.get("status") != "PASS":
        result["status"] = "FAIL"
        result["errors"].append("The video/pixel gate failed; inspect the stream evidence")
    result["stream"] = stream
    if args.host_log:
        host = assess_host_log(host_run_log(args.host_log))
        if host["errors"]:
            result["status"] = "FAIL"
            result["errors"] += host["errors"]
        result["host_log"] = host
    args.report.with_suffix(".json").write_text(json.dumps(result, indent=2) + "\n")
    lines = ["# EternalMonitor soak", "", f"Result: {result['status']}", "",
             f"Requested duration: {args.seconds} seconds. "
             f"Measured duration: {result.get('measured_seconds', 0):.1f} seconds.",
             f"FPS samples at or above 55: {result.get('samples_at_55_fps', 0)}/{result.get('samples', 0)}.", "",
             "| Process | RSS at minute 5, KiB | Final RSS, KiB | Growth |",
             "| --- | ---: | ---: | ---: |"]
    for name, memory in result.get("memory", {}).items():
        lines.append(f"| {name} | {memory['baseline_kib']} | {memory['final_kib']} | {memory['growth_percent']:.2f}% |")
    if "session_summaries" in result.get("host_log", {}):
        host = result["host_log"]
        lines += ["", f"Host log: {host['reconnects']} reconnects, {host['liveness_expiries']} liveness expiries, "
                  f"{host['holds']} silent-client holds ({host['holds_kept']} kept), "
                  f"{host['session_summaries']} session summaries."]
    lines += ["", f"Samples: {args.samples.resolve()}", f"Video evidence: {args.stream_result.resolve()}", ""]
    lines += ["- " + error for error in result["errors"]]
    args.report.write_text("\n".join(lines) + "\n")
    print(args.report.resolve())
    return int(result["status"] != "PASS")


if __name__ == "__main__":
    raise SystemExit(main())
