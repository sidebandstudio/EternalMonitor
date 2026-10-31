"""Each stable release publishes its notes, and they never move the updater's hash."""
import contextlib
import io
import pathlib
import re
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import release_notes as rn  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
TEMPLATE = (ROOT / ".github" / "release-body.md").read_text(encoding="utf-8")
NOTES = (ROOT / "RELEASE_NOTES.md").read_text(encoding="utf-8")
SHA = "3DFCD9AB8106589D5FB01ADDD169B060E418F9ADC9DDB229FEA1B3EC6AAF122B"
OTHER = "a" * 64

SAMPLE = """## EternalMonitor v1.2.0

Released today. A paragraph
wrapped over two lines.

### What changed

- A list item that wraps
  onto a second line with **bold
  words**.
- Second item.

```
code stays
as it is
```

## EternalMonitor v1.1.0

Older notes.
"""


class Section(unittest.TestCase):
    def test_takes_one_version_up_to_the_next(self):
        text = rn.section(SAMPLE, "1.2.0")
        self.assertTrue(text.startswith("Released today."))
        self.assertTrue(text.endswith("```"))
        self.assertNotIn("Older notes", text)
        self.assertEqual(rn.section(SAMPLE, "1.1.0"), "Older notes.")

    def test_needs_the_exact_version(self):
        self.assertIsNone(rn.section(SAMPLE, "1.2"))
        self.assertIsNone(rn.section(SAMPLE, "1.2.0-rc.1"))
        self.assertIsNone(rn.section(NOTES, "0.1.2"))
        self.assertIsNotNone(rn.section(NOTES, "0.1.2-mirror"))

    def test_unwraps_paragraphs_and_list_items_but_not_code(self):
        text = rn.unwrap(rn.section(SAMPLE, "1.2.0"))
        self.assertIn("Released today. A paragraph wrapped over two lines.", text)
        self.assertIn("- A list item that wraps onto a second line with **bold words**.\n- Second item.", text)
        self.assertIn("### What changed\n\n- A list", text)
        self.assertIn("```\ncode stays\nas it is\n```", text)

    def test_real_sections_keep_their_bullets(self):
        for version in ("0.3.3", "0.3.2"):
            raw = rn.section(NOTES, version)
            text = rn.unwrap(raw)
            bullets = len(re.findall(r"(?m)^- ", raw))
            self.assertGreater(bullets, 3)
            self.assertEqual(len(re.findall(r"(?m)^- ", text)), bullets)
            self.assertEqual(text.split(), raw.split())
            self.assertIn("### What changed", text)


class Body(unittest.TestCase):
    def test_every_published_version_renders_with_the_real_hash(self):
        versions = re.findall(r"(?m)^## EternalMonitor v(\S+)", NOTES)
        self.assertIn("0.3.3", versions)
        for version in versions:
            body, warning = rn.body(f"v{version}", SHA, TEMPLATE, NOTES, strict=True)
            self.assertIsNone(warning)
            self.assertEqual(rn.published_sha256(body), SHA.lower(), version)
            self.assertIn(f"## What's new in v{version}\n\n", body)
            self.assertIn("[RELEASE_NOTES.md](https://github.com/sidebandstudio/EternalMonitor/blob/main/RELEASE_NOTES.md)", body)
            self.assertNotIn("{{", body)
            # The hash block comes before the notes.
            self.assertLess(body.index(SHA), body.index("## What's new"))

    def test_the_template_puts_notes_after_its_only_hash_label(self):
        self.assertEqual(TEMPLATE.count("SHA-256"), 1)
        self.assertLess(TEMPLATE.index("SHA-256"), TEMPLATE.index("{{SHA256}}"))
        self.assertLess(TEMPLATE.index("{{SHA256}}"), TEMPLATE.index("{{NOTES}}"))
        self.assertIn("**EternalMonitor-Setup.exe SHA-256:**\n```\n{{SHA256}}\n```", TEMPLATE)

    def test_notes_after_the_hash_cannot_change_it(self):
        # Even notes that pass a label and another hash: the first label wins.
        for notes in (f"SHA-256: {OTHER}\n", f"sha-256\n```\n{OTHER.upper()}\n```\n", "SHA-256 SHA-256"):
            body = rn.render(TEMPLATE, SHA, notes)
            self.assertEqual(rn.published_sha256(body), SHA.lower())
            self.assertEqual(rn.published_sha256(body.replace("\n", "\r\n")), SHA.lower())

    def test_a_template_with_notes_before_the_hash_is_refused(self):
        before = "{{NOTES}}\n\nSHA-256:\n```\n{{SHA256}}\n```\n"
        with self.assertRaisesRegex(rn.NotesError, "after"):
            rn.render(before, SHA, "")
        with self.assertRaisesRegex(rn.NotesError, "one"):
            rn.render("SHA-256 {{SHA256}}\n", SHA, "")

    def test_notes_with_a_64_digit_hex_string_are_refused(self):
        for hex64 in (OTHER, OTHER.upper(), "0123456789abcdef" * 4):
            notes = f"## EternalMonitor v1.2.0\n\nThe driver is pinned to {hex64}.\n"
            with self.assertRaisesRegex(rn.NotesError, "64-digit"):
                rn.body("v1.2.0", SHA, TEMPLATE, notes, strict=False)

    def test_notes_may_mention_the_label(self):
        # v0.3.2's notes say "checks the installer's SHA-256".
        body, _ = rn.body("v0.3.2", SHA, TEMPLATE, NOTES, strict=True)
        self.assertIn("checks the installer's SHA-256", body)
        self.assertEqual(rn.published_sha256(body), SHA.lower())

    def test_crlf_files_render_like_lf(self):
        crlf, _ = rn.body("v0.3.3", SHA, TEMPLATE.replace("\n", "\r\n"), NOTES, strict=True)
        lf, _ = rn.body("v0.3.3", SHA, TEMPLATE, NOTES, strict=True)
        self.assertEqual(crlf, lf)
        self.assertNotIn("\r", lf)

    def test_a_bad_hash_is_refused(self):
        for sha in ("", SHA[:-1], SHA + "0", "g" * 64):
            with self.assertRaisesRegex(rn.NotesError, "64 hex"):
                rn.body("v0.3.3", sha, TEMPLATE, NOTES, strict=True)


class Missing(unittest.TestCase):
    def test_a_final_tag_without_notes_fails(self):
        with self.assertRaisesRegex(rn.NotesError, "no '## EternalMonitor v9.9.9' section"):
            rn.body("v9.9.9", SHA, TEMPLATE, NOTES, strict=True)

    def test_an_empty_section_counts_as_missing(self):
        notes = "## EternalMonitor v1.2.0\n\n  \n\n## EternalMonitor v1.1.0\n\n- Fixed a thing.\n"
        with self.assertRaisesRegex(rn.NotesError, "v1.2.0' section is empty"):
            rn.body("v1.2.0", SHA, TEMPLATE, notes, strict=True)
        body, warning = rn.body("v1.2.0", SHA, TEMPLATE, notes, strict=False)
        self.assertTrue(warning.startswith("::warning::"))
        self.assertNotIn("What's new", body)

    def test_otherwise_it_warns_and_keeps_the_link(self):
        for tag in ("v9.9.9", "v0.3.4-rc.1", "main"):
            body, warning = rn.body(tag, SHA, TEMPLATE, NOTES, strict=False)
            self.assertTrue(warning.startswith("::warning::"), tag)
            self.assertNotIn("What's new", body)
            self.assertIn("RELEASE_NOTES.md", body)
            self.assertNotIn("\n\n\n", body)
            self.assertEqual(rn.published_sha256(body), SHA.lower())


class Cli(unittest.TestCase):
    def run_main(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = rn.main(list(argv))
        return code, out.getvalue()

    def test_writes_the_body_as_utf8_with_lf(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "body.md"
            code, out = self.run_main("--tag", "v0.3.3", "--sha256", SHA, "--out", str(path), "--strict")
            self.assertEqual(code, 0, out)
            data = path.read_bytes()
            self.assertNotIn(b"\r", data)
            self.assertFalse(data.startswith(b"\xef\xbb\xbf"))
            self.assertIn("800×600", data.decode("utf-8"))

    def test_check_only_and_exit_codes(self):
        self.assertEqual(self.run_main("--tag", "v0.3.3", "--strict")[0], 0)
        code, out = self.run_main("--tag", "v9.9.9", "--strict")
        self.assertEqual(code, 1)
        self.assertTrue(out.startswith("::error::"))
        code, out = self.run_main("--tag", "v9.9.9")
        self.assertEqual(code, 0)
        self.assertTrue(out.startswith("::warning::"))


if __name__ == "__main__":
    unittest.main()
