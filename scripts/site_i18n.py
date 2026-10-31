#!/usr/bin/env python3
"""Keep the website's translations in step with its English.

English lives in the HTML (docs/index.html, docs/download.html) and in the
t('key', 'English') calls in docs/script.js. docs/i18n/en.json is that English
gathered in one place, and every other docs/i18n/<code>.json must have exactly
its keys, with the same <0>…</0> and <0/> tags, {placeholders}, numbers and
email addresses (docs/i18n.js explains the tags).

    site_i18n.py check     report every problem, exit 1 if there are any
    site_i18n.py extract   rewrite en.json from the HTML and script.js

After changing English copy, run extract, then update the same keys in every
language file; check (and the CI test) fails until they match.
"""
import json
import re
import sys
from html.parser import HTMLParser
from pathlib import Path

DOCS = Path(__file__).resolve().parent.parent / "docs"
LANGS = ["en", "es", "ar", "zh-Hans", "hi", "ru", "ja", "pt-BR", "fr", "de", "ko"]
TRANSLATED = ["index.html", "download.html"]
UNTRANSLATED = ["privacy.html", "known-issues.html", "updates.html", "compatibility.html", "ipad-second-monitor-windows.html", "alternatives.html"]
ATTRS = ["alt", "aria-label", "placeholder", "content", "title"]
VOID = {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"}
ICONS = {"svg", "img"}
KEEP = {"code", "kbd", "samp", "pre"}

TAG = re.compile(r"<(\d+)>([^<>]*)</\1>|<(\d+)/>")
PLACEHOLDER = re.compile(r"\{[a-z][A-Za-z]*\}")
INVARIANT = re.compile(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+|\d+", re.ASCII)
T_CALL = re.compile(r"""\bt\('([\w.]+)',\s*'((?:[^'\\]|\\.)*)'\)""")
DICT_KEY = re.compile(r"""dict\['([\w.]+)'\]""")


class Node:
    def __init__(self, tag, attrs):
        self.tag, self.attrs, self.children = tag, dict(attrs), []

    def text(self):
        return "".join(c if isinstance(c, str) else c.text() for c in self.children)

    def walk(self):
        yield self
        for c in self.children:
            if isinstance(c, Node):
                yield from c.walk()


class Tree(HTMLParser):
    def __init__(self, html):
        super().__init__(convert_charrefs=True)
        self.root = Node("#root", [])
        self.stack = [self.root]
        self.feed(html)
        self.close()

    def handle_starttag(self, tag, attrs):
        node = Node(tag, attrs)
        self.stack[-1].children.append(node)
        if tag not in VOID:
            self.stack.append(node)

    def handle_startendtag(self, tag, attrs):
        self.stack[-1].children.append(Node(tag, attrs))

    def handle_endtag(self, tag):
        for i in range(len(self.stack) - 1, 0, -1):
            if self.stack[i].tag == tag:
                del self.stack[i:]
                return

    def handle_data(self, data):
        self.stack[-1].children.append(data)


def squash(text):
    return re.sub(r"\s+", " ", text).strip()


def content(node):
    """The element's English as docs/i18n.js fills it: icons stay out, other
    child elements become <n>text</n>, or <n/> when they are kept whole."""
    out, index = [], 0
    for child in node.children:
        if isinstance(child, str):
            out.append(child)
            continue
        if child.tag in ICONS:
            continue
        nested = any(isinstance(c, Node) for c in child.children)
        kept = (child.tag in KEEP or nested or not child.text().strip()
                or any(name.startswith("data-") for name in child.attrs))
        out.append(f"<{index}/>" if kept else f"<{index}>{squash(child.text())}</{index}>")
        index += 1
    return squash("".join(out))


def page_strings(path, problems):
    """(key, English) for every marked element and attribute, in page order."""
    found = []
    for node in Tree(path.read_text()).root.walk():
        if "data-i18n" in node.attrs:
            found.append((node.attrs["data-i18n"], content(node)))
        for attr in ATTRS:
            key = node.attrs.get(f"data-i18n-{attr}")
            if key is None:
                continue
            if attr not in node.attrs:
                problems.append(f"{path.name}: data-i18n-{attr}={key!r} on an element without {attr}")
            else:
                found.append((key, node.attrs[attr]))
    return found


def english(problems):
    """Every key the site uses, with its English, and the keys only
    docs/i18n.js reads (their English lives in en.json alone)."""
    pairs = []
    for name in TRANSLATED:
        pairs += page_strings(DOCS / name, problems)
    for name in UNTRANSLATED:
        # Only the switcher is translated there; its keys must match.
        pairs += [p for p in page_strings(DOCS / name, problems) if p[0].startswith("lang.")]
    script = (DOCS / "script.js").read_text()
    pairs += [(k, v.replace("\\'", "'")) for k, v in T_CALL.findall(script)]
    if len(re.findall(r"\bt\('", script)) != len(T_CALL.findall(script)):
        problems.append("script.js: every t() call must be t('key', 'English') on one line")
    strings = {}
    for key, text in pairs:
        if key in strings and strings[key] != text:
            problems.append(f"key {key!r} has two English texts: {strings[key]!r} and {text!r}")
        strings.setdefault(key, text)
    runtime = DICT_KEY.findall((DOCS / "i18n.js").read_text())
    return strings, runtime


def tags(text):
    return sorted((m[1], "pair") if m[1] else (m[3], "keep") for m in TAG.finditer(text))


def check_value(lang, key, text, source, problems):
    where = f"{lang}.json {key!r}"
    if not isinstance(text, str) or not text.strip():
        problems.append(f"{where}: empty")
        return
    if text != text.strip():
        problems.append(f"{where}: leading or trailing space")
    if tags(text) != tags(source):
        problems.append(f"{where}: tags {tags(text)} differ from English {tags(source)}")
    if re.search(r"[<>]", TAG.sub("", text)):
        problems.append(f"{where}: stray < or > (only <0>…</0> and <0/> tags are allowed)")
    if sorted(PLACEHOLDER.findall(text)) != sorted(PLACEHOLDER.findall(source)):
        problems.append(f"{where}: placeholders differ from English")
    missing = set(INVARIANT.findall(TAG.sub("", source))) - set(INVARIANT.findall(TAG.sub("", text)))
    if missing:
        problems.append(f"{where}: missing {sorted(missing)} from the English")


def load(lang, problems):
    path = DOCS / "i18n" / f"{lang}.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        problems.append(f"{path.name}: {error}")
        return None
    if not isinstance(data, dict):
        problems.append(f"{path.name}: not a JSON object")
        return None
    return data


def check():
    problems = []
    strings, runtime = english(problems)
    en = load("en", problems)
    if en is None:
        return problems
    for key, text in strings.items():
        if key not in en:
            problems.append(f"en.json has no {key!r} (run site_i18n.py extract)")
        elif en[key] != text:
            problems.append(f"en.json {key!r} is {en[key]!r}; the page says {text!r} (run site_i18n.py extract)")
    for key in runtime:
        if key not in en:
            problems.append(f"en.json has no {key!r}, which docs/i18n.js reads")
    for key in en:
        if key not in strings and key not in runtime:
            problems.append(f"en.json {key!r} is not used anywhere")
    for key, text in en.items():
        check_value("en", key, text, text, problems)
    for lang in LANGS[1:]:
        data = load(lang, problems)
        if data is None:
            continue
        missing, extra = sorted(set(en) - set(data)), sorted(set(data) - set(en))
        if missing:
            problems.append(f"{lang}.json is missing {missing}")
        if extra:
            problems.append(f"{lang}.json has keys English does not: {extra}")
        for key in en:
            if key in data:
                check_value(lang, key, data[key], en[key], problems)
    return problems


def extract():
    problems = []
    strings, runtime = english(problems)
    if problems:
        return problems
    path = DOCS / "i18n" / "en.json"
    old = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
    for key in runtime:
        if key not in strings:
            if key not in old:
                return [f"docs/i18n.js reads {key!r}; add its English to en.json by hand"]
            strings[key] = old[key]
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps(strings, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return []


def main(argv):
    if argv[1:] not in (["check"], ["extract"]):
        sys.exit(__doc__)
    problems = check() if argv[1] == "check" else extract()
    for problem in problems:
        print(f"FAIL: {problem}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
