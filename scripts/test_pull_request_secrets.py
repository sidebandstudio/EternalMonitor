"""A pull request runs its own scripts, so no job a pull request can run may hold a
secret other than GITHUB_TOKEN, pass secrets on to a reusable workflow, or get an
OIDC token.

Only a job-level if: keeps a job's secrets away. The runner gets every secret its
job references and evaluates step conditions and expressions itself, so a step
skipped for pull requests, or ${{ ... && secrets.X || '' }}, still hands the job's
secrets to the code a pull request checked out."""
import pathlib
import re
import unittest

WORKFLOWS = pathlib.Path(__file__).resolve().parents[1] / ".github/workflows"
PR_EVENTS = ("pull_request", "pull_request_target")
TOKEN = re.compile(r"\s*(\|\||&&|==|!=|!|\(|\)|'(?:[^']|'')*'|[A-Za-z_][\w.-]*(?:\((?:[^()']|'[^']*')*\))?)")
# secrets.NAME, secrets['NAME'] and toJSON(secrets) inside ${{ }}.
EXPRESSION = re.compile(r"\$\{\{(.*?)\}\}")
SECRET = re.compile(r"\bsecrets(?:\.([A-Za-z0-9_]+)|\s*\[|\s*\))")


def code(text):
    """The text without comment lines."""
    return re.sub(r"(?m)^\s*#.*\n?", "", text)


def jobs(text):
    """{job id: job text}; jobs sit two spaces under jobs:."""
    body = code(text).partition("\njobs:\n")[2]
    parts = re.split(r"(?m)^  ([A-Za-z0-9_-]+):\s*$", body)
    return dict(zip(parts[1::2], parts[2::2]))


def pr_events(text):
    head = code(text).partition("\njobs:\n")[0]
    on = re.search(r"(?m)^(?:on|\"on\"|'on'):(.*\n(?:^[ -].*\n)*)", head)
    return [e for e in PR_EVENTS if on and re.search(rf"(?<![\w-]){e}(?![\w-])", on[1])]


def job_if(job):
    """A job's own if: (four spaces in), folded onto one line, without ${{ }}."""
    match = re.search(r"(?m)^    if: *(?:[>|][-+]?\n((?:^      .*\n?)+)|(.*)$)", job)
    expression = " ".join((match[1] or match[2]).split()) if match else ""
    wrapped = re.fullmatch(r"\$\{\{(.*)\}\}", expression)
    return wrapped[1].strip() if wrapped else expression


def value(expression, event):
    """The expression with github.event_name == event: True, False, or None when it
    depends on anything else or is not understood (three-valued logic)."""
    tokens, pos, text = [], 0, expression.strip()
    while pos < len(text):
        match = TOKEN.match(text, pos)
        if not match:
            return None
        tokens.append(match[1])
        pos = match.end()
    tokens.append("")

    def atom(i):
        if tokens[i] in ("", ")", "||", "&&", "==", "!="):
            raise ValueError(expression)
        if tokens[i] == "(":
            result, i = either(i + 1)
            if tokens[i] != ")":
                raise ValueError(expression)
            return result, i + 1
        if tokens[i].startswith("'"):
            return ("str", tokens[i][1:-1].replace("''", "'").lower()), i + 1
        if tokens[i] == "github.event_name":
            return ("str", event), i + 1
        return ("unknown",), i + 1

    def truth(operand):
        return operand if operand in (True, False, None) else bool(operand[1]) if operand[0] == "str" else None

    def compare(i):
        left, i = atom(i)
        if tokens[i] not in ("==", "!="):
            return truth(left), i
        op, (right, i) = tokens[i], atom(i + 1)
        if left and right and left[0] == right[0] == "str":
            return (left[1] == right[1]) == (op == "=="), i
        return None, i

    def negation(i):
        if tokens[i] == "!":
            result, i = negation(i + 1)
            return (None if result is None else not result), i
        return compare(i)

    def chain(i, operator, operand, stop):
        results = []
        result, i = operand(i)
        results.append(result)
        while tokens[i] == operator:
            result, i = operand(i + 1)
            results.append(result)
        if stop in results:
            return stop, i
        return (None if None in results else not stop), i

    def both(i):
        return chain(i, "&&", negation, False)

    def either(i):
        return chain(i, "||", both, True)

    try:
        result, end = either(0)
    except ValueError:
        return None
    return result if end == len(tokens) - 1 else None


def skipped(text, event):
    """The jobs that never run for event: their if: is false, or they need a skipped
    job and their if: has no always(), failure() or cancelled()."""
    every = jobs(text)
    out = {job_id for job_id, job in every.items() if job_if(job) and value(job_if(job), event) is False}
    while True:
        more = set()
        for job_id, job in every.items():
            needs = re.search(r"(?m)^    needs: *(.*)\n((?:^      - .*\n)*)", job)
            needed = set(re.findall(r"[\w-]+", needs[1] + needs[2])) if needs else set()
            if job_id not in out and needed & out and not re.search(r"\b(always|failure|cancelled)\(", job_if(job)):
                more.add(job_id)
        if not more:
            return out
        out |= more


def holds(text):
    """What this job or workflow head would hand its runner beyond GITHUB_TOKEN."""
    found = [name or "secrets" for expression in EXPRESSION.findall(text)
             for name in SECRET.findall(expression) if name != "GITHUB_TOKEN"]
    if re.search(r"(?m)^    secrets:", text):
        found.append("secrets: (to a reusable workflow)")
    if re.search(r"\bid-token: *write\b|\bpermissions: *write-all\b", text):
        found.append("id-token: write")
    return found


def leaks(text):
    """(job, what) for everything a pull request run of the workflow could reach."""
    events = pr_events(text)
    if not events:
        return []
    found = [("(workflow)", what) for what in holds(code(text).partition("\njobs:\n")[0])]
    never = set.intersection(*(skipped(text, event) for event in events))
    for job_id, job in jobs(text).items():
        if job_id not in never:
            found += [(job_id, what) for what in holds(job)]
    return found


FIXTURE = """\
on:
  push:
    tags: ["v*"]
  pull_request:
permissions:
  contents: read
jobs:
  check:
    if: github.event_name == 'pull_request'
    env:
      # job
      TOKEN: ${{ secrets.GITHUB_TOKEN }}
    steps:
      - name: Build
        env:
          # step
          GITHUB_TOKEN: ${{ github.token }}
        run: echo "no secrets here"
  archive:
    if: github.event_name != 'pull_request'
    permissions:
      contents: read
      id-token: write
    steps:
      - run: sign
        env:
          KEY: ${{ secrets.KEY }}
  review:
    needs: [archive]
    if: >-
      github.event_name == 'workflow_dispatch' ||
      (github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v'))
    steps:
      - run: echo ${{ secrets.REVIEW_KEY }}
  after:
    needs:
      - review
    steps:
      - run: echo ${{ secrets.AFTER_KEY }}
"""


class PullRequestSecretsTest(unittest.TestCase):
    def test_pull_requests_get_no_secrets(self):
        checked = [workflow.name for workflow in sorted(WORKFLOWS.glob("*.yml")) if pr_events(workflow.read_text())]
        self.assertIn("ci.yml", checked)
        self.assertIn("testflight.yml", checked)
        for workflow in sorted(WORKFLOWS.glob("*.yml")):
            with self.subTest(workflow.name):
                self.assertEqual(leaks(workflow.read_text()), [])

    def test_testflight_builds_pull_requests_in_their_own_job(self):
        text = (WORKFLOWS / "testflight.yml").read_text()
        every = jobs(text)
        self.assertEqual(skipped(text, "pull_request"), {"archive", "review"})
        self.assertEqual(skipped(text, "push"), {"pull-request"})
        self.assertIn("scripts/testflight-local.sh --dry-run", every["pull-request"])
        self.assertIn("secrets.ASC_KEY_P8", every["archive"])
        # Dropping the trusted job's guard is caught.
        unguarded = text.replace("    if: github.event_name != 'pull_request'\n", "")
        self.assertIn(("archive", "ASC_KEY_P8"), leaks(unguarded))
        # review keeps pull requests out by its own if:, not only through archive.
        self.assertIs(value(job_if(every["review"]), "pull_request"), False)

    def test_the_fixture_is_clean(self):
        self.assertEqual(leaks(FIXTURE), [])
        self.assertEqual(skipped(FIXTURE, "pull_request"), {"archive", "review", "after"})

    def test_secrets_in_a_pull_request_job_are_found(self):
        cases = {
            "job env": ("      # job", "      KEY: ${{ secrets.KEY }}", "KEY"),
            "step env": ("          # step", "          KEY: ${{ secrets.KEY }}", "KEY"),
            "skipped step": ("          # step", "          KEY: ${{ secrets.KEY }}\n        if: github.event_name != 'pull_request'", "KEY"),
            "guarded value": ("          # step", "          KEY: ${{ github.event_name != 'pull_request' && secrets.KEY || '' }}", "KEY"),
            "index": ("          # step", "          KEY: ${{ secrets['KEY'] }}", "secrets"),
            "every secret": ("          # step", "          ALL: ${{ toJSON(secrets) }}", "secrets"),
            "inherit": ("    env:\n      # job", "    uses: ./.github/workflows/x.yml\n    secrets: inherit\n    env:", "secrets: (to a reusable workflow)"),
            "passed": ("    env:\n      # job", "    secrets:\n      KEY: ${{ github.token }}\n    env:", "secrets: (to a reusable workflow)"),
            "oidc": ("    env:\n      # job", "    permissions:\n      id-token: write\n    env:", "id-token: write"),
            "write-all": ("    env:\n      # job", "    permissions: write-all\n    env:", "id-token: write"),
        }
        for case, (place, line, what) in cases.items():
            with self.subTest(case):
                self.assertEqual(leaks(FIXTURE.replace(place, line, 1)), [("check", what)])

    def test_secrets_at_the_workflow_level_are_found(self):
        for line in ("env:\n  KEY: ${{ secrets.KEY }}", "permissions:\n  id-token: write", "permissions: write-all"):
            with self.subTest(line):
                text = FIXTURE.replace("permissions:\n  contents: read", line)
                self.assertEqual(leaks(text)[0][0], "(workflow)")

    def test_a_guard_a_pull_request_can_pass_is_no_guard(self):
        guard = "    if: github.event_name != 'pull_request'\n"
        for condition in ("", "always() || github.event_name != 'pull_request'",
                          "github.event_name == 'pull_request' || vars.SIGN != ''", "vars.SIGN != ''",
                          "!(github.event_name == 'push')", "github.event_name != 'pull_request' ||",
                          "${{ github.event_name != 'push' }}", "github.event_name != 'PULL_REQUEST' || true"):
            with self.subTest(condition=condition):
                text = FIXTURE.replace(guard, f"    if: {condition}\n" if condition else "")
                self.assertIn(("archive", "KEY"), leaks(text))
        for condition in ("github.event_name == 'push' && vars.SIGN != ''", "!(github.event_name == 'pull_request')",
                          "${{ github.event_name != 'pull_request' }}", "success() && github.event_name != 'Pull_Request'",
                          "github.event_name == 'push' || github.event_name == 'workflow_dispatch'"):
            with self.subTest(condition=condition):
                self.assertEqual(leaks(FIXTURE.replace(guard, f"    if: {condition}\n")), [])

    def test_a_dependent_that_can_run_after_a_skip_is_checked(self):
        for condition in ("always()", "${{ !cancelled() }}", "failure()"):
            with self.subTest(condition):
                text = FIXTURE.replace("      - review\n", f"      - review\n    if: {condition}\n")
                self.assertEqual(leaks(text), [("after", "AFTER_KEY")])

    def test_every_pull_request_trigger_is_checked(self):
        for on in ("on: [push, pull_request]", "on: pull_request_target", "on:\n  pull_request_target:\n    types: [opened]"):
            with self.subTest(on):
                text = re.sub(r"(?s)^on:.*?(?=permissions:)", on + "\n", FIXTURE)
                self.assertTrue(pr_events(text))
                self.assertIn(("archive", "KEY"), leaks(text.replace("    if: github.event_name != 'pull_request'\n", "")))
        for on in ("on: [push, workflow_dispatch]", "on:\n  push:\n    branches: [pull_request_fix]"):
            with self.subTest(on):
                self.assertEqual(pr_events(re.sub(r"(?s)^on:.*?(?=permissions:)", on + "\n", FIXTURE)), [])


if __name__ == "__main__":
    unittest.main()
