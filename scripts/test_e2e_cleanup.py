import os
import pathlib
import subprocess
import tempfile
import time
import unittest

SCRIPT = pathlib.Path(__file__).with_name("e2e_ios.sh")


def run_helpers(body):
    text = SCRIPT.read_text()
    helpers = text[text.index("bounded() {"):text.index("cleanup() {")]
    return subprocess.run(["bash", "-c", helpers + body], capture_output=True, text=True,
                          timeout=30)


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class SimulatorCleanup(unittest.TestCase):
    def test_bounded_stops_a_stalled_command(self):
        started = time.monotonic()
        result = run_helpers('bounded 1 sleep 30; echo "status=$?"')
        self.assertIn("status=124", result.stdout)
        self.assertLess(time.monotonic() - started, 10)

    def test_bounded_keeps_the_command_status(self):
        self.assertIn("status=3", run_helpers('bounded 5 sh -c "exit 3"; echo "status=$?"').stdout)

    def test_stop_app_signals_only_the_launched_app_in_this_simulator(self):
        app = "/Users/ci/Library/Developer/CoreSimulator/Devices/SIM-A/data/EternalMonitor.app/EternalMonitor"
        # Detach like a simulator app so its real parent reaps it after the signal.
        pid = int(subprocess.run(["bash", "-c", 'exec -a "$1" sleep 30 >/dev/null 2>&1 & echo $!', "bash", app],
                                 capture_output=True, text=True, check=True).stdout)
        try:
            other = run_helpers(f'UDID=SIM-B APP_PID={pid}; xcrun() {{ echo "fallback $*"; }}; stop_app')
            self.assertIn("fallback simctl terminate SIM-B com.eternal.monitor", other.stdout)
            self.assertTrue(alive(pid))
            launched = run_helpers(f'UDID=SIM-A APP_PID={pid}; xcrun() {{ echo fallback; }}; stop_app')
            self.assertNotIn("fallback", launched.stdout)
            deadline = time.monotonic() + 5
            while alive(pid) and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertFalse(alive(pid))
        finally:
            if alive(pid):
                os.kill(pid, 9)

    def test_cleanup_stops_the_host_before_the_app(self):
        # An app stopped first leaves the host to expire its liveness.
        text = SCRIPT.read_text()
        script = text[text.index("bounded() {"):text.index("trap cleanup EXIT")]
        with tempfile.TemporaryDirectory() as root:
            events = pathlib.Path(root, "events")
            remote = pathlib.Path(root, "scripts/win/remote.sh")
            remote.parent.mkdir(parents=True)
            remote.write_text(f'#!/bin/sh\necho "remote $*" >> {events}\n')
            remote.chmod(0o755)
            body = f"""
ROOT={root} OUT={root} APP_LOG={root}/app.log UDID=SIM-A EM_STOP_REMOTE_HOST=1
SOAK_PID= PROFILE_PID= MONITOR_PID= PROXY_PID= APP_PID= INSTALL_DIR= SCENARIO=s STARTED=$SECONDS
bash -c 'trap "echo host stopped >> {events}; exit 0" TERM; while :; do sleep 0.1; done' &
HOST_PID=$!
sleep 0.5
xcrun() {{ echo "app $*" >> {events}; }}
cleanup
"""
            started = time.monotonic()
            result = subprocess.run(["bash", "-c", script + body], capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertLess(time.monotonic() - started, 5)
            self.assertEqual(events.read_text().splitlines(),
                             ["host stopped", "remote stop-host", "app simctl terminate SIM-A com.eternal.monitor"])


if __name__ == "__main__":
    unittest.main()
