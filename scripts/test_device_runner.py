import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import e2e_device

MILESTONES = ['E2E_DECODER kind=hw', 'E2E_FIRST_FRAME w=3440 h=1440 link={link} monotonic_ms=1000'] + [
    f'E2E_STATS decoded={60 * i} w=3440 h=1440 fps=60 dropped=0 repaired={5 * i} nacks={i} '
    f'jitter_us=100 monotonic_ms={1000 + 1000 * i}' for i in range(1, 24)]
HOST_LOG = ('Desktop duplication active\nEncoder opened bitrate=15000000 encoder="h264_nvenc"\n'
            'USB tunnel connected id=Usb\nretransmits=40\n')


class FakeApp:
    def __init__(self, stdout, lines):
        stdout.write('\n'.join(lines) + '\n')
        stdout.flush()
        self.signals = []

    def poll(self):
        return None

    def send_signal(self, signal):
        self.signals.append(signal)

    def wait(self, timeout=None):
        return 0


class DeviceRunnerTests(unittest.TestCase):
    def run_row(self, name, milestones, host_log=HOST_LOG):
        calls = []
        apps = []

        def remote(*args, output=None, timeout=300):
            calls.append(args)
            if output is not None:
                Path(output).write_text('')
            return {('host-info',): 'null', ('log',): host_log}.get(args, '')

        def popen(command, **kwargs):
            link = 'usb' if '-allowUSB' in command and command[command.index('-allowUSB') + 1] == 'YES' else 'udp'
            apps.append(FakeApp(kwargs['stdout'], [line.format(link=link) for line in milestones]))
            apps[-1].command = command
            return apps[-1]

        clock = iter(range(0, 10_000))
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(e2e_device, 'remote', side_effect=remote), \
                patch.object(e2e_device.subprocess, 'Popen', side_effect=popen), \
                patch.object(e2e_device.time, 'monotonic', side_effect=lambda: next(clock)), \
                patch.object(e2e_device.time, 'sleep'):
            args = argparse.Namespace(device='ipad', pc='10.0.0.45', size='3440x1440', duration=20)
            passed = e2e_device.run_row(name, e2e_device.ROWS[name], Path(directory), args)
            result = json.loads((Path(directory) / name / 'result.json').read_text())
        return passed, result, calls, apps

    def test_wifi_loss_row_passes_on_hardware_decode_with_repairs(self):
        passed, result, calls, apps = self.run_row('D-loss3-wifi', MILESTONES)
        self.assertTrue(passed, result)
        host = next(call for call in calls if call[0] == 'run-host')
        self.assertIn('ETERNAL_DROP=0.03', host)
        self.assertIn('ETERNAL_USB_DIRECT=127.0.0.1:0', host)
        environment = json.loads(apps[0].command[apps[0].command.index('--environment-variables') + 1])
        self.assertEqual(environment['EM_AUTOCONNECT'], '10.0.0.45:19876')
        self.assertEqual(result['host_retransmits'], 40)
        self.assertIn(('stop-host',), calls)
        self.assertIn(('pattern', 'stop'), calls)
        self.assertTrue(apps[0].signals, 'the app must be stopped')

    def test_software_decode_and_keyframe_storm_fail(self):
        milestones = [line for line in MILESTONES if 'E2E_DECODER' not in line]
        storm = HOST_LOG + 'Keyframe request received\n' * 3
        passed, result, _, _ = self.run_row('D-h264-wifi', milestones, storm)
        self.assertFalse(passed)
        self.assertTrue(any('hardware decoder' in e for e in result['errors']), result)
        self.assertTrue(any('Keyframe request storm' in e for e in result['errors']), result)

    def test_usb_row_measures_only_the_cable_and_needs_the_tunnel(self):
        passed, result, calls, apps = self.run_row('D-h264-usb', MILESTONES, HOST_LOG.replace('USB tunnel connected id=Usb\n', ''))
        self.assertFalse(passed)
        self.assertIn('The host never opened the USB tunnel', result['errors'])
        host = next(call for call in calls if call[0] == 'run-host')
        self.assertNotIn('ETERNAL_USB_DIRECT=127.0.0.1:0', host)
        self.assertNotIn('EM_AUTOCONNECT', apps[0].command[apps[0].command.index('--environment-variables') + 1])


if __name__ == '__main__':
    unittest.main()
