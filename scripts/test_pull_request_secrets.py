"""A pull request runs its own scripts, so no job a pull request can run may hold a
secret other than GITHUB_TOKEN, pass secrets on to a reusable workflow, or get an
OIDC token.

Only a job-level if: keeps a job's secrets away. The runner gets every secret its
job references and evaluates step conditions and expressions itself, so a step
skipped for pull requests, or ${{ ... && secrets.X || '' }}, still hands the job's
secrets to the code a pull request checked out.

The workflows are read line by line. A layout that reading could get wrong raises
ValueError, and an if: it cannot evaluate counts as one that runs."""
import pathlib
import re
import unittest

WORKFLOWS = pathlib.Path(__file__).resolve().parents[1] / ".github/workflows"
PR_EVENTS = ("pull_request", "pull_request_target")
TOKEN = re.compile(r"\s*(\|\||&&|==|!=|!|\(|\)|'(?:[^']|'')*'|[A-Za-z_][\w.-]*(?:\((?:[^()']|'[^']*')*\))?)")
# Any mention of secrets inside ${{ }}, which may span lines. Contexts ignore case.
EXPRESSION = re.compile(r"\$\{\{(.*?)\}\}", re.S)
SECRET = re.compile(r"\bsecrets\b(?:\s*\.\s*([\w-]+))?", re.I)
# Any of these in an if: stops GitHub adding its implicit success().
STATUS = re.compile(r"\b(?:success|always|failure|cancelled)\s*\(", re.I)
# A YAML alias or merge key could bring a secret in from elsewhere.
ALIAS = re.compile(r"(?m)(?::|^\s*-)[ \t]+\*[\w-]+[ \t]*$|[\[,{][ \t]*\*[\w-]|<<[ \t]*:")


def code(text):
    """The text without comment lines."""
    return re.sub(r"(?m)^\s*#.*\n?", "", text)


def sections(text):
    """(the workflow without jobs:, the body of jobs:)."""
    if ALIAS.search(text):
        raise ValueError("YAML alias")
    head, body, inside, seen = [], [], False, 0
    for line in text.splitlines(keepends=True):
        if line.strip() and not line[0].isspace() and line[0] != "#":
            inside = line.rstrip() == "jobs:"
            seen += inside
            if seen > 1 or not inside and re.match(r"[\"']?jobs\b", line):
                raise ValueError(f"unparsed {line.strip()}")
            if inside:
                continue
        (body if inside else head).append(line)
    return "".join(head), "".join(body)


def jobs(text):
    """{job id: job text}. Each id sits two spaces under jobs: and its job's keys
    four; any other key under jobs: raises, so no job goes unread."""
    out, lines, started = {}, None, False
    for line in sections(text)[1].splitlines(keepends=True):
        indent = len(line) - len(line.lstrip(" "))
        if line.strip() and not line.lstrip().startswith("#"):
            if indent < 4:
                key = re.fullmatch(r"  ([A-Za-z_][\w-]*):[ \t]*\n?", line)
                if not key or key[1] in out:
                    raise ValueError(f"unparsed job key {line.strip()}")
                lines, started = [], False
                out[key[1]] = lines
                continue
            if lines is None or not started and indent != 4:
                raise ValueError(f"unparsed {line.strip()}")
            started = True
        if lines is not None:
            lines.append(line)
    return {job_id: "".join(lines) for job_id, lines in out.items()}


def pr_events(text):
    head = code(sections(text)[0])
    on = re.search(r"(?m)^(?:on|\"on\"|'on'):(.*\n(?:^[ -].*\n)*)", head)
    return [e for e in PR_EVENTS if on and re.search(rf"(?<![\w-]){e}(?![\w-])", on[1])]


def job_if(job):
    """A job's own if: (four spaces in), folded onto one line, without YAML quotes
    or ${{ }}."""
    match = re.search(r"(?m)^    if: *(?:[>|][-+]?\n((?:^      .*\n?)+)|(.*)$)", job)
    expression = " ".join((match[1] or match[2]).split()) if match else ""
    quoted = re.fullmatch(r"'((?:[^']|'')*)'|\"([^\"\\]*)\"", expression)
    if quoted:
        expression = quoted[1].replace("''", "'") if quoted[2] is None else quoted[2]
    wrapped = re.fullmatch(r"\$\{\{(.*)\}\}", expression)
    return wrapped[1].strip() if wrapped else expression


def value(expression, event):
    """The expression with github.event_name == event: True, False, or None when it
    depends on anything else or is not understood (three-valued logic). As in
    GitHub, ! binds tighter than == and !=, then && and ||."""
    tokens, pos, text = [], 0, expression.strip()
    while pos < len(text):
        match = TOKEN.match(text, pos)
        if not match:
            return None
        tokens.append(match[1])
        pos = match.end()
    tokens.append("")
    unknown = ("unknown", None)

    def atom(i):
        """(kind, value) of one operand, kind "str", "bool" or "unknown"."""
        if tokens[i] in ("", ")", "||", "&&", "==", "!="):
            raise ValueError(expression)
        if tokens[i] == "!":
            (kind, operand), i = atom(i + 1)
            return (unknown if kind == "unknown" else ("bool", not operand)), i
        if tokens[i] == "(":
            result, i = either(i + 1)
            if tokens[i] != ")":
                raise ValueError(expression)
            return (unknown if result is None else ("bool", result)), i + 1
        if tokens[i].startswith("'"):
            return ("str", tokens[i][1:-1].replace("''", "'").lower()), i + 1
        if tokens[i].lower() == "github.event_name":
            return ("str", event), i + 1
        return unknown, i + 1

    def compare(i):
        left, i = atom(i)
        if tokens[i] not in ("==", "!="):
            return (None if left[0] == "unknown" else bool(left[1])), i
        op, (right, i) = tokens[i], atom(i + 1)
        if left[0] == right[0] != "unknown":
            return (left[1] == right[1]) == (op == "=="), i
        return None, i

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
        return chain(i, "&&", compare, False)

    def either(i):
        return chain(i, "||", both, True)

    try:
        result, end = either(0)
    except ValueError:
        return None
    return result if end == len(tokens) - 1 else None


def skipped(text, event):
    """The jobs that never run for event: their if: is false, or they need a skipped
    job and their if: has no status function, so GitHub adds success()."""
    every = jobs(text)
    out = {job_id for job_id, job in every.items() if job_if(job) and value(job_if(job), event) is False}
    while True:
        more = set()
        for job_id, job in every.items():
            needs = re.search(r"(?m)^    needs: *(.*)\n((?:^      - .*\n)*)", job)
            needed = set(re.findall(r"[\w-]+", re.sub(r" #.*", "", needs[1] + needs[2]))) if needs else set()
            if job_id not in out and needed & out and not STATUS.search(job_if(job)):
                more.add(job_id)
        if not more:
            return out
        out |= more


def holds(text):
    """What this job or workflow head would hand its runner beyond GITHUB_TOKEN."""
    found = [name or "secrets" for expression in EXPRESSION.findall(text)
             for name in SECRET.findall(expression) if name.upper() != "GITHUB_TOKEN"]
    if re.search(r"(?m)^    [\"']?secrets[\"']? *:", text):
        found.append("secrets: (to a reusable workflow)")
    if re.search(r"\bid-token[\"']? *: *[\"']?write\b|\bpermissions *: *[\"']?write-all\b", text):
        found.append("id-token: write")
    return found


def leaks(text):
    """(job, what) for everything a pull request run of the workflow could reach."""
    events = pr_events(text)
    if not events:
        return []
    found = [("(workflow)", what) for what in holds(sections(text)[0])]
    never = set.intersection(*(skipped(text, event) for event in events))
    for job_id, job in jobs(text).items():
        if job_id not in never:
            found += [(job_id, what) for what in holds(job)]
    return found


def steps(job):
    """A job's steps, each from its "- " line."""
    return re.split(r"(?m)^(?=      - )", job.partition("\n    steps:\n")[2])[1:]


def fields(text, indent):
    """{key: value} at one indentation (a step's "- " counts as two spaces), without
    comments, lines that read a secret, or values left empty."""
    text = re.sub(r"(?im)^.*\$\{\{[^}]*\bsecrets\b.*\n?", "", re.sub(r"\A( *)- ", r"\1  ", code(text)))
    parts = re.split(rf"(?m)^ {{{indent}}}([\w-]+):", text)
    return {key: value.strip() for key, value in zip(parts[1::2], parts[2::2]) if value.strip()}


def drift(text):
    """Where the pull-request job's settings and its steps up to the --dry-run archive
    differ from archive's. Secrets, the archive step's name and --dry-run aside, a
    pull request builds what a tag builds."""
    every = jobs(text)
    pr, trusted = every["pull-request"], every["archive"]

    def settings(job):
        ignored = ("name", "if", "outputs", "environment", "permissions")
        return {k: v for k, v in fields(job.partition("\n    steps:\n")[0], 4).items() if k not in ignored}

    found = [] if settings(pr) == settings(trusted) else ["job settings"]
    ours, theirs = steps(pr), steps(trusted)
    last = next((i for i, step in enumerate(ours) if "--dry-run" in step), None)
    if last is None:
        return found + ["no --dry-run archive step"]
    for i in range(last + 1):
        mine, other = fields(ours[i], 8), fields(theirs[i], 8) if i < len(theirs) else {}
        if i == last:
            mine["run"] = mine.get("run", "").replace(" --dry-run", "")
            mine.pop("name", None)
            other.pop("name", None)
        if mine != other:
            found.append(f"step {i + 1}")
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

    def test_the_pull_request_job_builds_what_tags_build(self):
        text = (WORKFLOWS / "testflight.yml").read_text()
        self.assertEqual(drift(text), [])
        pr, mark, trusted = text.partition("\n  archive:\n")
        archive = "        run: scripts/testflight-local.sh\n"
        cases = {
            "xcode": (pr.replace("Xcode_26.3", "Xcode_26.4"), trusted, ["job settings"]),
            "runner": (pr, trusted.replace("runs-on: macos-15", "runs-on: macos-26", 1), ["job settings"]),
            "setup": (pr, trusted.replace("brew install xcodegen", "brew install xcodegen swiftlint"), ["step 2"]),
            "versions": (pr.replace("          python3 scripts/testflight_notes.py --check\n", ""), trusted, ["step 4"]),
            "pull request flag": (pr.replace("--dry-run", "--dry-run --skip-archive"), trusted, ["step 5"]),
            "archive flag": (pr, trusted.replace(archive, archive.replace(".sh", ".sh --strict")), ["step 5"]),
            "archive env": (pr, trusted.replace(archive, "          SIGN: 1\n" + archive), ["step 5"]),
            "extra step": (pr, trusted.replace("      - name: Archive and upload",
                                               "      - run: scripts/prepare.sh\n      - name: Archive and upload"), ["step 5"]),
            "no dry run": (pr.replace(" --dry-run", ""), trusted, ["no --dry-run archive step"]),
        }
        for case, (ours, theirs, found) in cases.items():
            with self.subTest(case):
                self.assertNotEqual(ours + mark + theirs, text)
                self.assertEqual(drift(ours + mark + theirs), found)

    def test_the_fixture_is_clean(self):
        self.assertEqual(leaks(FIXTURE), [])
        self.assertEqual(skipped(FIXTURE, "pull_request"), {"archive", "review", "after"})
        self.assertEqual(leaks(FIXTURE.replace("secrets.GITHUB_TOKEN", "SECRETS . github_token")), [])

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
            "over lines": ("          # step", "          KEY: ${{\n            secrets.KEY }}", "KEY"),
            "upper case": ("          # step", "          KEY: ${{ SECRETS.KEY }}", "KEY"),
            "bare": ("          # step", "          ALL: ${{ format('{0}', secrets, github.sha) }}", "secrets"),
            "script comment": ('        run: echo "no secrets here"', "        run: |\n          # ${{ secrets.KEY }}\n          echo", "KEY"),
            "quoted inherit": ("    env:\n      # job", "    uses: ./.github/workflows/x.yml\n    \"secrets\": inherit\n    env:",
                               "secrets: (to a reusable workflow)"),
            "quoted oidc": ("    env:\n      # job", "    permissions:\n      'id-token': 'write'\n    env:", "id-token: write"),
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
                          "${{ github.event_name != 'push' }}", "github.event_name != 'PULL_REQUEST' || true",
                          "''", '""', "${{ }}", "${{ !github.event_name != 'push' }}"):
            with self.subTest(condition=condition):
                text = FIXTURE.replace(guard, f"    if: {condition}\n" if condition else "")
                self.assertIn(("archive", "KEY"), leaks(text))
        for condition in ("github.event_name == 'push' && vars.SIGN != ''", "!(github.event_name == 'pull_request')",
                          "${{ github.event_name != 'pull_request' }}", "success() && github.event_name != 'Pull_Request'",
                          "github.event_name == 'push' || github.event_name == 'workflow_dispatch'",
                          "GitHub.Event_Name != 'pull_request'", "\"github.event_name != 'pull_request'\"",
                          "'github.event_name != ''pull_request'''"):
            with self.subTest(condition=condition):
                self.assertEqual(leaks(FIXTURE.replace(guard, f"    if: {condition}\n")), [])

    def test_a_dependent_that_can_run_after_a_skip_is_checked(self):
        for condition in ("always()", "${{ !cancelled() }}", "failure()", "Always()", "${{ !Cancelled() }}",
                          "success() || github.event_name == 'pull_request'", "SUCCESS()"):
            with self.subTest(condition):
                text = FIXTURE.replace("      - review\n", f"      - review\n    if: {condition}\n")
                self.assertEqual(leaks(text), [("after", "AFTER_KEY")])
        # Without a status function GitHub adds success(), so the skip carries on.
        guarded = FIXTURE.replace("      - review\n", "      - review\n    if: github.event_name == 'pull_request'\n")
        self.assertEqual(leaks(guarded), [])
        # A comment naming a skipped job is not a need.
        commented = FIXTURE.replace("    needs:\n      - review\n", "    needs: [check] # not review\n")
        self.assertEqual(leaks(commented), [("after", "AFTER_KEY")])

    def test_a_layout_the_reader_could_misread_fails(self):
        head, mark, body = FIXTURE.partition("jobs:\n")
        cases = {
            "jobs four spaces in": head + mark + re.sub(r"(?m)^(?=.)", "  ", body),
            "quoted id": FIXTURE.replace("  archive:\n", "  \"archive\":\n"),
            "id with a comment": FIXTURE.replace("  archive:\n", "  archive: # signs\n"),
            "jobs: with a comment": FIXTURE.replace("jobs:\n", "jobs: # all\n"),
            "flow jobs": head + "jobs: {check: {steps: [{run: '${{ secrets.KEY }}'}]}}\n",
            "second jobs:": FIXTURE + "jobs:\n  more:\n    steps: []\n",
            "job keys six in": FIXTURE + "  more:\n      steps:\n        - run: echo ${{ secrets.KEY }}\n",
            "alias": FIXTURE.replace("    env:\n      # job", "    env: *signing"),
            "merge key": FIXTURE.replace("      # job", "      <<: *signing"),
        }
        for case, text in cases.items():
            with self.subTest(case):
                self.assertNotEqual(text, FIXTURE)
                self.assertRaises(ValueError, leaks, text)

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
