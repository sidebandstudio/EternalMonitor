"""The Duo upload is skipped only when every change is known not to reach the app."""
import subprocess
import unittest
from unittest.mock import patch

import duo_testflight_build as duo

COMMIT = "a" * 40
UPLOADED = "b" * 40


class AppFilesTests(unittest.TestCase):
    def test_tests_docs_other_workflows_and_the_pc_side_are_not_the_app(self):
        paths = [
            "ios/EternalMonitorUITests/DuoFoldTests.swift",
            "ios/EternalMonitorUITests/DuoHinge.m",
            "ios/EternalMonitorTests/PictureFitTests.swift",
            "docs/index.html",
            "notes/0.3.3-pending-doc-notes.md",
            "README.md",
            "ios/TESTFLIGHT_NOTES.md",
            ".github/pull_request_template.md",
            ".github/workflows/ci.yml",
            ".github/workflows/release.yml",
            ".github/workflows/testflight.yml",
            "host/src/main.rs",
            "proto/src/lib.rs",
            "Cargo.lock",
        ]
        self.assertEqual(duo.app_files(paths), [])

    def test_the_duo_pipeline_and_app_build_inputs_are_the_app(self):
        paths = [
            ".github/workflows/duo.yml",
            "scripts/duo-testflight.sh",
            "scripts/duo_testflight_build.py",
            "scripts/check-versions.sh",
            "ios/project.yml",
            "ios/EternalMonitor.xcodeproj/project.pbxproj",
            "ios/EternalMonitor/EternalMonitor.entitlements",
            "ios/EternalMonitor/Info.plist",
            "ios/exportOptions.plist",
            "ios/EternalMonitor/App/DisplayView.swift",
            # The app bundles whatever its source folder holds.
            "ios/EternalMonitor/Resources/Notes.md",
        ]
        self.assertEqual(duo.app_files(paths), paths)

    def test_a_path_the_list_does_not_know_is_the_app(self):
        paths = [".github/workflows/new.yml", ".github/CODEOWNERS", ".gitignore", "Makefile",
                 "ios/EternalMonitorUITestsHelper.swift", "scripts/test_ios.sh"]
        self.assertEqual(duo.app_files(paths), paths)


class AppChangesTests(unittest.TestCase):
    def diff(self, *paths, code=0):
        return subprocess.CompletedProcess(["git"], code, "".join(path + "\n" for path in paths), "")

    @patch("duo_testflight_build.subprocess.run")
    @patch("duo_testflight_build.uploaded_source", return_value=(UPLOADED, "27A9275"))
    def test_skips_only_when_nothing_reaches_the_app(self, _source, run):
        run.return_value = self.diff("ios/EternalMonitorUITests/DuoFoldTests.swift", "docs/index.html")
        self.assertIsNone(duo.app_changes(COMMIT, "27A9275"))
        self.assertEqual(run.call_args.args[0],
                         ["git", "diff", "--name-only", "--no-renames", UPLOADED, COMMIT])

    @patch("duo_testflight_build.subprocess.run")
    @patch("duo_testflight_build.uploaded_source", return_value=(UPLOADED, "27A9275"))
    def test_uploads_for_a_workflow_or_script_change(self, _source, run):
        for path in [".github/workflows/duo.yml", "scripts/duo-testflight.sh"]:
            run.return_value = self.diff("docs/index.html", path)
            self.assertIn(path, duo.app_changes(COMMIT, "27A9275"))

    @patch("duo_testflight_build.subprocess.run")
    @patch("duo_testflight_build.uploaded_source")
    def test_uploads_when_unsure(self, source, run):
        source.return_value = None
        self.assertIn("doesn't name the commit", duo.app_changes(COMMIT, "27A9275"))
        source.return_value = (UPLOADED, "27A9275")
        self.assertIn("not 27B1", duo.app_changes(COMMIT, "27B1"))
        run.return_value = self.diff(code=128)
        self.assertIn("can't be compared", duo.app_changes(COMMIT, "27A9275"))


if __name__ == "__main__":
    unittest.main()
