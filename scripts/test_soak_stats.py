import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from soak_stats import assess, assess_host_log, host_run_log

SCRIPTS = Path(__file__).parent
START = "2026-10-31T04:40:00Z  INFO Session log file initialized path=eternal-host-session.log\n"
SUMMARY = "2026-10-31T05:40:00Z  INFO session_summary v=1 role=pc session_id=7 nonce=00000000e2e00001 transport=wifi end={}\n"
EXPIRED = "2026-10-31T05:20:00Z  WARN Client liveness expired — tearing session down\n"
HELD = "2026-10-31T05:21:00Z  WARN Client silent — holding the session session_id=7 silent_ms=3000 last_heard=ping\n"
KEPT = ("2026-10-31T05:21:02Z  INFO Client heard again — session kept session_id=7 silent_ms=5200 held_ms=2200 "
        "heard=report\n")


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


class HostSessionLog(unittest.TestCase):
    def write(self, parts):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        log = Path(folder.name) / "eternal-host-session.log"
        for suffix, text in parts.items():
            log.with_name(log.name + suffix).write_text(text)
        return log

    def test_counts_read_the_rotated_parts_oldest_first(self):
        # A size rotation mid-soak moves the start and the expiry into .1; the
        # previous host run's start and summary were moved on to .2.
        log = self.write({".2": START + SUMMARY.format("shutdown"),
                          ".1": START + EXPIRED + "E2E_HOST_STATS captured=1",
                          "": " sent=2\n" + SUMMARY.format("liveness") + EXPIRED})
        text = host_run_log(log)
        self.assertTrue(text.startswith(START))
        self.assertIn("E2E_HOST_STATS captured=1 sent=2\n", text)
        self.assertNotIn("end=shutdown", text)
        result = assess_host_log(text)
        self.assertEqual((result["liveness_expiries"], result["session_summaries"]), (2, 1))
        self.assertEqual(len(result["errors"]), 2)

    def test_one_summary_at_shutdown_passes_only_without_reconnects_or_expiries(self):
        clean = START + SUMMARY.format("shutdown")
        self.assertEqual(assess_host_log(host_run_log(self.write({".1": START, "": clean})))["errors"], [])
        for text in (clean + EXPIRED, clean + "WARN Client reconnected — superseding session in place\n",
                     clean + SUMMARY.format("shutdown"), START + SUMMARY.format("liveness"), START):
            self.assertTrue(assess_host_log(text)["errors"], text)

    def test_a_hold_that_was_kept_is_counted_but_does_not_fail(self):
        result = assess_host_log(START + HELD + KEPT + SUMMARY.format("shutdown"))
        self.assertEqual(result["errors"], [])
        self.assertEqual((result["holds"], result["holds_kept"], result["liveness_expiries"]), (1, 1, 0))
        ended = assess_host_log(START + HELD + EXPIRED + SUMMARY.format("liveness"))
        self.assertEqual((ended["holds"], ended["holds_kept"], ended["liveness_expiries"]), (1, 0, 1))
        self.assertTrue(ended["errors"])

    def test_a_start_rotated_away_or_a_missing_log_fails(self):
        self.assertTrue(assess_host_log(host_run_log(self.write({".2": EXPIRED, ".1": EXPIRED,
                                                                "": SUMMARY.format("shutdown")})))["errors"])
        self.assertIsNone(host_run_log(Path(tempfile.gettempdir()) / "no-such-dir" / "eternal-host-session.log"))


class RealSoakRunner(unittest.TestCase):
    def test_host_stops_before_its_rotated_logs_are_collected_and_counted(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "scripts/win").mkdir(parents=True)
            (root / "build").mkdir()
            for name in ("soak.sh", "soak_stats.py"):
                shutil.copy(SCRIPTS / name, root / "scripts" / name)
            events = root / "events"
            samples = "\n".join(json.dumps(sample) for sample in trace()[:13])
            fakes = {
                # The host session rotated once: its start is in part 1.
                "win/remote.sh": f"""echo "remote $*" >> {events}
case "$*" in
    "session-log 1") printf '%s' '{START}' ;;
    "session-log 0") printf '%s' '{SUMMARY.format("shutdown")}' ;;
esac""",
                "e2e_ios.sh": f"""echo "e2e_ios stop_remote_host=$EM_STOP_REMOTE_HOST" >> {events}
echo '{samples}' > "$EM_OUTPUT_DIR/resources.jsonl"
echo '{{"status": "PASS"}}' > "$EM_OUTPUT_DIR/result.json"
""",
            }
            for name, body in fakes.items():
                (root / "scripts" / name).write_text("#!/bin/bash\n" + body)
                (root / "scripts" / name).chmod(0o755)
            out = root / "out"
            result = subprocess.run([str(root / "scripts/soak.sh"), "--real", "360"], capture_output=True,
                                    text=True, timeout=60, env=dict(os.environ, EM_OUTPUT_DIR=str(out)))
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(events.read_text().splitlines(), [
                "remote idle", "remote pattern start", "remote run-host ETERNAL_HEADLESS=1 ETERNAL_ENCODER=h264_nvenc "
                "ETERNAL_HEVC=0 ETERNAL_FPS=60 ETERNAL_MAX_DGRAM=1200 ETERNAL_E2E_LOG=1 ETERNAL_USB_DIRECT=127.0.0.1:0",
                "e2e_ios stop_remote_host=1", "remote stop-host", "remote log",
                "remote session-log 2", "remote session-log 1", "remote session-log 0", "remote pattern stop"])
            host = json.loads((out / "report.json").read_text())["host_log"]
            self.assertEqual((host["session_summaries"], host["liveness_expiries"]), (1, 0))
