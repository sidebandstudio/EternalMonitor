import importlib.util
import os
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('windows_remote', Path(__file__).parent / 'win/remote.py')
remote = importlib.util.module_from_spec(spec)
spec.loader.exec_module(remote)

class AvailabilityTests(unittest.TestCase):
    def test_idle_guard_is_default(self):
        with patch.dict(os.environ, {}, clear=True), patch.object(remote, 'ps') as call:
            remote.session('test', idle=True)
        self.assertIn('-RequireIdle', call.call_args.args[0])

    def test_explicit_availability_only_skips_idle_guard(self):
        with patch.dict(os.environ, {'EM_PC_AVAILABLE': '1'}, clear=True), patch.object(remote, 'ps') as call:
            remote.session('test', idle=True, run_level='Limited', timeout=30)
        command = call.call_args.args[0]
        self.assertNotIn('-RequireIdle', command)
        self.assertIn('-RunLevel Limited', command)
        self.assertIn('-TimeoutSec 30', command)

    def test_other_values_do_not_skip_idle_guard(self):
        with patch.dict(os.environ, {'EM_PC_AVAILABLE': 'yes'}, clear=True), patch.object(remote, 'ps') as call:
            remote.session('test', idle=True)
        self.assertIn('-RequireIdle', call.call_args.args[0])

class InstalledHostTests(unittest.TestCase):
    def test_selected_installed_path_is_used(self):
        path = r'C:\Program Files\EternalMonitor\EternalMonitor-host.exe'
        with patch.dict(os.environ, {'EM_INSTALLED_HOST': '1', 'EM_INSTALLED_HOST_PATH': path}, clear=True), \
                patch.object(remote, 'ps'), patch.object(remote, 'session') as launch:
            remote.main(['run-host'])
        self.assertIn("-FilePath '" + path + "'", launch.call_args.args[0])

    def test_installed_path_override_does_not_change_source_build_runs(self):
        with patch.dict(os.environ, {'EM_INSTALLED_HOST_PATH': r'C:\wrong.exe'}, clear=True), \
                patch.object(remote, 'ps'), patch.object(remote, 'session') as launch:
            remote.main(['run-host'])
        self.assertIn(remote.REPO + r'\target\release\eternal-host.exe', launch.call_args.args[0])
        self.assertNotIn('wrong.exe', launch.call_args.args[0])

if __name__ == '__main__':
    unittest.main()
