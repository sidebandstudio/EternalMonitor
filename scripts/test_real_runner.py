import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import e2e_real


class RealRunnerCleanupTests(unittest.TestCase):
    def run_failure(self, remote):
        with tempfile.TemporaryDirectory() as directory:
            with patch.dict(os.environ, {'EM_EVIDENCE_DIR':directory}), \
                    patch('sys.argv',['e2e_real.py']), patch.object(e2e_real,'remote',side_effect=remote), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(e2e_real.main(),1)
            results = [json.loads((Path(directory)/'real'/row/'result.json').read_text())
                       for row in e2e_real.SCENARIOS]
            self.assertEqual(results[0]['status'],'FAIL')
            self.assertTrue(all(r['status']=='NOT RUN' for r in results[1:]))
            return results[0]

    def test_preexisting_host_is_never_stopped(self):
        calls = []
        def remote(*args, **kwargs):
            calls.append(args)
            return '{"id":123}'
        result = self.run_failure(remote)
        self.assertEqual(calls,[('host-info',)])
        self.assertIn('already tracked',result['errors'][-1])

    def test_partial_pattern_launch_is_closed_before_stopping_matrix(self):
        calls = []
        def remote(*args, **kwargs):
            calls.append(args)
            if args==('host-info',): return 'null'
            if args==('pattern','start'): raise subprocess.CalledProcessError(1,args)
            return ''
        self.run_failure(remote)
        self.assertEqual(calls,[('host-info',),('pattern','start'),('pattern','stop')])

    def test_partial_host_launch_uses_force_cleanup_after_graceful_failure(self):
        calls = []
        def remote(*args, **kwargs):
            calls.append(args)
            if args==('host-info',): return 'null'
            if args[0]=='run-host' or args==('stop-host',):
                raise subprocess.CalledProcessError(1,args)
            return ''
        result = self.run_failure(remote)
        launched = next(call for call in calls if call[0] == 'run-host')
        self.assertIn('ETERNAL_USB_DIRECT=127.0.0.1:0', launched)
        self.assertIn(('stop-host','--kill'),calls)
        self.assertEqual(calls[-1],('pattern','stop'))
        self.assertTrue(any('Host cleanup' in error for error in result['errors']))

    def test_invalid_log_encoding_does_not_skip_cleanup(self):
        calls = []
        def remote(*args, **kwargs):
            calls.append(args)
            if args == ('host-info',): return 'null'
            if args[0] == 'run-host': raise OSError('test host launch failed')
            if args == ('log',):
                raise UnicodeDecodeError('utf-8', b'\x83', 0, 1, 'invalid byte')
            return ''
        self.run_failure(remote)
        self.assertIn(('stop-host',), calls)
        self.assertEqual(calls[-1], ('pattern', 'stop'))

    def run_input_geometry(self, size, probe_size):
        calls = []
        def remote(*args, **kwargs):
            calls.append((args, kwargs))
            if args == ('host-info',): return 'null'
            if args == ('probe-info',):
                return json.dumps(dict(x=0, y=0, width=probe_size[0], height=probe_size[1], pid=123))
            if args[0] == 'run-host': raise subprocess.CalledProcessError(1, args)
            return ''
        with tempfile.TemporaryDirectory() as directory:
            with patch.dict(os.environ, {'EM_EVIDENCE_DIR':directory, 'EM_SIZE':size}), \
                    patch('sys.argv', ['e2e_real.py', '--rows', 'R-input']), \
                    patch.object(e2e_real, 'remote', side_effect=remote), \
                    patch.object(e2e_real, 'run', return_value=directory), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(e2e_real.main(), 1)
            result = json.loads((Path(directory)/'real/R-input/result.json').read_text())
        return calls, result

    def test_input_uses_current_primary_display_geometry(self):
        calls, _ = self.run_input_geometry('3440x1440', (3440, 1440))
        args, kwargs = next(call for call in calls if call[0][0] == 'run-host')
        self.assertIn('ETERNAL_INPUT_WINDOW_PID=123', args)
        self.assertEqual(kwargs['env']['EM_SIZE'], '3440x1440')
        self.assertEqual(kwargs['env']['EM_INPUT_WIDTH'], '3440')
        self.assertEqual(kwargs['env']['EM_INPUT_HEIGHT'], '1440')
        self.assertIn(('probe', 'stop'), [call[0] for call in calls])

    def test_wrong_probe_geometry_prevents_input_host_start(self):
        calls, result = self.run_input_geometry('3440x1440', (1920, 1080))
        self.assertFalse(any(call[0][0] == 'run-host' for call in calls))
        self.assertIn('3440x1440 primary screen', result['errors'][-1])
        self.assertEqual(calls[-1][0], ('probe', 'stop'))


if __name__ == '__main__':
    unittest.main()
