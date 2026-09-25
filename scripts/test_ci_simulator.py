import subprocess
import unittest
from unittest.mock import patch

from ci_simulator import create_simulator


class SimulatorCreationTests(unittest.TestCase):
    def result(self, code=0, stdout="", stderr=""):
        return subprocess.CompletedProcess(["xcrun", "simctl"], code, stdout, stderr)

    def allocation_failure(self):
        return self.result(22, stderr="Device was allocated but was stuck in creation state.\n")

    @patch("ci_simulator.time.sleep")
    @patch("ci_simulator.subprocess.run")
    def test_retries_allocation_failure_and_cleans_only_its_device(self, run, sleep):
        run.side_effect = [self.allocation_failure(), self.result(), self.result(stdout="new-udid\n")]
        self.assertEqual(create_simulator("citest", "ipad", "ios"), "new-udid")
        create, delete, retry = [call.args[0] for call in run.call_args_list]
        self.assertEqual(delete, ["xcrun", "simctl", "delete", create[3]])
        self.assertNotEqual(create[3], retry[3])
        self.assertEqual(retry[4:], ["ipad", "ios"])
        sleep.assert_called_once_with(2)

    @patch("ci_simulator.time.sleep")
    @patch("ci_simulator.subprocess.run")
    def test_stops_after_three_allocation_failures(self, run, sleep):
        run.side_effect = [self.allocation_failure(), self.result()] * 3
        with self.assertRaises(subprocess.CalledProcessError):
            create_simulator("citest", "ipad", "ios")
        self.assertEqual(run.call_count, 6)
        self.assertEqual(sleep.call_count, 2)

    @patch("ci_simulator.time.sleep")
    @patch("ci_simulator.subprocess.run")
    def test_other_errors_fail_without_retry_or_deletion(self, run, sleep):
        run.return_value = self.result(1, stderr="Invalid runtime: ios\n")
        with self.assertRaises(subprocess.CalledProcessError):
            create_simulator("citest", "ipad", "ios")
        run.assert_called_once()
        sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main()
