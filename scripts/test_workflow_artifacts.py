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


def paths(step):
    block = re.search(r"(?m)^ +path: *(\|)?(.*)\n((?:^ {12,}.*\n?)*)", step)
    listed = block[3].split() if block[1] else [block[2].strip()]
    return [p.replace("\\", "/") for p in listed]


class WorkflowArtifactsTest(unittest.TestCase):
    def test_uploads_are_scanned_for_every_secret_their_job_has(self):
        uploads = 0
        for workflow in sorted(WORKFLOWS.glob("*.yml")):
            for job, head in jobs(workflow.read_text()):
                secrets = set(re.findall(r"secrets\.([A-Z0-9_]+)", head + job)) | {"GITHUB_TOKEN"}
                by_id = {m[1]: step for step in steps(job) if (m := re.search(r"(?m)^        id: (\S+)", step))}
                for step in steps(job):
                    if "uses: actions/upload-artifact" not in step:
                        continue
                    uploads += 1
                    name = f"{workflow.name}: {re.search(r'name: (.*)', step)[1]}"
                    with self.subTest(name):
                        gate = re.search(r"(?m)^        if: .*steps\.([\w-]+)\.outcome == 'success'", step)
                        self.assertTrue(gate, "upload must require a passing scan step")
                        scan = by_id.get(gate[1], "")
                        command = re.search(r"artifact_secrets\.py scan --secrets (\S+)(?: --optional (\S+))?(.*)",
                                            " ".join(scan.split()))
                        self.assertTrue(command, f"step {gate[1]} must run artifact_secrets.py scan")
                        optional = set((command[2] or "").split(",")) - {""}
                        self.assertLessEqual(secrets, set(command[1].split(",")) | optional)
                        self.assertIn("GITHUB_TOKEN: ${{ github.token }}", scan)
                        for secret in optional:
                            self.assertTrue("%s: ${{ secrets.%s }}" % (secret, secret) in head + job,
                                            f"{secret} must reach the scan step's env")
                        scanned = command[3].split()
                        for path in paths(step):
                            self.assertIn(path, scanned)
        self.assertGreaterEqual(uploads, 5)

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
