#!/usr/bin/env python3
"""Assert decoded packet count, loss and tone in the rendered audio output."""
import argparse
import datetime
import json
import pathlib
import re
import time

# CI runs dropped late packets only in the first 0.3-4.1 s of audio (by
# sequence, never past about 210). The waiver covers the stream's first 250
# sequences: 5 s of nominal audio (20 ms each), not wall time, since a
# starved host numbers fewer packets per second. That still leaves at least
# 4 s of each CI run strictly checked.
STARTUP_SEQS = 250


class IncompleteEvidence(ValueError):
    """The live host log has no stats yet that cover the iPad's range."""


def seq_ranges(text):
    """Parse the host's `3-5,9` range list; return (sequences, truncated)."""
    if text == 'none':
        return set(), False
    seqs, truncated = set(), False
    for part in text.split(','):
        if part == '+':
            truncated = True
            continue
        start, _, end = part.partition('-')
        start, end = int(start), int(end or start)
        if end < start:
            # The host never merges a range across the u32 wrap.
            raise ValueError(f'malformed host sequence range {part}')
        seqs.update(range(start, end + 1))
    return seqs, truncated


def compact(seqs):
    seqs, parts = sorted(seqs), []
    for seq in seqs:
        if parts and parts[-1][1] + 1 == seq:
            parts[-1][1] = seq
        else:
            parts.append([seq, seq])
    return ','.join(str(a) if a == b else f'{a}-{b}' for a, b in parts)


def host_stats(text):
    """Return each host audio stats line's cumulative counters and sequences."""
    lines = [line for line in re.sub(r'\x1b\[[0-9;]*m', '', text).splitlines()
             if 'Audio stream stats' in line]
    if not lines:
        raise ValueError('host logged no audio stream stats')
    stats = []
    for line in lines:
        values = dict(re.findall(r'\b(\w+)=(\d+)\b', line))
        drops = {key: int(values.get(key, 0)) for key in (
            'dropped', 'stale_dropped', 'capture_queue_dropped', 'oversized_dropped', 'send_failed')}
        # tracing stamps each line in UTC as the host emits it.
        stamp = re.match(r'\s*(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6})Z', line)
        drops['at'] = datetime.datetime.strptime(stamp.group(1), '%Y-%m-%dT%H:%M:%S.%f').replace(
            tzinfo=datetime.timezone.utc).timestamp() if stamp else 0.0
        fields = dict(re.findall(r'\b(late_seqs|failed_seqs|wire_stream)=(\S+)', line))
        if len(fields) == 3 and 'audio_stream' in values:
            drops['late'], late_truncated = seq_ranges(fields['late_seqs'])
            drops['failed'], failed_truncated = seq_ranges(fields['failed_seqs'])
            drops['truncated'] = late_truncated or failed_truncated
            drops['stream'] = int(values['audio_stream'])
            drops['wire'] = fields['wire_stream']
        stats.append(drops)
    return stats


def host_drops(text):
    """Return the host's last cumulative audio drop counters and sequences."""
    return host_stats(text)[-1]


def captures(host, session, epoch):
    """Host capture streams that numbered packets under this wire stream."""
    return {line['stream'] for line in host if line.get('wire') == f'{session}/{epoch}'}


def stream_stats(host, session, epoch, errors):
    """The last stats of the one host stream numbered under this wire stream."""
    streams = captures(host, session, epoch)
    if not streams:
        errors.append(f'host logged no audio stats for session {session} epoch {epoch}')
        return None
    stream = streams.pop()
    if any(line.get('wire') == 'mixed' for line in host if line.get('stream') == stream):
        errors.append(f'host audio stream {stream} went out under more than one wire stream')
        return None
    return [line for line in host if line.get('stream') == stream][-1]


def attribute(samples, host_lines, scheduling_gate, errors, notes, frozen_at=None):
    """Match every iPad loss to a host drop by sequence number."""
    last = samples[-1]
    first, after_last = int(last['first_seq']), int(last['next_seq'])
    identity = [(sample.get('session_id'), sample.get('epoch'), sample.get('first_seq'))
                for sample in samples if 'first_seq' in sample]
    if len(set(identity)) != 1:
        errors.append('the iPad audio timeline restarted; its losses cannot be matched to host drops')
        return None
    if after_last < first:
        errors.append(f'the iPad sequence range {first}-{after_last} wraps; attribution does not support it')
        return None
    lost = int(last['lost'])
    if int(last['decoded']) + lost != after_last - first:
        errors.append(f'iPad counters do not cover sequences {first}-{after_last - 1}')
        return None
    session, epoch = int(last['session_id']), int(last['epoch'])
    if len(captures(host_lines, session, epoch)) > 1:
        # Several captures numbered under this session and epoch (audio was
        # toggled), and nothing ties the frozen timeline to one of them. Each
        # numbers from 0, so a clean timeline from 0 is the only safe pass.
        restarted = f'host restarted audio numbering within session {session} epoch {epoch}'
        if lost:
            errors.append(f'{lost} audio packets lost but not attributable: {restarted}')
        elif first:
            errors.append(f'{first} audio packets before the iPad\'s first sequence {first} are missing '
                          f'but not attributable: {restarted}')
        else:
            notes.append(f'{restarted}; no loss to attribute')
        return None
    host = stream_stats(host_lines, session, epoch, errors)
    if host is None:
        return None
    if frozen_at is not None:
        # A live host logs stats once a second. The iPad had received a packet
        # at or past next_seq - 1 before its log froze, and the transport
        # settles every earlier packet before sending a later one, so a line
        # snapshotted after the freeze covers the range. The first line
        # emitted after it may hold an older snapshot; the stage's next cannot.
        later = sum(line['at'] > frozen_at for line in host_lines if line.get('stream') == host['stream'])
        if later < 2:
            raise IncompleteEvidence(
                f'incomplete audio evidence: host logged {later} audio stats line{"" if later == 1 else "s"} '
                f'after the app log froze; 2 are needed to cover sequences {first}-{after_last - 1}')
    if host['truncated']:
        errors.append('host audio drop list was truncated')
        return None
    # Only sequences the iPad had reached when its log was frozen count.
    # Later host drops (during screenshots) belong to no measured packet.
    late = {seq for seq in host['late'] if seq < after_last}
    failed = {seq for seq in host['failed'] if seq < after_last}
    # Each host stream numbers from 0, so everything before the iPad's first
    # packet was either dropped by the host or lost after it was sent.
    covered = sorted(seq for seq in late | failed if seq < first)
    gaps = [(a + 1, b - 1) for a, b in zip([-1] + covered, covered + [first]) if b - a > 1]
    if gaps:
        ranges = ','.join(str(a) if a == b else f'{a}-{b}' for a, b in gaps[:8])
        errors.append(f'{first - len(covered)} audio packets before the iPad\'s first sequence {first} '
                      f'were sent but never arrived ({ranges}{",..." if len(gaps) > 8 else ""})')
    unseen = {seq for seq in late | failed if seq >= first}
    if lost > len(unseen):
        errors.append(f'{lost - len(unseen)} audio packets lost after the host sent them '
                      f'(iPad lost {lost} of {first}-{after_last - 1}; host dropped {len(unseen)})')
    elif lost < len(unseen):
        errors.append(f'iPad counted {lost} losses but the host dropped {compact(unseen)}')
    if failed:
        errors.append(f'host failed to send audio packets {compact(failed)}')
    steady = {seq for seq in late if seq >= STARTUP_SEQS}
    if steady:
        errors.append(f'host dropped audio packets {compact(steady)} after the first 5 s of audio')
    startup = late - steady
    if len(startup) > 2:
        message = (f'host dropped {len(startup)} startup audio packets that missed their '
                   f'deadline ({compact(startup)})')
        (errors if scheduling_gate == 'enforce' else notes).append(message)
    return dict(audio_host_late=len(late), audio_host_late_seqs=compact(late),
                audio_first_seq=first, audio_next_seq=after_last)


def measure(text, host_text=None, scheduling_gate='enforce', frozen_at=None):
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
    notes = []
    if last['decoded'] < 100:
        errors.append(f"only {last['decoded']:.0f} audio packets decoded; need 100")
    lost = max(sample['lost'] for sample in samples)
    if any(b['lost'] < a['lost'] for a, b in zip(samples, samples[1:])):
        errors.append('audio loss counter reset during the measurement')
    host = host_stats(host_text) if host_text is not None else None
    attributed = None
    identified = host is not None and any('stream' in line for line in host) and 'session_id' in last
    if identified:
        # The iPad counts every sequence gap as lost, including packets the
        # host discarded itself. Packets the host held past their 100 ms
        # deadline because its sender did not get the CPU measure the
        # machine, but only while the app launches.
        if not errors:
            attributed = attribute(samples, host, scheduling_gate, errors, notes, frozen_at)
    else:
        # Evidence from builds without stream identity: no loss can be
        # attributed, and no drop is waived. Host numbering starts at 0, so
        # sequences before the iPad's first packet count as lost too.
        unattributed = lost + last.get('first_seq', 0)
        if unattributed > 2:
            errors.append(f"audio packet loss {unattributed:.0f} exceeds 2")
    # The source deliberately includes quiet intervals; require the tone in
    # at least two distinct rendered windows, not in a window of silence.
    tones = [sample for sample in samples if sample['tone1k_db'] > -20 and sample['rms'] > 0.05]
    if len(tones) < 2:
        errors.append('1 kHz tone missing from two rendered audio windows')
    if errors:
        raise ValueError('; '.join(errors))
    peak = max(tones, key=lambda sample: sample['tone1k_db'])
    result = dict(audio_packets=int(last['packets']), audio_decoded=int(last['decoded']),
                  audio_lost=int(lost), audio_buffer_ms=int(last['buffer_ms']),
                  audio_tone1k_db=peak['tone1k_db'], audio_rms=peak['rms'], audio_notes=notes)
    if attributed is not None:
        result.update(attributed, audio_scheduling_gate=scheduling_gate)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('log', type=pathlib.Path)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--host-log', type=pathlib.Path)
    # `record` notes startup deadline drops without failing on them, for
    # runners where the launching simulator app starves the host of CPU.
    parser.add_argument('--scheduling-gate', choices=['enforce', 'record'], default='enforce')
    # For a host log that is still being written: the Unix time the app log
    # was copied, and how long to wait for host stats that cover it.
    parser.add_argument('--frozen-at', type=float)
    parser.add_argument('--host-wait', type=float, default=0)
    args = parser.parse_args()
    result = json.loads(args.output.read_text())
    deadline = time.monotonic() + args.host_wait
    try:
        while True:
            host_text = args.host_log.read_text(errors='replace') if args.host_log else None
            try:
                measured = measure(args.log.read_text(), host_text, args.scheduling_gate, args.frozen_at)
                break
            except IncompleteEvidence:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.5)
    except ValueError as error:
        result['status'] = 'FAIL'
        result.setdefault('errors', []).append(str(error))
        args.output.write_text(json.dumps(result, indent=2) + '\n')
        raise SystemExit(str(error))
    result.setdefault('notes', []).extend(measured.pop('audio_notes'))
    result.update(measured)
    args.output.write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
