#!/usr/bin/env python3
"""Drive real iPad gestures and validate the Windows probe's observed input."""
import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess

from real_checks import check_probe

ROOT = Path(__file__).resolve().parent.parent


def main():
    out = Path(os.environ['EM_OUTPUT_DIR'])
    test_root = Path(os.environ['EM_UI_EVIDENCE_DIR'])
    env = dict(os.environ, DEVELOPER_DIR='/Applications/Xcode.app/Contents/Developer',
               EM_TEST_STAMP=datetime.datetime.now().strftime('%Y%m%d-%H%M%S'))
    stamp = env['EM_TEST_STAMP']
    errors = []
    with (out / 'ui-run.log').open('w') as log:
        test = subprocess.run([str(ROOT / 'scripts/test_ios.sh'),
                              '-only-testing:EternalMonitorUITests/InputGestureTests/testVideoGesturesAndKeyboard'],
                             env=env, stdout=log, stderr=subprocess.STDOUT, timeout=600)
    if test.returncode:
        errors.append('Input UI test failed; see ui-run.log')
    bundle = test_root / f'ios-tests-{stamp}.xcresult'
    summary = subprocess.check_output(['xcrun','xcresulttool','get','test-results','summary',
                                       '--path',str(bundle),'--format','json'], env=env, text=True)
    (out / 'tests.json').write_text(summary)
    summary = json.loads(summary)
    if summary.get('passedTests') != 1 or summary.get('failedTests') != 0:
        errors.append('Expected exactly one passing input gesture test')
    shots = test_root / f'screenshots/ui-{stamp}'
    manifest = json.loads((shots / 'manifest.json').read_text())
    for name, suffix in [('input-gestures','.png'), ('input-keyboard','.png'), ('input-expected','.json')]:
        found = False
        for test in manifest:
            for item in test.get('attachments', []):
                if item.get('suggestedHumanReadableName','').startswith(name + '_') and item['exportedFileName'].endswith(suffix):
                    shutil.copy2(shots / item['exportedFileName'], out / (name + suffix))
                    found = True
        if not found:
            errors.append('Missing attachment: ' + name)
        elif suffix == '.png':
            pixels = subprocess.run([str(ROOT / 'scripts/pixels.sh'), str(out / (name + suffix)),
                                     '--assert-pattern'], text=True, capture_output=True)
            (out / (name + '-pixels.log')).write_text(pixels.stdout + pixels.stderr)
            if pixels.returncode:
                errors.append('Blank or invalid video in ' + name)
    subprocess.run([str(ROOT / 'scripts/win/remote.sh'), 'probe-log'], check=True)
    probe = Path(os.environ['EM_EVIDENCE_DIR']) / 'windows/input-probe.log'
    shutil.copy2(probe, out / 'input-probe.log')
    result = dict(scenario='R-input', status='FAIL' if errors else 'PASS', errors=errors,
                  screenshot=str(out / 'input-gestures.png'), tests_passed=summary.get('passedTests',0))
    try:
        expected = json.loads((out / 'input-expected.json').read_text())
        result.update(check_probe([json.loads(line) for line in probe.read_text().splitlines()], expected))
    except (ValueError, OSError) as error:
        result['errors'].append(str(error))
        result['status'] = 'FAIL'
    (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
    return int(result['status'] != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
