#!/usr/bin/env python3
"""Check that the troubleshooting guides quote the apps' messages exactly.

SETUP.md's iPad and PC tables put each message in bold in the first column,
and QUICKSTART.txt lists them as `- "message": what to do`. Each quote must
equal a whole message in the Windows host or iPad app source: one string
literal as the app renders it, or literals the source joins into one message
(Swift `+`, Rust `concat!`), with comments between them ignored. When a
quote is not a whole message but starts one, the error names that message. A
placeholder such as {version} or \\(host) in the source is written as an
ellipsis (…). Runs of whitespace compare as one space. QUICKSTART.txt is
ASCII, so there it is "..." and apostrophes are straight.

This is a drift detector, not a compiler. A source file it can't read is
skipped with a warning instead of failing the check. Forms it doesn't read:
Swift regex literals (/…/) containing a double quote, format strings built
inside another macro (format!(concat!(…))), and messages joined any other way
than Swift `+` or Rust `concat!` (for example String + &str or .join()).
"""
import pathlib
import re
import sys
from typing import NamedTuple

ROOT = pathlib.Path(__file__).resolve().parents[1]
CHECKED_TABLES = ("### Messages on the iPad", "### Messages on the PC")
ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
RUST_RAW = re.compile(r'b?r(#*)"')
RUST_CHAR = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'")
# The format string is a macro's first argument, or its second after a writer.
RUST_FORMAT_CALL = re.compile(r"\w!\(\s*(?:[^,()\"]+,\s*)?$")
SWIFT_OPEN = re.compile(r'(#*)("""|")')


class LexError(ValueError):
    """The lexer could not read a file's string literals."""


class Literal(NamedTuple):
    value: str
    # The code since the previous literal, with each comment as one space.
    # Joins and format calls are decided from this alone, so text inside a
    # literal (a URL's //, a "/*") is never read as code.
    code_before: str


def _identifier_before(text, i):
    return i > 0 and (text[i - 1].isalnum() or text[i - 1] == "_")


def _skip_comment(text, i):
    """The index after a // or (nested) /* */ comment starting at i, or None."""
    if text.startswith("//", i):
        end = text.find("\n", i)
        return len(text) if end < 0 else end
    if not text.startswith("/*", i):
        return None
    depth, i = 1, i + 2
    while depth:
        if i >= len(text):
            raise LexError("unterminated /* comment")
        if text.startswith("/*", i):
            depth, i = depth + 1, i + 2
        elif text.startswith("*/", i):
            depth, i = depth - 1, i + 2
        else:
            i += 1
    return i


def _escape(text, i, skip_indent):
    """Decode the escape whose backslash is at i: (text, next index)."""
    c = text[i + 1]
    if c in ESCAPES:
        return ESCAPES[c], i + 2
    if c == "u":
        close = text.index("}", i)
        return chr(int(text[i + 3:close], 16)), close + 1
    if c == "x":
        return chr(int(text[i + 2:i + 4], 16)), i + 4
    if c == "\n":
        # A backslash ending a line joins the next line. Rust also drops that
        # line's leading whitespace; Swift's indentation is already removed.
        i += 2
        while skip_indent and i < len(text) and text[i].isspace():
            i += 1
        return "", i
    return c, i + 2


def _rust_format(value):
    """A format string as printed: {{ and }} are braces, {…} is a value."""
    value = value.replace("{{", "\x01").replace("}}", "\x02")
    value = re.sub(r"\{[^{}]*\}", "…", value)
    return value.replace("\x01", "{").replace("\x02", "}")


def _rust_literal(code, value):
    """A Rust literal as printed, given the code before it."""
    return Literal(_rust_format(value) if RUST_FORMAT_CALL.search(code) else value, code)


def rust_literals(text):
    literals, code, i = [], [], 0
    while i < len(text):
        after_comment = _skip_comment(text, i)
        raw = RUST_RAW.match(text, i)
        if after_comment is not None:
            code.append(" ")
            i = after_comment
        elif raw and not _identifier_before(text, i):
            close = text.index('"' + raw[1], raw.end())
            literals.append(_rust_literal("".join(code), text[raw.end():close]))
            code, i = [], close + 1 + len(raw[1])
        elif text[i] == '"' or (text.startswith('b"', i) and not _identifier_before(text, i)):
            i, out = text.index('"', i) + 1, []
            while text[i] != '"':
                if text[i] == "\\":
                    piece, i = _escape(text, i, skip_indent=True)
                else:
                    piece, i = text[i], i + 1
                out.append(piece)
            literals.append(_rust_literal("".join(code), "".join(out)))
            code, i = [], i + 1
        elif text[i] == "'" and RUST_CHAR.match(text, i):
            end = RUST_CHAR.match(text, i).end()
            code.append(text[i:end])
            i = end
        else:
            code.append(text[i])
            i += 1
    return _join(literals, r"\s*,\s*", opener=r"concat!\(\s*$")


def _skip_parens(text, i):
    """The index after the balanced parentheses starting at i."""
    depth = 0
    while True:
        after_comment = _skip_comment(text, i)
        if after_comment is not None:
            i = after_comment
            continue
        if SWIFT_OPEN.match(text, i) and text[i] in '"#':
            _, i = swift_string(text, i)
            continue
        depth += {"(": 1, ")": -1}.get(text[i], 0)
        i += 1
        if depth == 0:
            return i


def _swift_body(text, i, close, escape):
    out = []
    while not text.startswith(close, i):
        if text.startswith(escape + "(", i):
            i = _skip_parens(text, i + len(escape))
            out.append("…")
        elif text.startswith(escape, i):
            piece, i = _escape(text[:i] + "\\" + text[i + len(escape):], i, skip_indent=False)
            i += len(escape) - 1
            out.append(piece)
        else:
            out.append(text[i])
            i += 1
    return "".join(out), i + len(close)


def swift_string(text, i):
    """(rendered value, end) for the Swift string literal starting at i."""
    opening = SWIFT_OPEN.match(text, i)
    hashes, quote = opening[1], opening[2]
    escape = "\\" + hashes
    if quote == '"':
        return _swift_body(text, opening.end(), '"' + hashes, escape)
    # """: content starts on the next line and ends before the closing line,
    # whose indentation is removed from every line.
    begin = text.index("\n", opening.end()) + 1
    # An escaped \""" is text; only an unescaped """ (plus the hashes) closes.
    close = begin
    while not text.startswith('"""' + hashes, close):
        close += len(escape) + 1 if text.startswith(escape, close) else 1
        if close >= len(text):
            raise LexError("unterminated multi-line string")
    last_newline = text.rindex("\n", begin - 1, close)
    indent = text[last_newline + 1:close]
    lines = [line[len(indent):] if line.startswith(indent) else line.lstrip()
             for line in text[begin:last_newline].split("\n")] if last_newline >= begin else []
    value, _ = _swift_body("\n".join(lines) + '"""' + hashes, 0, '"""' + hashes, escape)
    return value, close + 3 + len(hashes)


def swift_literals(text):
    literals, code, i = [], [], 0
    while i < len(text):
        after_comment = _skip_comment(text, i)
        if after_comment is not None:
            code.append(" ")
            i = after_comment
        elif SWIFT_OPEN.match(text, i) and (text[i] == '"' or not _identifier_before(text, i)):
            value, i = swift_string(text, i)
            literals.append(Literal(value, "".join(code)))
            code = []
        else:
            code.append(text[i])
            i += 1
    return _join(literals, r"\s*\+\s*")


def _join(literals, joiner, opener=None):
    """Messages, with literals the source joins into one message merged.

    A literal joins the message before it when only the joiner (Swift `+`,
    or a comma inside Rust `concat!(`) stands between them. For concat!, the
    message's first literal must follow the opener directly.
    """
    messages = []  # [value, code before its first literal]
    for literal in literals:
        if (messages and re.fullmatch(joiner, literal.code_before)
                and (opener is None or re.search(opener, messages[-1][1]))):
            messages[-1][0] += literal.value
        else:
            messages.append([literal.value, literal.code_before])
    return [value for value, _ in messages]


SOURCES = [("host/src", "*.rs", rust_literals), ("ios/EternalMonitor", "*.swift", swift_literals)]


def _spaces(text):
    return " ".join(text.split())


def source_messages(root):
    """Every whole message in the app sources, as the user would read it.

    A file the lexer can't read is skipped with a warning on stderr.
    """
    messages = set()
    for folder, pattern, extract in SOURCES:
        for path in sorted((root / folder).rglob(pattern)):
            try:
                found = extract(path.read_text(encoding="utf-8"))
            except (IndexError, ValueError) as error:
                print(f"WARNING: skipped {path.relative_to(root)}: could not read its string literals"
                      f" ({type(error).__name__}: {error})", file=sys.stderr)
                continue
            messages.update(_spaces(m) for m in found)
    return messages


def setup_quotes(text):
    """Bold quotes in the first column of the iPad and PC tables."""
    quotes = []
    table = None
    for number, line in enumerate(text.splitlines(), 1):
        if line.startswith("#"):
            table = line.strip() if line.strip() in CHECKED_TABLES else None
        elif table and line.startswith("| ") and not line.startswith("| ---"):
            first = line.split(" | ")[0]
            if first.startswith("| What "):
                continue
            quotes += [(number, quote) for quote in re.findall(r"\*\*(.+?)\*\*", first)]
    return quotes


def quickstart_quotes(text):
    """Quoted messages in QUICKSTART.txt's "If something goes wrong" section."""
    section = text.split("If something goes wrong\n", 1)[1].split("\nGetting help\n", 1)[0]
    quotes = []
    for item in re.split(r"\n- ", section)[1:]:
        match = re.match(r'"(.+?)":', " ".join(item.split()), re.S)
        if match:
            line = text.count("\n", 0, text.index(item)) + 1
            quotes.append((line, match[1]))
    return quotes


def ascii_form(message):
    return message.replace("’", "'").replace("…", "...")


def _problem(quote, messages):
    """Why a quote is not a whole app message, or None."""
    if quote in messages:
        return None
    longer = sorted(m for m in messages if m.startswith(quote))
    if longer:
        return f"only the start of the app message \"{longer[0]}\""
    return "not an exact app message"


def check(root=ROOT):
    """The list of problems; empty when every quote matches."""
    messages = source_messages(root)
    problems = []
    setup = setup_quotes((root / "SETUP.md").read_text(encoding="utf-8"))
    if not setup:
        problems.append("SETUP.md: no quoted messages found in the iPad and PC tables")
    for line, quote in setup:
        problem = _problem(_spaces(quote), messages)
        if problem:
            problems.append(f"SETUP.md:{line}: {problem}: {quote}")
    ascii_messages = {ascii_form(m) for m in messages}
    quickstart = (root / "scripts/QUICKSTART.txt").read_text(encoding="utf-8")
    if not quickstart.isascii():
        problems.append("scripts/QUICKSTART.txt: must be ASCII")
    found = quickstart_quotes(quickstart)
    if not found:
        problems.append("scripts/QUICKSTART.txt: no quoted messages found")
    for line, quote in found:
        problem = _problem(_spaces(quote), ascii_messages)
        if problem:
            problems.append(f"scripts/QUICKSTART.txt:{line}: {problem}: {quote}")
    return problems


def main():
    problems = check()
    for problem in problems:
        print(f"FAIL: {problem}")
    if problems:
        sys.exit(1)
    print("Troubleshooting messages match the apps")


if __name__ == "__main__":
    main()
