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
    def test_vdd_checks_measured_stream_before_separate_host_exit_session(self):
        after_measurement = False
        states = iter((True, False, True))
        pixel_paths = []
        with tempfile.TemporaryDirectory() as directory:
            def remote(*args, **kwargs):
                if args == ('host-info',): return 'null'
                if args == ('vdd-state',):
                    return json.dumps(dict(disabled=next(states, True), settings_xml=
                        '<vdd_settings><resolutions><resolution><width>2420</width><height>1668</height>'
                        '<refresh_rate>60</refresh_rate></resolution></resolutions></vdd_settings>'))
                if args == ('log',):
                    return ('Desktop duplication active\nEncoder opened encoder="h264_nvenc"\n' +
                            'Keyframe request received\n' * (3 if after_measurement else 2))
                return ''

            def run(command, **kwargs):
                if Path(command[0]).name == 'pixels.sh': pixel_paths.append(Path(command[1]).name)
                return ''

            def host_exit(*args):
                nonlocal after_measurement
                after_measurement = True

            class Stream:
                returncode = 0
                def __init__(self, command, *, env, **kwargs):
                    self.polls = 0
                    row = Path(env['EM_OUTPUT_DIR'])
                    (row / 'app.log').write_text('E2E_HELLO w=1668 h=2420 refresh_hz=60\nw=2420 h=1668')
                    (row / 'result.json').write_text(json.dumps(dict(status='PASS', measured_seconds=20)))
                def poll(self):
                    self.polls += 1
                    return None if self.polls == 1 else 0

            with patch.dict(os.environ, {'EM_EVIDENCE_DIR': directory}), \
                    patch('sys.argv', ['e2e_real.py', '--rows', 'R-vdd']), \
                    patch.object(e2e_real, 'remote', side_effect=remote), \
                    patch.object(e2e_real, 'run', side_effect=run), \
                    patch.object(e2e_real, 'verify_vdd_host_exit', side_effect=host_exit), \
                    patch.object(e2e_real.time, 'sleep'), \
                    patch.object(e2e_real.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1)), \
                    patch.object(e2e_real.subprocess, 'Popen', Stream), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(e2e_real.main(), 0)
            row = Path(directory) / 'real/R-vdd'
            self.assertEqual(json.loads((row / 'result.json').read_text())['keyframe_requests'], 2)
            self.assertEqual((row / 'host.log').read_text().count('Keyframe request received'), 3)
            self.assertEqual((row / 'host-measurement.log').read_text().count('Keyframe request received'), 2)
        self.assertEqual(pixel_paths, ['R-vdd-connected.png'])

    def test_pairing_build_does_not_skip_first_release_stream_build(self):
        builds = []
        with tempfile.TemporaryDirectory() as directory:
            container = Path(directory) / 'container'
            (container / 'tmp').mkdir(parents=True)
            (container / 'tmp/eternal-e2e.log').write_text('pairing evidence')

            def complete(env):
                (Path(env['EM_OUTPUT_DIR']) / 'result.json').write_text(
                    json.dumps(dict(scenario=env['EM_SCENARIO'], status='PASS', errors=[])))

            def run(command, *, env=None, **kwargs):
                if Path(command[0]).name == 'e2e_pairing.sh':
                    complete(env)
                return str(container)

            def remote(*args, **kwargs):
                if args == ('host-info',): return 'null'
                if args == ('log',): return 'pairing_code=123456'
                return ''

            class Stream:
                returncode = 0
                def __init__(self, command, *, env, **kwargs):
                    builds.append(env['EM_SKIP_BUILD'])
                    complete(env)
                def poll(self): return 0

            with patch.dict(os.environ, {'EM_EVIDENCE_DIR': directory}), \
                    patch('sys.argv', ['e2e_real.py', '--rows', 'R-pairing', 'R-baseline', 'R-nvenc-h264']), \
                    patch.object(e2e_real, 'remote', side_effect=remote), \
                    patch.object(e2e_real, 'run', side_effect=run), \
                    patch.object(e2e_real, 'check_stream'), \
                    patch.object(e2e_real.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1)), \
                    patch.object(e2e_real.subprocess, 'Popen', Stream), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(e2e_real.main(), 0)
        self.assertEqual(builds, ['0', '1'])

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
