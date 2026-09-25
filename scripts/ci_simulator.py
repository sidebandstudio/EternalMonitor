#!/usr/bin/env python3
"""Create a CI simulator, retrying CoreSimulator's transient allocation failure."""
import subprocess
import sys
import time
import uuid


def create_simulator(name, device_type, runtime):
    for attempt in range(1, 4):
        # Failed allocation can leave a device behind. A unique name makes it
        # safe to delete only the device allocated by this attempt.
        device_name = f"{name}-{uuid.uuid4()}"
        result = subprocess.run(
            ["xcrun", "simctl", "create", device_name, device_type, runtime],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            return result.stdout.strip()

        print(result.stderr, file=sys.stderr, end="")
        if "Device was allocated but was stuck in creation state" not in result.stderr:
            result.check_returncode()

        subprocess.run(
            ["xcrun", "simctl", "delete", device_name],
            stdout=sys.stderr, stderr=sys.stderr, check=False,
        )
        if attempt == 3:
            result.check_returncode()
        print(f"Retrying simulator creation after allocation failure ({attempt}/3)", file=sys.stderr)
        time.sleep(2)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit("Usage: ci_simulator.py NAME DEVICE_TYPE RUNTIME")
    print(create_simulator(*sys.argv[1:]))
