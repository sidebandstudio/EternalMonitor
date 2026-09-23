import os
import pathlib
import subprocess
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


if __name__ == "__main__":
    unittest.main()
