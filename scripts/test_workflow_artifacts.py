"""Every artifact upload must wait for artifact_secrets.py to pass on the same paths."""
import pathlib
import re
import unittest

WORKFLOWS = pathlib.Path(__file__).resolve().parents[1] / ".github/workflows"


def jobs(text):
    """(job text, top-level text) pairs; jobs sit two spaces under jobs:."""
    head, _, body = text.partition("\njobs:\n")
    return [(job, head) for job in re.split(r"(?m)^(?=  [A-Za-z0-9_-]+:\s*$)", body) if job.strip()]


def steps(job):
    return re.split(r"(?m)^(?=      - )", job)[1:]


def step_env(step):
    """(name, value) pairs bound directly in the step's own env: block. Comments and
    lines nested deeper, such as block scalar contents, are not bindings."""
    block = re.search(r"(?m)^        env:\n((?:(?: {10}.*)?\n)*)", step + "\n")
    return re.findall(r"(?m)^ {10}([\w-]+): *(.*?) *$", block[1] if block else "")


def paths(step):
    block = re.search(r"(?m)^ +path: *(\|)?(.*)\n((?:^ {12,}.*\n?)*)", step)
    listed = block[3].split() if block[1] else [block[2].strip()]
    return [p.replace("\\", "/") for p in listed]


# An upload whose job has one optional secret. Replacing a "# place" comment
# binds the secret in that env: block.
FIXTURE = """\
env:
  # workflow
jobs:
  build:
    env:
      # job
    steps:
      - name: Build the installer
        env:
          # build
        run: build
      - name: Check the installer artifact for secret values
        id: artifact-scan
        env:
          GITHUB_TOKEN: ${{ github.token }}
          # scan
        run: >-
          python scripts/artifact_secrets.py scan
          --secrets GITHUB_TOKEN
          --optional AZURE_CLIENT_ID
          out/setup.exe
      - name: Upload installer artifact
        if: always() && steps.artifact-scan.outcome == 'success'
        uses: actions/upload-artifact@v4
        with:
          name: setup
          path: out/setup.exe
"""


class WorkflowArtifactsTest(unittest.TestCase):
    def assert_uploads_scanned(self, text):
        """Every upload in the workflow waits for a passing scan of its paths for every
        secret its job has. Returns the number of uploads."""
        uploads = 0
        for job, head in jobs(text):
            secrets = set(re.findall(r"secrets\.([A-Z0-9_]+)", head + job)) | {"GITHUB_TOKEN"}
            by_id = {m[1]: step for step in steps(job) if (m := re.search(r"(?m)^        id: (\S+)", step))}
            for step in steps(job):
                if "uses: actions/upload-artifact" not in step:
                    continue
                uploads += 1
                name = re.search(r"name: (.*)", step)[1]
                gate = re.search(r"(?m)^        if: .*steps\.([\w-]+)\.outcome == 'success'", step)
                self.assertTrue(gate, f"{name}: upload must require a passing scan step")
                scan = by_id.get(gate[1], "")
                command = re.search(r"artifact_secrets\.py scan --secrets (\S+)(?: --optional (\S+))?(.*)",
                                    " ".join(scan.split()))
                self.assertTrue(command, f"{name}: step {gate[1]} must run artifact_secrets.py scan")
                optional = set((command[2] or "").split(",")) - {""}
                self.assertLessEqual(secrets, set(command[1].split(",")) | optional, name)
                self.assertIn("GITHUB_TOKEN: ${{ github.token }}", scan, name)
                bound = step_env(scan)
                for secret in optional:
                    self.assertEqual([value for key, value in bound if key == secret],
                                     ["${{ secrets.%s }}" % secret],
                                     f"{name}: the scan step's env must bind {secret} to its secret")
                scanned = command[3].split()
                for path in paths(step):
                    self.assertIn(path, scanned, name)
        return uploads

    def test_uploads_are_scanned_for_every_secret_their_job_has(self):
        uploads = 0
        for workflow in sorted(WORKFLOWS.glob("*.yml")):
            with self.subTest(workflow.name):
                uploads += self.assert_uploads_scanned(workflow.read_text())
        self.assertGreaterEqual(uploads, 5)

    def test_optional_secrets_must_be_bound_in_the_scan_step(self):
        binding = "AZURE_CLIENT_ID: ${{ secrets.AZURE_CLIENT_ID }}"
        self.assertEqual(self.assert_uploads_scanned(FIXTURE.replace("# scan", binding)), 1)
        rejected = {
            "workflow only": FIXTURE.replace("# workflow", binding),
            "job only": FIXTURE.replace("# job", binding),
            "another step": FIXTURE.replace("# build", binding),
            "commented out": FIXTURE.replace("# scan", "# " + binding),
            "block scalar": FIXTURE.replace("# scan", "NOTE: |\n            " + binding),
            "empty": FIXTURE.replace("# scan", 'AZURE_CLIENT_ID: ""'),
            "literal": FIXTURE.replace("# scan", "AZURE_CLIENT_ID: abc123"),
            "other secret": FIXTURE.replace("# scan", "AZURE_CLIENT_ID: ${{ secrets.GITHUB_TOKEN }}"),
            "empty over job": FIXTURE.replace("# job", binding).replace("# scan", 'AZURE_CLIENT_ID: ""'),
        }
        for case, text in rejected.items():
            with self.subTest(case), self.assertRaisesRegex(AssertionError, "must bind AZURE_CLIENT_ID"):
                self.assert_uploads_scanned(text)

    def test_testflight_review_runs_only_for_tags_and_manual_runs(self):
        text = (WORKFLOWS / "testflight.yml").read_text()
        review = next(job for job, _ in jobs(text) if job.startswith("  review:"))
        condition = re.search(r"(?m)^    if: >-\n((?:^      .*\n)+)", review)
        self.assertTrue(condition, "the review job needs a job-level if")
        condition = " ".join(condition[1].split())
        self.assertEqual(condition, "github.event_name == 'workflow_dispatch' || "
                                    "(github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v'))")
        self.assertIn("    needs: archive\n", review)


if __name__ == "__main__":
    unittest.main()
