#!/usr/bin/env python3
"""Stream from the reference PC to the physical iPad and measure the result.

The PC host runs in the console session through scripts/win/remote.sh. The
app runs on a CoreDevice-paired iPad, launched with devicectl; with
OS_ACTIVITY_DT_MODE its E2E milestones arrive on the launch console. WiFi rows
use the real LAN, so they measure the network the product actually runs on.
"""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time

from e2e_stats import measure
from real_checks import check_high_refresh, check_stream, clean

ROOT = Path(__file__).resolve().parent.parent
REMOTE = ROOT / 'scripts/win/remote.sh'
ROWS = {
    'D-h264-wifi': dict(link='udp'),
    'D-hevc-wifi': dict(link='udp', hevc=True),
    'D-hevc-amf-wifi': dict(link='udp', hevc=True, family='amf'),
    'D-loss3-wifi': dict(link='udp', repairs=True, host=['ETERNAL_DROP=0.03', 'ETERNAL_REORDER=0.01']),
    'D-burst-wifi': dict(link='udp', bitrate=40, host=['ETERNAL_ABR=0', 'ETERNAL_FORCE_IDR_PERIOD=60']),
    'D-h264-usb': dict(link='usb'),
    'D-hevc-usb': dict(link='usb', hevc=True),
    'D-fps120-usb': dict(link='usb', fps=120),
}


def remote(*args, output=None, timeout=300):
    if output is None:
        return subprocess.run([str(REMOTE), *args], check=True, timeout=timeout,
                              text=True, stdout=subprocess.PIPE).stdout
    with open(output, 'w') as log:
        subprocess.run([str(REMOTE), *args], check=True, timeout=timeout,
                       text=True, stdout=log, stderr=subprocess.STDOUT)


def save(path, result):
    path.write_text(json.dumps(result, indent=2) + '\n')


def run_row(name, spec, out, args):
    row = out / name
    row.mkdir(parents=True, exist_ok=True)
    result_path = row / 'result.json'
    save(result_path, dict(scenario=name, status='FAIL', errors=['Run did not finish']))
    fps = spec.get('fps', 60)
    codec = 'hevc' if spec.get('hevc') else 'h264'
    family = spec.get('family', 'nvenc')
    host = ['ETERNAL_HEADLESS=1', f'ETERNAL_ENCODER=h264_{family}', f'ETERNAL_HEVC={int(codec == "hevc")}',
            f'ETERNAL_FPS={fps}', 'ETERNAL_E2E_LOG=1', *spec.get('host', [])]
    if os.environ.get('EM_HOST_RUST_LOG'):
        host.append('RUST_LOG=' + os.environ['EM_HOST_RUST_LOG'])
    launch = {'EM_E2E_LOG': '1', 'OS_ACTIVITY_DT_MODE': 'YES'}
    if spec['link'] == 'udp':
        # The cable stays attached; keep the host's USB supervisor off it.
        host.append('ETERNAL_USB_DIRECT=127.0.0.1:0')
        launch['EM_AUTOCONNECT'] = f'{args.pc}:19876'
    env = dict(os.environ, EM_BITRATE_MBPS=str(spec.get('bitrate', 15)), EM_REQUIRE_PAIRING='0', EM_AUDIO='0')
    os.environ.update(env)
    app = console = None
    host_started = pattern_started = False
    result = json.loads(result_path.read_text())
    try:
        if json.loads(remote('host-info')) is not None:
            raise ValueError('Another host run is already tracked; finish it first')
        pattern_started = True
        remote('pattern', 'start', output=row / 'pattern-start.log')
        host_started = True
        remote('run-host', *host, output=row / 'host-start.log')
        console = (row / 'app.log').open('w')
        app = subprocess.Popen(
            ['xcrun', 'devicectl', 'device', 'process', 'launch', '--device', args.device,
             '--terminate-existing', '--console', '--environment-variables', json.dumps(launch),
             'com.eternal.monitor', '--', '-didSeeOnboarding', 'YES',
             '-allowUSB', 'YES' if spec['link'] == 'usb' else 'NO', '-playPCaudio', 'NO',
             '-targetFPS', str(fps)],
            env=dict(os.environ, DEVELOPER_DIR='/Applications/Xcode.app/Contents/Developer'),
            stdout=console, stderr=subprocess.STDOUT)
        app_log = row / 'app.log'
        deadline = time.monotonic() + 90
        started = None
        while True:
            text = app_log.read_text(errors='replace')
            if started is None and re.search(rf'E2E_FIRST_FRAME .*link={spec["link"]}', text):
                started = time.monotonic()
            if started is not None and time.monotonic() - started >= args.duration + 2:
                break
            if app.poll() is not None:
                raise ValueError('The iPad app exited during the row')
            if started is None and time.monotonic() > deadline:
                raise TimeoutError(f'No {spec["link"]} frame within 90 s')
            time.sleep(0.5)
        milestones = '\n'.join(line for line in text.splitlines() if 'E2E_' in line)
        (row / 'milestones.log').write_text(milestones + '\n')
        measured = measure(milestones, link=spec['link'])
        if measured is None:
            raise ValueError('No E2E_STATS samples on the measured link')
        result = dict(measured, scenario=name, status='PASS', errors=[], notes=[], link=spec['link'])
        errors = result['errors']
        if 'E2E_DECODER kind=hw' not in milestones:
            errors.append('The iPad did not report its hardware decoder')
        width, height = map(int, args.size.split('x'))
        if (result.get('w'), result.get('h')) != (width, height):
            errors.append(f'Decoded {result.get("w")}x{result.get("h")}, expected {args.size}')
        if result['measured_seconds'] < args.duration:
            errors.append(f'Measured {result["measured_seconds"]} s, expected {args.duration} s')
        if fps == 60 and result['average_fps'] < 55:
            errors.append(f'average FPS {result["average_fps"]} is below 55')
        if result['dropped'] / max(result['decoded'], 1) >= 0.02:
            errors.append(f'dropped {result["dropped"]} of {result["decoded"]} decoded frames')
        if spec.get('repairs') and not result['repaired']:
            errors.append('loss row did not exercise retransmission repair')
        host_log = remote('log')
        (row / 'host.log').write_text(host_log)
        encoder = ('hevc_' if codec == 'hevc' else 'h264_') + family
        check_stream(result, host_log, encoder, repairs=spec.get('repairs', False),
                     bitrate=40_000_000 if spec.get('bitrate') == 40 else None)
        if fps == 120:
            check_high_refresh(result, host_log)
        if spec['link'] == 'usb' and 'USB tunnel connected' not in clean(host_log):
            errors.append('The host never opened the USB tunnel')
        if errors:
            result['status'] = 'FAIL'
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        result['status'] = 'FAIL'
        result.setdefault('errors', []).append(str(error))
    finally:
        if app is not None:
            app.send_signal(signal.SIGINT)
            try:
                app.wait(timeout=30)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait()
        if console is not None:
            console.close()
        if host_started:
            try:
                (row / 'host.log').write_text(remote('log'))
                remote('stop-host', output=row / 'host-stop.log')
            except (OSError, subprocess.SubprocessError) as error:
                result['status'] = 'FAIL'
                result.setdefault('errors', []).append('Host cleanup: ' + str(error))
                remote('stop-host', '--kill', output=row / 'host-force-cleanup.log')
        if pattern_started:
            try:
                remote('pattern', 'stop', output=row / 'pattern-stop.log')
            except (OSError, subprocess.SubprocessError) as error:
                result['status'] = 'FAIL'
                result.setdefault('errors', []).append('Pattern cleanup: ' + str(error))
        save(result_path, result)
    print(json.dumps({k: result.get(k) for k in ('scenario', 'status', 'average_fps', 'decoded', 'dropped',
                                                 'repaired', 'nacks', 'keyframe_requests', 'host_retransmits',
                                                 'errors')}), flush=True)
    return result['status'] == 'PASS'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rows', nargs='+', choices=list(ROWS), default=list(ROWS))
    parser.add_argument('--device', default=os.environ.get('EM_DEVICE', '00008112-000625DA2E23C01E'))
    parser.add_argument('--pc', default=os.environ.get('EM_PC_LAN', '10.0.0.45'),
                        help='the PC address on the iPad\'s WiFi network')
    parser.add_argument('--size', default=os.environ.get('EM_SIZE', '1920x1080'),
                        help='the captured primary display size')
    parser.add_argument('--duration', type=float, default=20)
    args = parser.parse_args()
    evidence = Path(os.environ.get('EM_EVIDENCE_DIR', '/Users/aldo/Desktop/EternalMonitor-Handoff/evidence'))
    out = evidence.resolve() / 'device'
    out.mkdir(parents=True, exist_ok=True)
    if subprocess.run(['git', '-C', str(out), 'rev-parse', '--is-inside-work-tree'],
                      capture_output=True).returncode == 0:
        parser.error('EM_EVIDENCE_DIR must be outside a git checkout')
    passed = [run_row(name, ROWS[name], out, args) for name in args.rows]
    return 0 if all(passed) else 1


if __name__ == '__main__':
    raise SystemExit(main())
