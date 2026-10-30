"""A pull request runs its own code, so no job a pull request can reach gets a secret
or an OIDC token."""
import pathlib
import re
import unittest

WORKFLOWS = pathlib.Path(__file__).resolve().parents[1] / ".github/workflows"
NOT_PR = "github.event_name != 'pull_request'"
# A value that is empty for a pull request.
GUARDED = re.compile(r"\$\{\{ " + re.escape(NOT_PR) + r" && secrets\.([A-Z0-9_]+) \|\| '' \}\}")


def jobs(text):
    """{job id: job text}; jobs sit two spaces under jobs:."""
    body = text.partition("\njobs:\n")[2]
    parts = re.split(r"(?m)^  ([A-Za-z0-9_-]+):\s*$", body)
    return dict(zip(parts[1::2], parts[2::2]))


def steps(job):
    return re.split(r"(?m)^(?=      - )", job)[1:]


def excludes_pr(condition):
    """The if: condition is false for a pull request: NOT_PR is one of its && terms."""
    return "||" not in condition and NOT_PR in [c.strip() for c in condition.split("&&")]


def condition(text, indent):
    """The if: of a job (indent 4) or a step (indent 6, its keys at 8)."""
    keys = " " * indent if indent == 4 else " " * indent + "(?:- |  )"
    match = re.search(rf"(?m)^{keys}if: (.*)$", text)
    return match[1].strip() if match else ""


def permissions(job):
    """A job's permissions: block as {scope: access}."""
    block = re.search(r"(?m)^    permissions:\n((?:      .*\n)+)", job)
    lines = block[1].splitlines() if block else []
    return dict(re.match(r" *([a-z-]+): *(\S+)", line).groups() for line in lines
                if not line.strip().startswith("#"))


def runs_on_pull_requests(text):
    head = text.partition("\njobs:\n")[0]
    return bool(re.search(r"(?m)^  pull_request(_target)?:|^on:.*\bpull_request\b", head))


def leaks(text):
    """The secrets a pull request run of this workflow would hand to a job, as
    (job, secret) pairs. GITHUB_TOKEN is the run's own token. A reusable workflow
    gets only what its caller passes in secrets:."""
    if not runs_on_pull_requests(text):
        return []
    head = text.partition("\njobs:\n")[0]
    found = [("(workflow)", name) for name in re.findall(r"secrets\.([A-Z0-9_]+)", head)]
    if "id-token: write" in head:
        found.append(("(workflow)", "id-token: write"))
    every = jobs(text)
    skipped = set()
    for job_id, job in every.items():
        needs = re.search(r"(?m)^    needs: *\[?([^\]\n]*)\]?", job)
        needed = {n.strip() for n in needs[1].split(",")} if needs else set()
        job_if = condition(job, 4)
        if excludes_pr(job_if) or (needed & skipped and "always()" not in job_if):
            skipped.add(job_id)
            continue
        if re.search(r"(?m)^    secrets:", job):  # passed to a reusable workflow
            found.append((job_id, "secrets:"))
        if "id-token: write" in job:
            found.append((job_id, "id-token: write"))
        parts = re.split(r"(?m)^(?=    steps:)", job, maxsplit=1)
        live = [parts[0]] + [step for step in steps(job) if not excludes_pr(condition(step, 6))]
        for part in live:
            unguarded = GUARDED.sub("", part)
            found += [(job_id, name) for name in re.findall(r"secrets\.([A-Z0-9_]+)", unguarded)
                      if name != "GITHUB_TOKEN"]
    return found


FIXTURE = """\
on:
  pull_request:
jobs:
  plan:
    runs-on: ubuntu-latest
    steps:
      - run: echo
  build:
    needs: plan
    env:
      KEY: ${{ github.event_name != 'pull_request' && secrets.KEY || '' }}
      # job
    steps:
      - name: Sign
        if: github.event_name != 'pull_request' && vars.SIGNING != ''
        with:
          id: ${{ secrets.AZURE_CLIENT_ID }}
      - name: Scan
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # scan
        run: scan
  release:
    needs: [plan, build]
    if: github.event_name != 'pull_request'
    steps:
      - run: echo ${{ secrets.RELEASE_KEY }}
  after:
    needs: release
    steps:
      - run: echo ${{ secrets.AFTER_KEY }}
"""


class WorkflowSecretsTest(unittest.TestCase):
    def test_nightly_pull_requests_get_no_secrets(self):
        text = (WORKFLOWS / "nightly.yml").read_text()
        self.assertTrue(runs_on_pull_requests(text))
        self.assertEqual(leaks(text), [])
        # Both builds call beta-installer.yml; only the one a pull request can't
        # reach passes it secrets and may sign.
        callers = {job_id: job for job_id, job in jobs(text).items() if "beta-installer.yml" in job}
        self.assertEqual(callers.keys(), {"build", "check"})
        self.assertTrue(excludes_pr(condition(callers["build"], 4)))
        self.assertIn("secrets: inherit", callers["build"])
        self.assertEqual(condition(callers["check"], 4), "github.event_name == 'pull_request'")
        installer = (WORKFLOWS / "beta-installer.yml").read_text()
        self.assertIn("workflow_call:", installer)
        self.assertNotIn("pull_request:", installer)
        # The installer's build job asks for an OIDC token for signing. A called
        # workflow gets at most its caller's permissions, so the trusted caller
        # grants id-token and the pull request caller grants only contents: read.
        self.assertEqual(permissions(jobs(installer)["build"]), {"contents": "read", "id-token": "write"})
        self.assertEqual(permissions(callers["build"]), {"contents": "read", "id-token": "write"})
        self.assertEqual(permissions(callers["check"]), {"contents": "read"})
        self.assertNotIn("secrets:", callers["check"])
        # It signs only outside pull requests, whatever the variables say.
        for step in steps(jobs(installer)["build"]):
            if "azure/login" in step or "INSTALLER_SIGN_COMMAND=" in step or "Get-AuthenticodeSignature $file" in step:
                self.assertTrue(excludes_pr(condition(step, 6)), step.splitlines()[0])

    def test_ci_pull_requests_get_no_secrets(self):
        self.assertEqual(leaks((WORKFLOWS / "ci.yml").read_text()), [])

    def test_the_fixture_is_clean(self):
        self.assertEqual(leaks(FIXTURE), [])

    def test_a_job_skipped_for_pull_requests_skips_its_dependents(self):
        self.assertEqual(jobs(FIXTURE).keys(), {"plan", "build", "release", "after"})
        text = FIXTURE.replace("    needs: release\n", "    needs: release\n    if: always()\n")
        self.assertEqual(leaks(text), [("after", "AFTER_KEY")])

    def test_unguarded_secrets_are_found(self):
        cases = {
            "      # job": ("build", "      OTHER: ${{ secrets.OTHER }}"),
            "    needs: plan\n    env": ("build", "    needs: plan\n    permissions:\n      id-token: write\n    env"),
            "          # scan": ("build", "          OTHER: ${{ secrets.OTHER }}"),
            "    needs: plan\n": ("build", "    needs: plan\n    secrets: inherit\n"),
        }
        for place, (job, line) in cases.items():
            with self.subTest(line=line):
                found = leaks(FIXTURE.replace(place, line))
                self.assertEqual([j for j, _ in found], [job])
        # A guard that also lets a pull request through is no guard.
        for condition_text in ("always() || github.event_name != 'pull_request'",
                               "github.event_name == 'pull_request' && vars.SIGNING != ''",
                               "vars.SIGNING != ''"):
            with self.subTest(condition=condition_text):
                text = FIXTURE.replace("github.event_name != 'pull_request' && vars.SIGNING != ''", condition_text)
                self.assertEqual(leaks(text), [("build", "AZURE_CLIENT_ID")])
        text = FIXTURE.replace("&& secrets.KEY || ''", "&& secrets.KEY || secrets.FALLBACK")
        self.assertEqual(leaks(text), [("build", "KEY"), ("build", "FALLBACK")])

    def test_workflows_without_pull_requests_are_not_checked(self):
        self.assertEqual(leaks(FIXTURE.replace("  pull_request:", "  push:")), [])


if __name__ == "__main__":
    unittest.main()
