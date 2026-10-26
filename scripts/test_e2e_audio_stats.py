import datetime
import unittest
from e2e_audio_stats import IncompleteEvidence, measure

STAMP = datetime.datetime(2026, 10, 21, 17, 34, 28, 124881)
# The app log froze half a second after the host's first stats line.
FROZEN = (STAMP + datetime.timedelta(seconds=0.5)).replace(tzinfo=datetime.timezone.utc).timestamp()


def sample(decoded=100, lost=0, tone=-12, rms=0.177, first=None, after=None, session=7, epoch=3):
    line = f'E2E_AUDIO packets={decoded} decoded={decoded} lost={lost} buffer_ms=60 rms={rms} tone1k_db={tone}'
    if first is not None:
        after = first + decoded + lost if after is None else after
        line += f' session_id={session} epoch={epoch} first_seq={first} next_seq={after % 2**32}'
    return line + '\n'


def host(stale=0, queue=0, oversized=0, send_failed=0, late=None, failed='none', stream=1, wire='7/3', at=0):
    # The host writes tracing fields with ANSI styling around each name.
    fields = dict(dropped=stale + queue + oversized + send_failed, capture_queue_dropped=queue,
                  stale_dropped=stale, oversized_dropped=oversized, send_failed=send_failed)
    if late is not None:
        fields.update(late_seqs=late, failed_seqs=failed, audio_stream=stream, wire_stream=wire)
    styled = ' '.join(f'\x1b[3m{key}\x1b[0m\x1b[2m=\x1b[0m{value}' for key, value in fields.items())
    stamp = (STAMP + datetime.timedelta(seconds=at)).strftime('%Y-%m-%dT%H:%M:%S.%fZ')
    return f'\x1b[2m{stamp}\x1b[0m \x1b[32m INFO\x1b[0m Audio stream stats {styled}\n'


# CI run 39643467699 (pr65b): the stalled host dropped sequence 0 before the
# iPad's first packet and sequences 9-13 after it; the iPad lost five.
PR65B_APP = sample(200, lost=5, first=1) + sample(400, lost=5, first=1)
PR65B_HOST = host() + host(stale=5, queue=1, late='0,9-13')


class AudioMeasurementTests(unittest.TestCase):
    def test_accepts_sustained_tone_with_deliberate_quiet_window(self):
        result = measure(sample(50) + sample(100) + sample(150, tone=-120, rms=0))
        self.assertEqual(result['audio_decoded'], 150)
        self.assertEqual(result['audio_tone1k_db'], -12)

    def test_requires_rendered_tone_and_packet_count(self):
        for log in ('', sample(50) * 2, sample(tone=-120, rms=0) * 2, sample()):
            with self.subTest(log=log), self.assertRaises(ValueError):
                measure(log)

    def test_loss_must_not_be_hidden_by_a_later_counter_reset(self):
        for host_log in (None, host(late='none')):
            with self.subTest(host_log=host_log), self.assertRaises(ValueError):
                measure(sample(lost=3, first=0) + sample(lost=0, first=0), host_log, 'record')

    def test_evidence_without_sequence_numbers_keeps_the_original_limit(self):
        # Counts alone cannot show which packets the host dropped.
        for app, host_log in ((sample(lost=3) * 2, host(stale=3)),
                              (sample(lost=3, first=0) * 2, host(stale=3)),
                              (sample(lost=3) * 2, host(stale=3, late='0-2'))):
            with self.subTest(app=app, host=host_log), self.assertRaisesRegex(ValueError, 'exceeds 2'):
                measure(app, host_log, 'record')
        self.assertEqual(measure(sample(lost=2) * 2, host(stale=2))['audio_lost'], 2)

    def test_startup_deadline_drops_are_enforced_locally_and_recorded_on_ci(self):
        with self.assertRaisesRegex(ValueError, 'host dropped 6 startup audio packets'):
            measure(PR65B_APP, PR65B_HOST)
        result = measure(PR65B_APP, PR65B_HOST, scheduling_gate='record')
        self.assertEqual((result['audio_lost'], result['audio_host_late']), (5, 6))
        self.assertEqual(result['audio_host_late_seqs'], '0,9-13')
        self.assertEqual(len(result['audio_notes']), 1)
        two = measure(sample(400, lost=2, first=0) * 2, host(stale=2, late='30-31'))
        self.assertEqual(two['audio_host_late'], 2)

    def test_deadline_drops_after_startup_fail_on_ci_too(self):
        for late in ('260', '9-13,400-499'):
            with self.subTest(late=late), self.assertRaisesRegex(ValueError, 'after the first 5 s'):
                lost = 1 if late == '260' else 105
                measure(sample(400, lost=lost, first=0) * 2, host(stale=lost, late=late), 'record')

    def test_a_receiver_gap_the_host_did_not_drop_fails(self):
        # One extra iPad loss: the early drop of sequence 0 must not hide it.
        app = sample(200, lost=6, first=1) + sample(400, lost=6, first=1)
        for gate in ('enforce', 'record'):
            with self.subTest(gate=gate), self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
                measure(app, PR65B_HOST, gate)
        with self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
            measure(sample(400, lost=1, first=0) * 2, host(late='none'), 'record')

    def test_only_host_stats_logged_after_the_app_log_froze_attribute_loss(self):
        # Sol's case: the host log is still live. Its last line was snapshotted
        # before the transport dropped startup sequence 230, which the iPad had
        # already counted. That is incomplete evidence, not network loss.
        app = sample(200, first=0) + sample(240, lost=1, first=0)
        before = host(late='none')
        for gate in ('enforce', 'record'):
            with self.subTest(gate=gate), self.assertRaisesRegex(IncompleteEvidence, 'incomplete'):
                measure(app, before, gate, FROZEN)
        # The first line emitted after the freeze may hold an older snapshot;
        # the next line from the same stage cannot.
        after = host(stale=1, late='230', at=1)
        with self.assertRaisesRegex(IncompleteEvidence, '1 audio stats line'):
            measure(app, before + after, 'record', FROZEN)
        result = measure(app, before + after + host(stale=1, late='230', at=2), 'enforce', FROZEN)
        self.assertEqual(result['audio_host_late_seqs'], '230')

    def test_a_host_drop_the_receiver_never_missed_fails(self):
        with self.assertRaisesRegex(ValueError, 'iPad counted 0 losses'):
            measure(sample(400, first=0) * 2, host(stale=1, late='17'), 'record')

    def test_host_drops_after_the_frozen_app_log_are_outside_the_measurement(self):
        # The host keeps streaming during screenshots. Its later drops must
        # neither fail enforce nor cover an earlier receiver loss.
        app = sample(400, first=0) * 2
        late_host = host(stale=3, queue=1, late='400-402', failed='403', oversized=1)
        self.assertEqual(measure(app, late_host)['audio_host_late'], 0)
        with self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
            measure(sample(400, lost=1, first=0) * 2, host(stale=1, late='401'), 'record')

    def test_host_send_failures_always_fail(self):
        for failed in ('5', '5-6'):
            count = 1 if failed == '5' else 2
            with self.subTest(failed=failed), self.assertRaisesRegex(ValueError, 'host failed to send audio packets'):
                measure(sample(400, lost=count, first=0) * 2,
                        host(oversized=count, late='none', failed=failed), 'record')

    def test_unattributable_receiver_ranges_fail(self):
        cases = {
            'restarted': (sample(200, first=0) + sample(400, first=7), host(late='none')),
            'do not cover': (sample(400, first=0, after=420) * 2, host(late='none')),
            'truncated': (sample(400, first=0) * 2, host(late='0,+')),
        }
        for message, (app, host_log) in cases.items():
            with self.subTest(message), self.assertRaisesRegex(ValueError, message):
                measure(app, host_log, 'record')

    def test_packets_before_the_first_received_sequence_must_be_host_drops(self):
        # Sol's case: the host numbers from 0, so sequences 0-99 were sent
        # and vanished before the iPad's first packet.
        app = sample(400, lost=0, first=100, after=500) * 2
        for gate in ('enforce', 'record'):
            with self.subTest(gate=gate), self.assertRaisesRegex(ValueError, '100 audio packets before'):
                measure(app, host(late='none'), gate)
        # Host drops that cover the prefix are attributed.
        self.assertEqual(measure(app, host(stale=100, late='0-99'), 'record')['audio_host_late'], 100)

    def test_a_later_stream_cannot_attribute_an_earlier_loss(self):
        # The old stream's network loss fails against its own stats.
        app = sample(400, lost=1, first=0) * 2
        own = host(late='none')
        with self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
            measure(app, own, 'record')
        # A replacement stream after the app log froze, with a startup drop at
        # sequence 10, must not cover it. Under a new epoch it is ignored;
        # under the same epoch the loss cannot be attributed at all.
        with self.assertRaisesRegex(ValueError, '1 audio packets lost after'):
            measure(app, own + host(stale=1, late='10', stream=2, wire='7/4'), 'record')
        with self.assertRaisesRegex(ValueError, 'not attributable'):
            measure(app, own + host(stale=1, late='10', stream=2, wire='7/3'), 'record')
        with self.assertRaisesRegex(ValueError, 'no audio stats for session 7 epoch 3'):
            measure(app, host(stale=1, late='10', wire='8/3'), 'record')
        with self.assertRaisesRegex(ValueError, 'more than one wire stream'):
            measure(app, host(stale=1, late='10', wire='mixed') + own, 'record')

    def test_a_clean_capture_restart_outside_the_measurement_passes(self):
        # Sol's case: an audio off/on toggle after the app log froze gives two
        # loss-free captures under the iPad's session and epoch.
        app = sample(200, first=0) + sample(400, first=0)
        result = measure(app, host(late='none') + host(late='none', stream=2), 'enforce')
        self.assertEqual(result['audio_lost'], 0)
        self.assertNotIn('audio_host_late', result)
        self.assertRegex(result['audio_notes'][0], 'restarted audio numbering.*no loss to attribute')

    def test_ambiguous_captures_pass_only_without_loss(self):
        captures = host(stale=3, late='5-7') + host(late='none', stream=2)
        for lost in (1, 3):
            with self.subTest(lost=lost), self.assertRaisesRegex(ValueError, f'{lost} audio packets lost but not attributable'):
                measure(sample(400, lost=lost, first=0) * 2, captures, 'record')
        # Packets before the iPad's first one cannot be attributed either.
        with self.assertRaisesRegex(ValueError, '100 audio packets before the iPad.s first sequence 100 are missing but not attributable'):
            measure(sample(400, first=100, after=500) * 2, captures, 'record')

    def test_a_toggle_inside_the_measurement_fails_coverage(self):
        # Claude's probe: audio is toggled mid-measurement and capture A's
        # last five sent packets are lost in flight. B's discontinuity resets
        # the iPad timeline to sequence 0, so first_seq stays 0, but its
        # counters now cover more packets than its range.
        app = sample(200, first=0) + sample(395, first=0, after=195)
        captures = host(late='none') + host(late='none', stream=2)
        for gate in ('enforce', 'record'):
            with self.subTest(gate=gate), self.assertRaisesRegex(ValueError, 'do not cover'):
                measure(app, captures, gate)

    def test_sequence_wrap_fails_closed(self):
        with self.assertRaisesRegex(ValueError, 'malformed host sequence range'):
            measure(sample(400, lost=2, first=0) * 2, host(stale=2, late='4294967295-0'), 'record')
        app = sample(400, first=2**32 - 100) * 2
        with self.assertRaisesRegex(ValueError, 'wraps'):
            measure(app, host(late='none'), 'record')

    def test_host_log_without_audio_stats_fails(self):
        with self.assertRaisesRegex(ValueError, 'no audio stream stats'):
            measure(sample() * 2, 'host started\n')


if __name__ == '__main__':
    unittest.main()
