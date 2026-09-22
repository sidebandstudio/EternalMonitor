#!/usr/bin/env python3
"""Sample only the processes owned by the current stream fixture."""
import argparse
import json
from pathlib import Path
import re
import signal
import subprocess
import threading
import time


def rss(pid):
    value = subprocess.check_output(["ps", "-p", str(pid), "-o", "rss="], text=True).strip()
    return int(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host-pid", type=int)
    parser.add_argument("--remote", type=Path)
    parser.add_argument("--app-pid", type=int, required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    stopped = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stopped.set())
    signal.signal(signal.SIGINT, lambda *_: stopped.set())
    # Start the clock when video arrives, retaining the first interval's startup.
    deadline = time.monotonic() + 120
    while not stopped.is_set():
        if args.log.exists() and "E2E_FIRST_FRAME" in args.log.read_text(errors="replace"):
            break
        if time.monotonic() >= deadline:
            raise RuntimeError("No first frame before the soak deadline")
        stopped.wait(.1)
    started = time.monotonic()
    next_sample = started
    with args.output.open("w") as output:
        while True:
            sample = dict(elapsed=time.monotonic() - started)
            try:
                sample["app_rss_kib"] = rss(args.app_pid)
                if args.remote:
                    host = json.loads(subprocess.check_output([str(args.remote), "rss"], text=True))
                    sample["host_rss_kib"] = host["rss_kib"]
                else:
                    sample["host_rss_kib"] = rss(args.host_pid)
                log = args.log.read_text(errors="replace")
                counts = re.findall(r"E2E_STATS decoded=(\d+)", log)
                sample["decoded"] = int(counts[-1]) if counts else 0
            except (ValueError, OSError, subprocess.CalledProcessError) as error:
                sample["error"] = str(error)
                sample.setdefault("decoded", 0)
            output.write(json.dumps(sample) + "\n")
            output.flush()
            if stopped.is_set():
                break
            next_sample += 30
            stopped.wait(max(0, next_sample - time.monotonic()))


if __name__ == "__main__":
    main()
