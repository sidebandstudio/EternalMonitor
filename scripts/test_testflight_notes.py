"""TestFlight notes come from the release notes, without the release date."""
import pathlib
import subprocess
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import testflight_notes as tn  # noqa: E402

SOURCE = """## EternalMonitor v0.3.4

Released [date set at release]. Windows and iPad versions listed on the
download page work together.

### What to test

Move windows.

## EternalMonitor v0.3.3

Released October 27, 2026. Older notes.
"""


class TestflightNotesTest(unittest.TestCase):
    def test_the_date_placeholder_never_reaches_testers(self):
        notes = tn.notes("0.3.4", SOURCE)
        self.assertNotIn("Released", notes)
        self.assertNotIn("[date", notes)
        self.assertTrue(notes.startswith("# EternalMonitor 0.3.4: What to Test\n\nWindows and iPad versions listed on the\n"), notes)
        self.assertTrue(notes.endswith("Move windows.\n"), notes)

    def test_a_real_date_is_left_out_too(self):
        notes = tn.notes("0.3.3", SOURCE)
        self.assertEqual(notes, "# EternalMonitor 0.3.3: What to Test\n\nOlder notes.\n")

    def test_notes_over_the_limit_are_refused(self):
        long = SOURCE.replace("Move windows.", "x" * tn.LIMIT)
        with self.assertRaises(SystemExit):
            tn.notes("0.3.4", long)
        with self.assertRaises(SystemExit):
            tn.notes("0.3.5", SOURCE)

    def test_the_committed_notes_are_current(self):
        result = subprocess.run([sys.executable, str(pathlib.Path(tn.__file__)), "--check"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        committed = (pathlib.Path(tn.__file__).resolve().parents[1] / "ios/TESTFLIGHT_NOTES.md").read_text()
        self.assertNotIn("[date set at release]", committed)


if __name__ == "__main__":
    unittest.main()
