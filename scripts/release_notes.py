#!/usr/bin/env python3
"""Write a GitHub release body with this version's section of RELEASE_NOTES.md.

The release workflow fills .github/release-body.md: {{SHA256}} becomes the
installer's hash and {{NOTES}} the tag's "## EternalMonitor vX.Y.Z" section.

    release_notes.py --tag v0.3.3 --strict                     # check only
    release_notes.py --tag v0.3.3 --sha256 HASH --out FILE --strict

The in-app updater (host/src/updates.rs, published_sha256) and
scripts/latest_json.py take the first 64-digit hex after the first "SHA-256"
in the body, and refuse an installer that differs. So the notes go after the
hash's code block, notes containing a 64-digit hex string are refused, and
the finished body is parsed the same way before it is written.

--strict (final tags) exits 1 when the tag has no section. Otherwise that is a
GitHub Actions warning and the body keeps only the link to RELEASE_NOTES.md.
"""
import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / ".github" / "release-body.md"
NOTES = ROOT / "RELEASE_NOTES.md"

# host/src/updates.rs: (?is)SHA-256.*?\b([0-9a-f]{64})\b
PUBLISHED_SHA256 = re.compile(r"SHA-256.*?\b([0-9a-f]{64})\b", re.I | re.S)
HEX64 = re.compile(r"[0-9a-f]{64}", re.I)
# Lines that start a new Markdown block instead of continuing a paragraph.
BLOCK = re.compile(r"(#|[-*+] |\d+[.)] |```|>|\|)")


class NotesError(Exception):
    pass


def section(notes, version):
    """The body of "## EternalMonitor v{version}" up to the next section, or None."""
    match = re.search(
        rf"(?ms)^## EternalMonitor v{re.escape(version)}[ \t]*\n(.*?)(?=^## EternalMonitor |\Z)", notes
    )
    return match[1].strip() if match else None


def unwrap(text):
    """Joins hard-wrapped lines: GitHub shows each newline in a release body as a break."""
    lines, fenced = [], False
    for line in text.split("\n"):
        stripped = line.strip()
        joins = (
            not fenced
            and stripped
            and lines
            and lines[-1].strip()
            and not lines[-1].lstrip().startswith("#")
            and not BLOCK.match(stripped)
        )
        if joins:
            lines[-1] = lines[-1].rstrip() + " " + stripped
        else:
            lines.append(line.rstrip())
        if stripped.startswith("```"):
            fenced = not fenced
    return "\n".join(lines)


def published_sha256(body):
    match = PUBLISHED_SHA256.search(body)
    return match[1].lower() if match else None


def render(template, sha256, notes):
    """The body: notes (Markdown, may be empty) go after the hash block."""
    template = template.replace("\r\n", "\n")
    if template.count("{{SHA256}}") != 1 or template.count("{{NOTES}}") != 1:
        raise NotesError("the template needs one {{SHA256}} and one {{NOTES}}")
    if template.index("{{NOTES}}") < template.index("{{SHA256}}"):
        raise NotesError("the template must put {{NOTES}} after {{SHA256}}")
    head, tail = template.split("{{NOTES}}")
    body = head.replace("{{SHA256}}", sha256) + notes + tail
    return re.sub(r"\n{3,}", "\n\n", body)


def notes_for(tag, notes_text, strict):
    """(Markdown for {{NOTES}}, warning or None)."""
    version = tag[1:] if re.fullmatch(r"v\d+\.\d+\.\d+\S*", tag) else None
    text = section(notes_text, version) if version else None
    if not text:
        # A heading with nothing under it is no notes either.
        problem = f"RELEASE_NOTES.md has no '## EternalMonitor {tag}' section" if text is None else (
            f"RELEASE_NOTES.md's '## EternalMonitor {tag}' section is empty"
        )
        if strict:
            raise NotesError(problem)
        return "", f"::warning::{problem}; the release body links to RELEASE_NOTES.md only."
    if HEX64.search(text):
        # The hash parsers must never meet another 64-digit hex string.
        raise NotesError(f"the {tag} notes contain a 64-digit hex string; take it out")
    return f"## What's new in {tag}\n\n{unwrap(text)}\n", None


def body(tag, sha256, template, notes_text, strict):
    """(release body, warning or None)."""
    if not re.fullmatch(r"[0-9A-Fa-f]{64}", sha256):
        raise NotesError("--sha256 must be 64 hex digits")
    notes, warning = notes_for(tag, notes_text, strict)
    text = render(template, sha256, notes)
    if published_sha256(text) != sha256.lower():
        raise NotesError("the updater would not read this installer's SHA-256 from the body")
    return text, warning


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--sha256", help="write the body for this installer hash")
    parser.add_argument("--out", help="file for the body (default: standard output)")
    parser.add_argument("--strict", action="store_true", help="fail when the tag has no section")
    parser.add_argument("--notes", default=str(NOTES))
    parser.add_argument("--template", default=str(TEMPLATE))
    args = parser.parse_args(argv)
    notes_text = pathlib.Path(args.notes).read_text(encoding="utf-8").replace("\r\n", "\n")
    try:
        if args.sha256 is None:
            notes, warning = notes_for(args.tag, notes_text, args.strict)
            text = None
        else:
            template = pathlib.Path(args.template).read_text(encoding="utf-8")
            text, warning = body(args.tag, args.sha256, template, notes_text, args.strict)
    except NotesError as error:
        print(f"::error::{error}")
        return 1
    if warning:
        print(warning)
    if text is None:
        print(f"Release notes for {args.tag}: {len(notes)} characters")
    elif args.out:
        with open(args.out, "w", encoding="utf-8", newline="\n") as out:
            out.write(text)
        print(f"Release body for {args.tag}: {len(text)} characters, in {args.out}")
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
