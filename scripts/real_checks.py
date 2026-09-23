"""Checks for evidence produced by the interactive Windows hardware rows."""
import re


def clean(text):
    return re.sub(r'\x1b\[[0-9;]*m', '', text)


def check_bgra(result, log, colors, reference):
    """Direct input must remain active for ten minutes and preserve color."""
    log = clean(log)
    formats = re.findall(r'Encoder opened[^\n]*\binput="?([A-Z0-9]+)', log)
    if not formats or any(value not in ('BGRA', 'BGRZ', 'BGR0') for value in formats):
        raise ValueError(f'Direct BGRA input was not used throughout: {formats}')
    if result.get('measured_seconds', 0) < 600:
        raise ValueError('BGRA stability measurement was shorter than ten minutes')
    if re.search(r'\bERROR\b|encoder[^\n]*failed|capture watchdog|pipeline restart storm', log, re.I):
        raise ValueError('Encoder/capture error during BGRA stability run')
    if (colors.get('width'), colors.get('height')) != (reference.get('width'), reference.get('height')):
        raise ValueError('BGRA and YUV screenshots have different dimensions')
    samples = [entry.get('quadrants_rgb') for entry in (colors, reference)]
    if any(not isinstance(s, list) or len(s) != 4 or any(
            not isinstance(rgb, list) or len(rgb) != 3 or any(
                not isinstance(v, (int, float)) or not 0 <= v <= 255 for v in rgb) for rgb in s)
           for s in samples):
        raise ValueError('Expected four RGB color patches in both screenshots')
    errors = [sum(abs(a - b) for a, b in zip(actual, expected)) / 3
              for actual, expected in zip(*samples)]
    result.update(encoder_input=formats[-1], color_mean_channel_errors=errors)
    if max(errors) >= 12:
        raise ValueError(f'BGRA color error must be below 12/255 in every quadrant: {errors}')
    return result


def check_high_refresh(result, log):
    log = clean(log)
    rates = [int(v) for v in re.findall(r'Encoder opened[^\n]*\bfps=(\d+)', log)]
    if not rates or rates[-1] != 120:
        raise ValueError(f'Host did not negotiate a 120 FPS encoder: {rates}')
    if re.search(r'\bERROR\b|encoder[^\n]*failed|capture watchdog|pipeline restart storm', log, re.I):
        raise ValueError('Encoder/capture error during high-refresh run')
    result['target_fps'] = 120
    return result


def check_stream(result, log, encoder, repairs=False, bitrate=None, audio=False):
    log = clean(log)
    errors = []
    opened = [line for line in log.splitlines() if 'Encoder opened' in line]
    names = [m[1] for line in opened if (m := re.search(r'\bencoder="?([\w]+)', line))]
    if 'Desktop duplication active' not in log:
        errors.append('DXGI desktop duplication did not open')
    if not names or names[-1] != encoder:
        errors.append(f'Expected active {encoder}, observed {names}')
    if 'software fallback' in log.lower() or 'pipeline restart storm' in log.lower():
        errors.append('Hardware encoder fallback or restart storm')
    if bitrate is not None:
        rates = [int(m[1]) for line in opened if (m := re.search(r'\bbitrate=(\d+)', line))]
        if not rates or any(value != bitrate for value in rates):
            errors.append(f'Fixed encoder bitrate was {rates}; expected {bitrate}')
    requests = log.count('Keyframe request received')
    retransmits = max([int(v) for v in re.findall(r'\bretransmits=(\d+)', log)], default=0)
    if requests > max(1, int(result.get('measured_seconds', 0) / 10)):
        errors.append(f'Keyframe request storm: {requests}')
    if repairs and not retransmits:
        errors.append('Host did not retransmit any fragments')
    result.update(encoder=names[-1] if names else None,
                  keyframe_requests=requests, host_retransmits=retransmits)
    if audio:
        audio_lines = [line for line in log.splitlines() if 'Audio stream stats' in line]
        if not audio_lines or 'WASAPI' not in log:
            errors.append('Real WASAPI endpoint/audio statistics missing')
        quiet = [int(v) for v in re.findall(r'\bquiet_packets=(\d+)', '\n'.join(audio_lines))]
        if not quiet or max(quiet) <= min(quiet):
            errors.append('No compact silence packets after the Windows tone')
        result['audio_quiet_packets'] = max(quiet, default=0)
    if errors:
        result['status'] = 'FAIL'
        result.setdefault('errors', []).extend(errors)
    return result


def check_probe(events, expected):
    """Assert the observed Windows events, including desktop coordinate error."""
    ready = next((e for e in events if e['event'] == 'Ready'), None)
    if not ready or (ready['width'], ready['height']) != (expected['width'], expected['height']):
        raise ValueError('Input probe does not cover the captured desktop')
    armed = [i for i, e in enumerate(events) if e['event'] == 'Armed']
    if len(armed) != 1:
        raise ValueError('Input probe must confirm foreground focus exactly once before input')
    startup = events[:armed[0]]
    focus_clicks = [e for e in startup if e['event'] == 'FocusClick']
    startup_buttons = [e for e in startup if e['event'] in ('MouseDown', 'MouseUp')]
    if focus_clicks:
        center = (ready['x'] + ready['width'] // 2, ready['y'] + ready['height'] // 2)
        if len(focus_clicks) != 1 or (focus_clicks[0]['x'], focus_clicks[0]['y']) != center or \
                [e['event'] for e in startup_buttons] != ['MouseDown', 'MouseUp'] or \
                any(e['button'] != 'Left' or (e['x'], e['y']) != center for e in startup_buttons):
            raise ValueError('Probe activation must be one bounded center click')
    elif startup_buttons:
        raise ValueError('Input reached the probe before it was armed')
    if any(e['event'] not in ('Ready', 'Deactivated', 'MouseMove', 'Arming', 'FocusClick', 'MouseDown', 'MouseUp') for e in startup):
        raise ValueError('Input reached the probe before it was armed')
    events = events[armed[0]:]
    if any(e['event'] == 'Deactivated' for e in events):
        raise ValueError('Input probe lost foreground focus during the test')
    errors = []
    mapping_error = 0

    def located(event, point):
        nonlocal mapping_error
        distance = max(abs(event['x'] - ready['x'] - point[0]),
                       abs(event['y'] - ready['y'] - point[1]))
        mapping_error = max(mapping_error, distance)
        if distance > 3:
            errors.append(f'{event["event"]} at {(event["x"], event["y"])} missed {point} by {distance:.1f}px')

    downs = [(i, e) for i, e in enumerate(events) if e['event'] == 'MouseDown' and e['button'] == 'Left']
    ups = [(i, e) for i, e in enumerate(events) if e['event'] == 'MouseUp' and e['button'] == 'Left']
    if len(downs) != 6 or len(ups) != 6:
        errors.append(f'Expected five clicks and one drag, got {len(downs)} downs/{len(ups)} ups')
    else:
        for (_, down), (_, up), point in zip(downs[:5], ups[:5], expected['clicks']):
            located(down, point)
            located(up, point)
        located(downs[5][1], expected['drag_start'])
        located(ups[5][1], expected['drag_end'])
        moves = [e for e in events[downs[5][0]:ups[5][0]] if e['event'] == 'MouseMove' and e['button'] == 'Left']
        if len({(e['x'], e['y']) for e in moves}) < 5:
            errors.append('Drag did not produce a held-button path')
        if any(d[0] >= u[0] for d, u in zip(downs, ups)):
            errors.append('Mouse release preceded its press')
    right_down = [e for e in events if e['event'] == 'MouseDown' and e['button'] == 'Right']
    right_up = [e for e in events if e['event'] == 'MouseUp' and e['button'] == 'Right']
    if len(right_down) != 1 or len(right_up) != 1:
        errors.append('Long press did not produce exactly one right-click')
    else:
        located(right_down[0], expected['right_click'])
        located(right_up[0], expected['right_click'])
    wheel = [e['delta'] for e in events if e['event'] == 'MouseWheel']
    # The content follows the fingers: an upward pan sends a negative Windows wheel.
    if len(wheel) < 2 or any(v >= 0 for v in wheel) or sum(wheel) > -120:
        errors.append(f'Upward two-finger scroll had unexpected wheel deltas: {wheel}')
    typed = ''.join(e['char'] for e in events if e['event'] == 'KeyPress')
    if typed != 'Hi!\r':
        errors.append(f'Probe received {typed!r}, expected Hi! and Enter')
    enter = [e['event'] for e in events if e['event'] in ('KeyDown','KeyUp') and e.get('keycode') == 13 and e.get('scan') == 28]
    if enter != ['KeyDown', 'KeyUp']:
        errors.append(f'Enter scan code sequence was {enter}')
    if errors:
        raise ValueError('; '.join(errors))
    return dict(input_mapping_error_px=mapping_error, wheel_delta=sum(wheel), probe_events=len(events))


def check_vdd_mode(state, milestones):
    """Windows must expose the connected client's actual advertised mode first."""
    import xml.etree.ElementTree as ET
    advertised = re.search(r'E2E_HELLO w=(\d+) h=(\d+) refresh_hz=(\d+)', milestones)
    if not advertised:
        raise ValueError('Client display advertisement is missing')
    width, height, hz = map(int, advertised.groups())
    expected = (max(width, height), min(width, height), hz)
    if expected[:2] != (2420, 1668) or expected[2] not in (30, 60, 120):
        raise ValueError(f'Unexpected simulator display advertisement: {expected}')
    first = ET.fromstring(state['settings_xml']).find('./resolutions/resolution')
    actual = None if first is None else tuple(int(first.findtext(k)) for k in ('width', 'height', 'refresh_rate'))
    if state['disabled'] or actual != expected:
        raise ValueError(f'Connected VDD first mode {actual} did not match advertised {expected}')
    return expected
