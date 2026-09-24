import unittest
from soak_stats import assess


def trace():
    return [dict(elapsed=t, decoded=t*60, host_rss_kib=100_000, app_rss_kib=200_000)
            for t in range(0, 1801, 30)]


class SoakMeasurements(unittest.TestCase):
    def test_stable_stream_passes_the_thirty_minute_gate(self):
        result = assess(trace(), 1800)
        self.assertEqual(result['status'], 'PASS')
        self.assertEqual(result['samples_at_55_fps'], 60)

    def test_memory_baseline_excludes_first_five_minutes(self):
        samples = trace()
        samples[0]['host_rss_kib'] = 50_000
        samples[-1]['host_rss_kib'] = 119_999
        self.assertEqual(assess(samples, 1800)['status'], 'PASS')
        samples[-1]['host_rss_kib'] = 120_000
        self.assertEqual(assess(samples, 1800)['status'], 'FAIL')

    def test_physical_ipad_soak_checks_host_memory_only(self):
        samples = trace()
        for sample in samples:
            del sample['app_rss_kib']
        self.assertEqual(assess(samples, 1800)['status'], 'FAIL')
        result = assess(samples, 1800, processes=('host',))
        self.assertEqual(result['status'], 'PASS')
        self.assertEqual(list(result['memory']), ['host'])
        samples[-1]['host_rss_kib'] = 120_000
        self.assertEqual(assess(samples, 1800, processes=('host',))['status'], 'FAIL')

    def test_stalled_decoder_cannot_pass_using_its_last_logged_fps(self):
        samples = trace()
        for sample in samples[-5:]:
            sample['decoded'] = samples[-6]['decoded']
        self.assertEqual(assess(samples, 1800)['status'], 'FAIL')

    def test_exactly_ninety_five_percent_good_samples_passes(self):
        samples = trace()
        for i, sample in enumerate(samples):
            sample['decoded'] -= min(i, 3)*300
        self.assertEqual(assess(samples, 1800)['status'], 'PASS')
        samples[-1]['decoded'] -= 300
        self.assertEqual(assess(samples, 1800)['status'], 'FAIL')

    def test_restart_missing_process_gap_and_short_run_fail(self):
        for change in ('reset', 'missing', 'gap', 'short'):
            samples = trace()
            if change == 'reset': samples[-1]['decoded'] = 60
            if change == 'missing': samples[-1]['app_rss_kib'] = 0
            if change == 'gap': samples.pop(20)
            if change == 'short': samples.pop()
            self.assertEqual(assess(samples, 1800)['status'], 'FAIL', change)
