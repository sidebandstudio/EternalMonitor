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
        self.assertIn(('stop-host','--kill'),calls)
        self.assertEqual(calls[-1],('pattern','stop'))
        self.assertTrue(any('Host cleanup' in error for error in result['errors']))


if __name__ == '__main__':
    unittest.main()
