#!/usr/bin/env python3
"""Run Windows capture/encoder/audio/pairing rows through the console runner."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import shutil
import time

from real_checks import check_stream, check_vdd_mode, clean

ROOT = Path(__file__).resolve().parent.parent
REMOTE = ROOT / 'scripts/win/remote.sh'
SCENARIOS = ('R-baseline', 'R-nvenc-h264', 'R-nvenc-hevc', 'R-amf-h264', 'R-amf-hevc',
             'R-nvenc-h264-loss3', 'R-nvenc-burst', 'R-audio', 'R-pairing', 'R-vdd', 'R-reconnect', 'R-gui', 'R-input')
# Input runs last: its SendInput events reset Windows' two-minute idle gate.


def run(command, *, env=None, output=None, timeout=600):
    if output is None:
        return subprocess.run(command, env=env, check=True, timeout=timeout,
                              text=True, stdout=subprocess.PIPE).stdout
    with output.open('w') as log:
        subprocess.run(command, env=env, check=True, timeout=timeout,
                       text=True, stdout=log, stderr=subprocess.STDOUT)


def remote(*args, **kwargs):
    return run([str(REMOTE), *args], **kwargs)


def save(path, result):
    path.write_text(json.dumps(result, indent=2) + '\n')


def fail(path, reason):
    result = json.loads(path.read_text())
    result['status'] = 'FAIL'
    result.setdefault('errors', []).append(reason)
    save(path, result)


def verify_vdd_host_exit(row, env):
    """Reconnect, then exit the host while its virtual display is still in use."""
    udid = env.get('EM_SIM_UDID', '06416ADB-C33D-4EE4-82DB-44FCD663362F')
    container = run(['xcrun','simctl','get_app_container',udid,'com.eternal.monitor','data'], env=env).strip()
    milestone = Path(container) / 'tmp/eternal-e2e.log'
    milestone.unlink(missing_ok=True)
    launch_env = dict(env, SIMCTL_CHILD_EM_AUTOCONNECT='100.81.59.48:19876', SIMCTL_CHILD_EM_E2E_LOG='1')
    run(['xcrun','simctl','launch',udid,'com.eternal.monitor','-didSeeOnboarding','YES',
         '-allowUSB','NO','-playPCaudio','NO'], env=launch_env)
    try:
        deadline = time.monotonic() + 45
        while not milestone.exists() or 'E2E_FIRST_FRAME w=2420 h=1668' not in milestone.read_text():
            if time.monotonic() > deadline:
                raise ValueError('VDD did not reconnect before the active-host-exit check')
            time.sleep(.2)
        state = json.loads(remote('vdd-state'))
        save(row / 'vdd-before-host-exit.json', state)
        if state['disabled']:
            raise ValueError('VDD was disabled before the active-host-exit check')
        remote('stop-host', output=row / 'active-host-stop.log')
        deadline = time.monotonic() + 20
        while True:
            state = json.loads(remote('vdd-state'))
            if state['disabled']:
                save(row / 'vdd-active-host-exit.json', state)
                break
            if time.monotonic() > deadline:
                raise ValueError('VDD remained enabled after active host exit')
            time.sleep(.5)
        run(['xcrun','simctl','io',udid,'screenshot',str(row / 'vdd-host-exit.png')], env=env)
        run([str(ROOT / 'scripts/pixels.sh'),str(row / 'vdd-host-exit.png'),'--assert-pattern'],
            output=row / 'vdd-host-exit-pixels.json')
    finally:
        if milestone.exists():
            shutil.copy2(milestone, row / 'vdd-host-exit-app.log')
        run(['xcrun','simctl','terminate',udid,'com.eternal.monitor'], env=env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--list', action='store_true')
    parser.add_argument('--rows', nargs='+', choices=SCENARIOS, default=list(SCENARIOS))
    args = parser.parse_args()
    if args.list:
        print('\n'.join(args.rows))
        return 0
    size = os.environ.get('EM_SIZE', '1920x1080')
    if not re.fullmatch(r'[1-9][0-9]*x[1-9][0-9]*', size):
        parser.error('EM_SIZE must be the primary display width and height, for example 3440x1440')
    width, height = map(int, size.split('x'))
    evidence = Path(os.environ.get('EM_EVIDENCE_DIR', '/Users/aldo/Desktop/EternalMonitor-Handoff/evidence')).resolve()
    out = evidence / 'real'
    out.mkdir(parents=True, exist_ok=True)
    # Desktop and input evidence must never land in any checked-out repository.
    if subprocess.run(['git', '-C', str(out), 'rev-parse', '--is-inside-work-tree'],
                      capture_output=True).returncode == 0:
        parser.error('EM_EVIDENCE_DIR must be outside a git checkout')
    failed = False
    skip_build = '0'
    archive = evidence / 'real-archive' / str(time.time_ns())
    for scenario in args.rows:
        row = out / scenario
        if row.exists() and any(row.iterdir()):
            archive.mkdir(parents=True, exist_ok=True)
            shutil.move(str(row), str(archive / scenario))
        row.mkdir(parents=True, exist_ok=True)
        save(row / 'result.json', dict(scenario=scenario, status='NOT RUN', errors=['Prerequisite not reached']))
    for scenario in args.rows:
        row = out / scenario
        row.mkdir(parents=True, exist_ok=True)
        result_path = row / 'result.json'
        save(result_path, dict(scenario=scenario, status='FAIL', errors=['Run did not finish']))
        started = time.monotonic()
        pattern_started = host_started = probe_started = False
        tone = None
        tone_log = None
        controller = None
        controller_log = None
        gui_checked = False
        vdd_owned = vdd_checked = False
        env = dict(os.environ, EM_SCENARIO=scenario, EM_OUTPUT_DIR=str(row),
                   EM_EVIDENCE_DIR=str(evidence), EM_UI_EVIDENCE_DIR=str(row / 'ui'),
                   DEVELOPER_DIR='/Applications/Xcode.app/Contents/Developer',
                   EM_SCREENSHOT=str(row / 'simulator.png'), EM_REMOTE_HOST='100.81.59.48',
                   EM_PORT='19876', EM_SIZE=size, EM_DURATION='20', EM_TIMEOUT='180',
                   EM_SKIP_BUILD=skip_build, EM_REQUIRE_PAIRING='0', EM_AUDIO='0', EM_BITRATE_MBPS='15',
                   EM_REQUIRE_REPAIRS='0')
        hevc = scenario.endswith('hevc')
        family = 'amf' if scenario.startswith('R-amf') else 'nvenc'
        encoder = ('hevc_' if hevc else 'h264_') + family
        host_args = ['ETERNAL_HEADLESS=1', 'ETERNAL_ENCODER=h264_' + family,
                     f'ETERNAL_HEVC={int(hevc)}', 'ETERNAL_FPS=60', 'ETERNAL_MAX_DGRAM=1200',
                     'ETERNAL_E2E_LOG=1', 'ETERNAL_USB_DIRECT=127.0.0.1:0']
        # These rows target the simulator over UDP. A physically attached
        # iPad must not claim their session through the USB supervisor.
        env['EM_CODEC'] = 'hevc' if hevc else 'h264'
        if scenario == 'R-nvenc-h264-loss3':
            env['EM_REQUIRE_REPAIRS'] = '1'
            host_args += ['ETERNAL_DROP=0.03', 'ETERNAL_REORDER=0.01']
        if scenario == 'R-nvenc-burst':
            env['EM_BITRATE_MBPS'] = '40'
            host_args += ['ETERNAL_ABR=0', 'ETERNAL_FORCE_IDR_PERIOD=60']
        if family == 'amf':
            host_args += ['ETERNAL_AMF_DIAG=1']
        if scenario == 'R-audio':
            env.update(EM_AUDIO='1', EM_DURATION='40')
        if scenario in ('R-audio', 'R-pairing', 'R-gui'):
            host_args[0] = 'ETERNAL_HEADLESS=0'
        if scenario == 'R-gui':
            env['EM_DURATION'] = '40'
        if scenario == 'R-vdd':
            env.update(EM_CAPTURE_DISPLAY='virtual', EM_SIZE='2420x1668')
        if scenario == 'R-pairing':
            env['EM_REQUIRE_PAIRING'] = '1'
            host_args[0] = 'ETERNAL_HEADLESS=0'
        print('==> ' + scenario, flush=True)
        try:
            if json.loads(remote('host-info')) is not None:
                raise ValueError('Another host run is already tracked; finish it before starting this matrix')
            if scenario == 'R-vdd':
                state = json.loads(remote('vdd-state'))
                save(row / 'vdd-before.json', state)
                if not state['disabled']:
                    raise ValueError('VDD is already enabled; refusing to change another session’s display')
                vdd_owned = True
            # Set flags before startup so partial launches also get cleaned up.
            if scenario == 'R-input':
                probe_started = True
                remote('probe', 'start', 'fullscreen', output=row / 'probe-start.log')
                probe = json.loads(remote('probe-info'))
                save(row / 'probe-ready.json', probe)
                if (probe['x'], probe['y'], probe['width'], probe['height']) != (0, 0, width, height):
                    raise ValueError(f'The foreground input probe must cover the {size} primary screen')
                host_args += ['ETERNAL_INPUT_WINDOW_PID=' + str(probe['pid']), 'ETERNAL_INPUT_RECORDER_LOG=1']
                env.update(EM_INPUT_WIDTH=str(width), EM_INPUT_HEIGHT=str(height))
            else:
                pattern_started = True
                remote('pattern', 'start', *(['virtual'] if scenario == 'R-vdd' else []), output=row / 'pattern-start.log')
            host_started = True
            remote('run-host', *host_args, env=env, output=row / 'host-start.log')
            if scenario in ('R-reconnect', 'R-input'):
                lifecycle = row / 'lifecycle'
                lifecycle.mkdir(exist_ok=True)
                for name in ('stop.request', 'start.request', 'started.json', 'stopped.json', 'restarted.json', 'error.txt'):
                    (lifecycle / name).unlink(missing_ok=True)
                env.update(EM_LIFECYCLE_DIR=str(lifecycle), EM_INPUT_HOST='100.81.59.48:19876',
                           EM_INPUT_HOST_LOG=str(row / 'live-host.log'))
                controller_log = (row / 'controller.log').open('w')
                controller = subprocess.Popen(['python3', str(ROOT / 'scripts/remote_lifecycle.py'),
                                               str(lifecycle), env['EM_INPUT_HOST_LOG'], *host_args],
                                              env=env, stdout=controller_log, stderr=subprocess.STDOUT)
                runner = ['python3', str(ROOT / 'scripts/e2e_input.py')] if scenario == 'R-input' else [str(ROOT / 'scripts/e2e_lifecycle.sh')]
                run(runner, env=env, output=row / 'run.log')
                if controller.poll() is not None or (lifecycle / 'error.txt').exists():
                    raise ValueError('The real host restart controller failed')
            elif scenario == 'R-pairing':
                log = remote('log')
                (row / 'host.log').write_text(log)
                match = re.search(r'pairing_code=(\d{6})', clean(log))
                if not match:
                    raise ValueError('No startup pairing code in the real host log')
                env.update(EM_PAIRING_CODE=match[1], EM_PAIRING_HOST='100.81.59.48:19876')
                remote('gui', 'Stream', scenario + '-pairing', output=row / 'pairing-card.json')
                run([str(ROOT / 'scripts/pixels.sh'), str(evidence / 'windows' / (scenario + '-pairing.png')),
                     '--assert-ui'], output=row / 'pairing-pixels.json')
                run([str(ROOT / 'scripts/e2e_pairing.sh')], env=env, output=row / 'run.log')
            else:
                # Build/install first, then start the thirty-second PC tone when
                # the app actually begins video; boot/build time is not tone time.
                with (row / 'run.log').open('w') as log:
                    client = subprocess.Popen([str(ROOT / 'scripts/e2e_ios.sh')], env=env,
                                              stdout=log, stderr=subprocess.STDOUT)
                    deadline = time.monotonic() + 600
                    try:
                        while client.poll() is None:
                            app_log = row / 'app.log'
                            milestones = app_log.read_text() if app_log.exists() else ''
                            if scenario == 'R-vdd' and not vdd_checked and 'w=2420 h=1668' in milestones:
                                state = json.loads(remote('vdd-state'))
                                save(row / 'vdd-connected.json', state)
                                check_vdd_mode(state, milestones)
                                remote('shot', scenario + '-connected', output=row / 'vdd-connected-shot.log')
                                vdd_checked = True
                            if scenario == 'R-audio' and tone is None and 'E2E_FIRST_FRAME' in milestones:
                                tone_log = (row / 'tone.log').open('w')
                                tone = subprocess.Popen([str(REMOTE), 'tone', '30'], stdout=tone_log, stderr=subprocess.STDOUT)
                            if scenario in ('R-audio', 'R-gui') and not gui_checked and milestones.count('E2E_STATS') >= 4:
                                for view in (('Stream', 'Settings', 'QR') if scenario == 'R-gui' else ('Stream',)):
                                    name = scenario + '-' + view.lower()
                                    remote('gui', view, name, '--connected', output=row / (view.lower() + '-controls.json'))
                                    run([str(ROOT / 'scripts/pixels.sh'), str(evidence / 'windows' / (name + '.png')),
                                         '--assert-ui'], output=row / (view.lower() + '-pixels.json'))
                                gui_checked = True
                            if time.monotonic() >= deadline:
                                raise TimeoutError('Simulator row exceeded ten minutes')
                            time.sleep(.2)
                        if client.returncode:
                            raise subprocess.CalledProcessError(client.returncode, client.args)
                    finally:
                        if client.poll() is None:
                            client.terminate()
                            client.wait(timeout=20)
                if scenario == 'R-audio':
                    if tone is None:
                        raise ValueError('The Windows tone was not started')
                    if tone.wait(timeout=60):
                        raise ValueError('The Windows tone failed; see tone.log')
                if scenario in ('R-audio', 'R-gui') and not gui_checked:
                    raise ValueError('Connected host GUI evidence was not collected')
                if scenario == 'R-vdd':
                    if not vdd_checked:
                        raise ValueError('VDD connection/mode evidence was not collected')
                    deadline = time.monotonic() + 20
                    while True:
                        state = json.loads(remote('vdd-state'))
                        if state['disabled']:
                            save(row / 'vdd-disconnected.json', state)
                            break
                        if time.monotonic() >= deadline:
                            raise ValueError('VDD remained enabled after the app disconnected')
                        time.sleep(.5)
                    verify_vdd_host_exit(row, env)
            skip_build = '1'
            host_log = remote('log')
            (row / ('host-final.log' if scenario == 'R-reconnect' else 'host.log')).write_text(host_log)
            (row / 'host.stderr.log').write_text(remote('stderr'))
            result = json.loads(result_path.read_text())
            if scenario not in ('R-pairing', 'R-reconnect', 'R-input'):
                check_stream(result, host_log, encoder,
                             repairs=scenario.endswith('loss3'),
                             bitrate=40000000 if scenario.endswith('burst') else None,
                             audio=scenario == 'R-audio')
            save(result_path, result)
            if family == 'amf':
                remote('diagnostic', env['EM_CODEC'], scenario, output=row / 'bitstream-validation.log')
            remote('shot', scenario, output=row / 'desktop-shot.log')
            screenshot = evidence / 'windows' / (scenario + '.png')
            run([str(ROOT / 'scripts/pixels.sh'), str(screenshot), '--assert-pattern'], output=row / 'desktop-pixels.json')
            if result['status'] != 'PASS':
                failed = True
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            failed = True
            result = json.loads(result_path.read_text())
            result.update(status='FAIL', scenario=scenario)
            result.setdefault('errors', []).append(str(error))
            save(result_path, result)
            print(f'FAIL: {scenario}: {error}', flush=True)
        finally:
            if controller is not None:
                controller.terminate()
                try:
                    controller.wait(timeout=50)
                except subprocess.TimeoutExpired:
                    controller.kill()
                    controller.wait(timeout=5)
                    fail(result_path, 'Host log/restart controller did not stop')
                    failed = True
                controller_log.close()
            if tone is not None:
                try:
                    # The tone is bounded on the PC. Await its completion even
                    # when the client fails, rather than abandoning a remote sound.
                    tone.wait(timeout=65)
                except subprocess.TimeoutExpired:
                    fail(result_path, 'Windows tone did not finish within its bound')
                    failed = True
                if tone_log:
                    tone_log.close()
            if host_started:
                try:
                    (row / ('host-final.log' if scenario == 'R-reconnect' else 'host.log')).write_text(remote('log'))
                except (OSError, UnicodeError, subprocess.SubprocessError) as error:
                    print('Could not collect final host log: ' + str(error), flush=True)
                try:
                    remote('stop-host', output=row / 'host-stop.log')
                except (OSError, subprocess.SubprocessError) as error:
                    fail(result_path, 'Host cleanup: ' + str(error))
                    failed = True
                    try:
                        remote('stop-host', '--kill', output=row / 'host-force-cleanup.log')
                    except (OSError, subprocess.SubprocessError) as force_error:
                        print('Host force cleanup failed: ' + str(force_error), flush=True)
            if probe_started:
                try:
                    remote('probe', 'stop', output=row / 'probe-stop.log')
                except (OSError, subprocess.SubprocessError) as error:
                    fail(result_path, 'Probe cleanup: ' + str(error))
                    failed = True
            if pattern_started:
                try:
                    remote('pattern', 'stop', output=row / 'pattern-stop.log')
                except (OSError, subprocess.SubprocessError):
                    failed = True
                    result = json.loads(result_path.read_text())
                    result.update(status='FAIL')
                    result.setdefault('errors', []).append('Pattern window cleanup failed')
                    save(result_path, result)
            if vdd_owned:
                try:
                    state = json.loads(remote('vdd-state'))
                    save(row / 'vdd-after-host.json', state)
                    if not state['disabled']:
                        remote('vdd-disable', output=row / 'vdd-cleanup.log')
                        raise ValueError('Host exit left the VDD enabled; cleanup task was required')
                except (OSError, ValueError, subprocess.SubprocessError) as error:
                    try:
                        remote('vdd-disable', output=row / 'vdd-cleanup-retry.log')
                    except (OSError, subprocess.SubprocessError) as cleanup_error:
                        print('VDD cleanup failed: ' + str(cleanup_error), flush=True)
                    failed = True
                    result = json.loads(result_path.read_text())
                    result.update(status='FAIL')
                    result.setdefault('errors', []).append('VDD cleanup: ' + str(error))
                    save(result_path, result)
            if scenario in ('R-input', 'R-pairing', 'R-reconnect') and host_started:
                try:
                    container = run(['xcrun', 'simctl', 'get_app_container',
                                     env.get('EM_SIM_UDID', '06416ADB-C33D-4EE4-82DB-44FCD663362F'),
                                     'com.eternal.monitor', 'data'], env=env).strip()
                    shutil.copy2(Path(container) / 'tmp/eternal-e2e.log', row / 'milestones.log')
                except (OSError, subprocess.SubprocessError) as error:
                    fail(result_path, 'App milestone evidence: ' + str(error))
                    failed = True
            result = json.loads(result_path.read_text())
            result['elapsed'] = round(time.monotonic() - started, 2)
            save(result_path, result)
        print(json.dumps(result), flush=True)
        # A failed desktop prerequisite must not trigger the remaining GUI rows.
        if result['status'] != 'PASS':
            break
    return int(failed)


if __name__ == '__main__':
    raise SystemExit(main())
