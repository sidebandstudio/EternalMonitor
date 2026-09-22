#!/usr/bin/env python3
"""Assert decoded packet count, loss and tone in the rendered audio output."""
import argparse
import json
import pathlib
import re


def measure(text):
    samples = []
    for line in text.splitlines():
        if 'E2E_AUDIO ' not in line:
            continue
        values = dict(re.findall(r'(\w+)=(-?[\d.]+(?:[eE][+-]?\d+)?)', line.split('E2E_AUDIO ', 1)[1]))
        if all(key in values for key in ('packets', 'decoded', 'lost', 'buffer_ms', 'rms', 'tone1k_db')):
            samples.append({key: float(value) for key, value in values.items()})
    if not samples:
        raise ValueError('no rendered audio measurements')
    last = samples[-1]
    errors = []
    if last['decoded'] < 100:
        errors.append(f"only {last['decoded']:.0f} audio packets decoded; need 100")
    lost = max(sample['lost'] for sample in samples)
    if lost > 2:
        errors.append(f"audio packet loss {lost:.0f} exceeds 2")
    # The source deliberately includes quiet intervals; require the tone in
    # at least two distinct rendered windows, not in a window of silence.
    tones = [sample for sample in samples if sample['tone1k_db'] > -20 and sample['rms'] > 0.05]
    if len(tones) < 2:
        errors.append('1 kHz tone missing from two rendered audio windows')
    if errors:
        raise ValueError('; '.join(errors))
    peak = max(tones, key=lambda sample: sample['tone1k_db'])
    return dict(audio_packets=int(last['packets']), audio_decoded=int(last['decoded']),
                audio_lost=int(lost), audio_buffer_ms=int(last['buffer_ms']),
                audio_tone1k_db=peak['tone1k_db'], audio_rms=peak['rms'])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('log', type=pathlib.Path)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    result = json.loads(args.output.read_text())
    try:
        result.update(measure(args.log.read_text()))
    except ValueError as error:
        result['status'] = 'FAIL'
        result.setdefault('errors', []).append(str(error))
        args.output.write_text(json.dumps(result, indent=2) + '\n')
        raise SystemExit(str(error))
    args.output.write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
