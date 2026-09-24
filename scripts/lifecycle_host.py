#!/usr/bin/env python3
"""Own a test host and restart it only when the UI test requests that action."""
import argparse
import json
from pathlib import Path
import signal
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    args.directory.mkdir(parents=True, exist_ok=True)
    host = None
    stopped = False

    def stop_controller(*_):
        nonlocal stopped
        stopped = True

    def note(name):
        data = dict(event=name, monotonic=time.monotonic(), pid=host.pid)
        (args.directory / (name + ".json")).write_text(json.dumps(data))
        print("E2E_LIFECYCLE " + json.dumps(data), flush=True)

    signal.signal(signal.SIGTERM, stop_controller)
    signal.signal(signal.SIGINT, stop_controller)
    try:
        host = subprocess.Popen(args.command)
        note("started")
        while not stopped:
            if (args.directory / "stop.request").exists():
                (args.directory / "stop.request").unlink()
                if host.poll() is not None:
                    raise RuntimeError("The test host exited before the requested kill")
                host.kill()
                host.wait(timeout=10)
                note("stopped")
            if (args.directory / "start.request").exists():
                (args.directory / "start.request").unlink()
                if host.poll() is None:
                    raise RuntimeError("The test tried to restart a running host")
                host = subprocess.Popen(args.command)
                note("restarted")
            time.sleep(0.05)
    finally:
        if host is not None and host.poll() is None:
            host.terminate()
            try:
                host.wait(timeout=10)
            except subprocess.TimeoutExpired:
                host.kill()
                host.wait(timeout=5)


if __name__ == "__main__":
    main()
