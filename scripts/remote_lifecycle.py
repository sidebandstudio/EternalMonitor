#!/usr/bin/env python3
"""Mirror the owned Windows host log and service XCTest restart requests."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import time

REMOTE = Path(__file__).resolve().parent / 'win/remote.sh'


def command(*args):
    return subprocess.check_output([str(REMOTE), *args], text=True, timeout=45)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('log', type=Path)
    parser.add_argument('host_args', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    stopped = False
    prefix = ''
    current = ''
    running = True
    pid = json.loads(command('host-info'))['id']
    args.directory.mkdir(parents=True, exist_ok=True)

    def stop(*_):
        nonlocal stopped
        stopped = True

    def note(name):
        nonlocal prefix
        value = dict(event=name, monotonic=time.monotonic(), pid=pid)
        (args.directory / (name + '.json')).write_text(json.dumps(value))
        prefix += 'E2E_LIFECYCLE ' + json.dumps(value) + '\n'

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    note('started')
    try:
        while not stopped:
            if (args.directory / 'stop.request').exists():
                if not running:
                    raise ValueError('The real host was already stopped')
                command('stop-host', '--kill')
                running = False
                prefix += current
                current = ''
                note('stopped')
                (args.directory / 'stop.request').unlink()
            if (args.directory / 'start.request').exists():
                if running:
                    raise ValueError('Cannot restart an already running host')
                command('run-host', *args.host_args)
                pid = json.loads(command('host-info'))['id']
                running = True
                note('restarted')
                (args.directory / 'start.request').unlink()
            if running:
                current = command('log')
            # Replace atomically so XCTest never sees a partially copied log.
            temp = args.log.with_suffix('.tmp')
            temp.write_text(prefix + current)
            os.replace(temp, args.log)
            time.sleep(.2)
    except Exception as error:
        (args.directory / 'error.txt').write_text(str(error))
        raise
    finally:
        args.log.write_text(prefix + current)


if __name__ == '__main__':
    main()
