"""check_messages.py must reject a troubleshooting quote the apps don't show."""
import contextlib
import io
import pathlib
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import check_messages  # noqa: E402

RUST = '''
const TITLE: &str = "The update did not finish";
fn banner(version: &str) -> String { format!("EternalMonitor {version} is available") }
'''
SWIFT = '''
let a = "That code didn’t match. Check the PC and try again."
let b = "Reaching \\(normalizedHostIP)"
let c = "No PCs found."
let d = "No PCs found. Check that EternalMonitor is open on the PC."
let e = "Check that EternalMonitor is open on the PC, "
    + "then try again."
'''
SETUP = '''# Set up

## If something goes wrong

### Messages on the iPad

| What the iPad says | What it means | What to do |
| --- | --- | --- |
| {ipad} | It means this. | Do that. |

### Messages on the PC

| What the PC window says | What it means | What to do |
| --- | --- | --- |
| **EternalMonitor … is available** | A new version. | Click **Update now**. |

### Messages from Windows, and problems without a message

| What you see | What it means | What to do |
| --- | --- | --- |
| **Windows protected your PC** | Not from the apps. | Click **Run anyway**. |
'''
QUICKSTART = '''Quick Start

If something goes wrong
-----------------------
- "{quick}": do this,
  over two lines.
- "The update did not finish": try again.

Getting help
------------
- "Not checked here": fine.
'''


class CheckMessagesTest(unittest.TestCase):
    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp())
        for name, text in (("host/src/gui/a.rs", RUST), ("ios/EternalMonitor/App/b.swift", SWIFT)):
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text(text, encoding="utf-8")
        (self.root / "scripts").mkdir()

    def tearDown(self):
        shutil.rmtree(self.root)

    def problems(self, ipad="**That code didn’t match. Check the PC and try again.**",
                 quick="That code didn't match. Check the PC and try again."):
        (self.root / "SETUP.md").write_text(SETUP.format(ipad=ipad), encoding="utf-8")
        (self.root / "scripts/QUICKSTART.txt").write_text(QUICKSTART.format(quick=quick), encoding="utf-8")
        return check_messages.check(self.root)

    def test_exact_quotes_pass(self):
        self.assertEqual(self.problems(), [])

    def test_shortened_quote_fails(self):
        problems = self.problems(ipad="**That code didn’t match**")
        self.assertEqual(len(problems), 1)
        self.assertIn("SETUP.md:9: only the start of the app message", problems[0])
        self.assertTrue(problems[0].endswith(": That code didn’t match"))

    def test_setup_keeps_the_apps_curly_apostrophe(self):
        self.assertEqual(len(self.problems(ipad="**That code didn't match. Check the PC and try again.**")), 1)

    def test_placeholders_are_written_as_an_ellipsis(self):
        self.assertEqual(self.problems(ipad="**Reaching …**"), [])
        self.assertEqual(len(self.problems(ipad="**Reaching 192.168.1.20**")), 1)

    def test_quickstart_is_checked_in_ascii(self):
        self.assertEqual(len(self.problems(quick="That code didn't match")), 1)
        problems = self.problems(quick="That code didn’t match. Check the PC and try again.")
        self.assertIn("scripts/QUICKSTART.txt: must be ASCII", problems)

    def test_an_exact_whole_message_passes_even_when_a_longer_one_starts_with_it(self):
        # "No PCs found." is a message of its own (the scan status), so quoting
        # it is exact even though another message begins with it.
        self.assertEqual(self.problems(ipad="**No PCs found.**"), [])
        self.assertEqual(self.problems(ipad="**No PCs found. Check that EternalMonitor is open on the PC.**"), [])
        problems = self.problems(ipad="**No PCs found. Check that**")
        self.assertEqual(len(problems), 1)
        self.assertIn('only the start of the app message "No PCs found. Check that EternalMonitor', problems[0])

    def test_comments_between_joined_literals_are_ignored(self):
        (self.root / "host/src/gui/d.rs").write_text(
            'const M: &str = concat!("Check the PC, ", /* note */ "then make sure.");')
        self.assertEqual(self.problems(ipad="**Check the PC, then make sure.**"), [])
        self.assertEqual(len(self.problems(ipad="**Check the PC,**")), 1)

    def test_a_url_before_a_join_does_not_hide_it(self):
        # Sol's input, plus an earlier "/*" literal: neither is code.
        (self.root / "host/src/gui/e.rs").write_text(
            'const A: &str = "/*";\nconst URL: &str = "https://example.com"; '
            'const MSG: &str = concat!("Check the cable.", " Then try again.");')
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            problems = self.problems(ipad="**Check the cable.**")
            self.assertEqual(self.problems(ipad="**Check the cable. Then try again.**"), [])
        self.assertEqual(len(problems), 1)
        self.assertIn('only the start of the app message "Check the cable. Then try again."', problems[0])
        self.assertEqual(err.getvalue(), "")

    def test_a_file_the_lexer_cannot_read_is_skipped_with_a_warning(self):
        (self.root / "ios/EternalMonitor/App/broken.swift").write_text('let s = "never closed')
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(self.problems(), [])
        self.assertIn("WARNING: skipped ios/EternalMonitor/App/broken.swift: could not read its string literals",
                      err.getvalue())

    def test_a_title_that_is_not_a_sentence_may_start_a_longer_message(self):
        # "The update did not finish" and "…did not finish, so …" are both whole.
        (self.root / "host/src/gui/c.rs").write_text('const B: &str = "The update did not finish, so try again.";')
        self.assertEqual(self.problems(), [])

    def test_joined_literals_are_one_message(self):
        self.assertEqual(self.problems(ipad="**Check that EternalMonitor is open on the PC, then try again.**"), [])
        self.assertEqual(len(self.problems(ipad="**Check that EternalMonitor is open on the PC,**")), 1)

    def test_the_repository_quotes_match_the_apps(self):
        self.assertEqual(check_messages.check(), [])



def rust(source):
    return {check_messages._spaces(m) for m in check_messages.rust_literals(source)}


def swift(source):
    return {check_messages._spaces(m) for m in check_messages.swift_literals(source)}


class LiteralFormsTest(unittest.TestCase):
    def test_rust_line_continuation_drops_the_newline_and_indent(self):
        source = 'notice(\n    "Windows didn\'t add the second screen in time. \\\n     Click Try again.",\n)'
        self.assertIn("Windows didn't add the second screen in time. Click Try again.", rust(source))

    def test_rust_literal_spanning_lines_keeps_its_words(self):
        self.assertIn("First line. Second line.", rust('let s = "First line.\n        Second line.";'))

    def test_rust_format_braces(self):
        messages = rust('let a = format!("Use {{braces}} for {name}");\nwrite!(f, "{} of {:.0}", a, b);\nlet b = "{{kept}}";')
        self.assertIn("Use {braces} for …", messages)
        self.assertIn("… of …", messages)
        self.assertIn("{{kept}}", messages)

    def test_rust_raw_strings_chars_comments_and_concat(self):
        messages = rust('''
            let q = '"';
            // "Commented out message"
            /* "Also /* nested */ commented out" */
            let raw = r#"He said "go""#;
            let joined = concat!("Part one, ", "part two.");
            let after = "After a quote char";
        ''')
        self.assertEqual(messages, {'He said "go"', "Part one, part two.", "After a quote char"})

    def test_swift_multiline_strings_follow_the_closing_indent(self):
        source = 'let m = """\n        Line one \\\n        continues.\n          Indented line\n        """\n'
        self.assertEqual(swift(source), {"Line one continues. Indented line"})
        self.assertEqual(check_messages.swift_literals(source), ["Line one continues.\n  Indented line"])

    def test_swift_interpolation_with_nested_strings(self):
        messages = swift('let s = "Found \\(n) PC\\(n == 1 ? "" : "s")"\nlet t = "Next"')
        self.assertEqual(messages, {"Found … PC…", "Next"})

    def test_swift_comment_inside_an_interpolation(self):
        self.assertEqual(swift('let s = "Reaching \\(1 /* ( */)"'), {"Reaching …"})

    def test_comments_between_joined_literals(self):
        self.assertEqual(swift('let j = "Check the PC, " /* note */ + "then try again."'),
                         {"Check the PC, then try again."})
        self.assertEqual(rust('let j = concat!("Check the PC, ", /* note */ "then try again.");'),
                         {"Check the PC, then try again."})

    def test_literals_are_never_read_as_code(self):
        source = 'const URL: &str = "https://example.com"; const MSG: &str = concat!("No PCs found.", " Check the PC.");'
        messages = rust(source)
        self.assertEqual(messages, {"https://example.com", "No PCs found. Check the PC."})
        self.assertNotIn("No PCs found.", messages)
        self.assertEqual(rust('const A: &str = "/*"; ' + source), messages | {"/*"})
        self.assertEqual(swift('let u = "https://x.y"\nlet j = "One, " + "two."'), {"https://x.y", "One, two."})

    def test_a_comment_between_a_format_call_and_its_string(self):
        self.assertEqual(rust('format!(\n    // keeps "Windows 10"\n    "build={}.{}",\n    a, b)'),
                         {"build=….…"})

    def test_rust_raw_format_string(self):
        self.assertEqual(rust('let s = format!(r#"Reaching {host}"#);'), {"Reaching …"})

    def test_swift_multiline_escaped_and_raw_triple_quotes(self):
        self.assertEqual(swift('let m = """\n    Say \\""" here\n    """\nlet n = "after"'), {'Say """ here', "after"})
        self.assertEqual(swift('let m = #"""\n    Bare """ inside\n    """#\nlet n = "after"'), {'Bare """ inside', "after"})

    def test_unterminated_forms_raise_instead_of_hanging(self):
        with self.assertRaises(check_messages.LexError):
            check_messages.swift_literals('let s = "ok" /* never closed')
        with self.assertRaises(IndexError):
            check_messages.rust_literals('let s = "never closed')

    def test_swift_raw_strings_and_concatenation(self):
        messages = swift('let r = #"Say "hi" \\#(name)"#\nlet j = "One, "\n    + "two."\n// "Not this"')
        self.assertEqual(messages, {'Say "hi" …', "One, two."})


class RepositoryMutationTest(unittest.TestCase):
    """A temp copy of the real docs and sources: a shortened quote must fail."""

    # Sol's pattern on one line: a URL literal, then a concat! whose first
    # piece is the shortened quote the tests below use.
    PROBE = ('const URL: &str = "https://eternalmonitor.dev/known-issues.html"; '
             'const M: &str = concat!("That code didn’t match.", " Check the PC and try again.");\n')

    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp())
        root = check_messages.ROOT

        def sources_only(folder, names):
            return [n for n in names if not (pathlib.Path(folder, n).is_dir() or n.endswith((".rs", ".swift")))]

        for folder in ("host/src", "ios/EternalMonitor"):
            shutil.copytree(root / folder, self.root / folder, ignore=sources_only)
        (self.root / "scripts").mkdir()
        shutil.copy(root / "SETUP.md", self.root / "SETUP.md")
        shutil.copy(root / "scripts/QUICKSTART.txt", self.root / "scripts/QUICKSTART.txt")
        (self.root / "host/src/zz_probe.rs").write_text(self.PROBE, encoding="utf-8")

    def tearDown(self):
        shutil.rmtree(self.root)

    def shorten(self, name, full, short):
        path = self.root / name
        text = path.read_text(encoding="utf-8")
        self.assertEqual(text.count(full), 1, f"{name} no longer quotes {full!r}")
        path.write_text(text.replace(full, short), encoding="utf-8")

    def test_the_copy_passes_unchanged(self):
        self.assertEqual(check_messages.check(self.root), [])

    def test_a_shortened_setup_quote_fails(self):
        self.shorten("SETUP.md", "**That code didn’t match. Check the PC and try again.**", "**That code didn’t match.**")
        problems = check_messages.check(self.root)
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith("SETUP.md:"))

    def test_a_shortened_quickstart_quote_fails(self):
        self.shorten("scripts/QUICKSTART.txt", '"That code didn\'t match. Check the PC and try again.":',
                     '"That code didn\'t match.":')
        problems = check_messages.check(self.root)
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(problems[0].startswith("scripts/QUICKSTART.txt:"))


if __name__ == "__main__":
    unittest.main()
