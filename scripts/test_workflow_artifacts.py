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


def env(text, indent):
    """The env: block at this indent: workflow 0, job 4, step 8."""
    block = re.search(r"(?m)^%senv:\n((?:(?: {%d}.*)?\n)*)" % (" " * indent, indent + 2), text + "\n")
    return block[1] if block else ""


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
                visible = env(head, 0) + env(job, 4) + env(scan, 8)
                for secret in optional:
                    self.assertTrue("%s: ${{ secrets.%s }}" % (secret, secret) in visible,
                                    f"{name}: {secret} must reach the scan step's env")
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

    def test_optional_secrets_must_be_bound_where_the_scan_step_sees_them(self):
        binding = "AZURE_CLIENT_ID: ${{ secrets.AZURE_CLIENT_ID }}"
        for place in ("workflow", "job", "scan"):
            with self.subTest(place):
                self.assertEqual(self.assert_uploads_scanned(FIXTURE.replace(f"# {place}", binding)), 1)
        with self.assertRaisesRegex(AssertionError, "AZURE_CLIENT_ID must reach the scan step's env"):
            self.assert_uploads_scanned(FIXTURE.replace("# build", binding))

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
