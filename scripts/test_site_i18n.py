"""site_i18n.py must keep every language file in step with the site's English."""
import json
import pathlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import site_i18n  # noqa: E402

DOCS = pathlib.Path(__file__).resolve().parents[1] / "docs"


class RealSiteTest(unittest.TestCase):
    def test_site_strings_match(self):
        self.assertEqual(site_i18n.check(), [])

    def test_every_language_has_a_file(self):
        for lang in site_i18n.LANGS:
            self.assertTrue((DOCS / "i18n" / f"{lang}.json").is_file(), lang)

    def test_translated_pages_mark_their_copy(self):
        for name in site_i18n.TRANSLATED:
            html = (DOCS / name).read_text()
            self.assertIn('<script src="i18n.js" data-translated></script>', html, name)
            self.assertGreater(html.count("data-i18n"), 50, name)
        for name in site_i18n.UNTRANSLATED:
            html = (DOCS / name).read_text()
            self.assertIn('<script src="i18n.js"></script>', html, name)
            self.assertIn("data-lang-english", html, name)


class ContentTest(unittest.TestCase):
    def english(self, html):
        node = site_i18n.Tree(html).root.children[0]
        return site_i18n.content(node)

    def test_icons_stay_out_and_links_become_tags(self):
        self.assertEqual(self.english('<a data-i18n="k">\n  <svg><path d="M0 0"/></svg>\n  Download for Windows\n</a>'),
                         "Download for Windows")
        self.assertEqual(self.english('<p data-i18n="k">See <a href="x">known issues</a>, or <a href="y">tell us</a>.</p>'),
                         "See <0>known issues</0>, or <1>tell us</1>.")

    def test_release_values_and_code_are_kept_whole(self):
        self.assertEqual(self.english('<p data-i18n="k">Build: <span data-ipad-version>v0.3.3, build 364</span>.</p>'),
                         "Build: <0/>.")
        self.assertEqual(self.english('<li data-i18n="k">Set <code>FFMPEG_DIR</code> first</li>'), "Set <0/> first")
        self.assertEqual(self.english('<a data-i18n="k">Star <span class="count" data-stars></span></a>'), "Star <0/>")


class CheckTest(unittest.TestCase):
    """Each drift a later edit could cause is reported."""

    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        shutil.copytree(DOCS / "i18n", self.tmp / "i18n")
        for name in site_i18n.TRANSLATED + site_i18n.UNTRANSLATED + ["script.js", "i18n.js"]:
            shutil.copy(DOCS / name, self.tmp / name)
        patcher = mock.patch.object(site_i18n, "DOCS", self.tmp)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(shutil.rmtree, self.tmp)

    def strings(self, lang):
        return json.loads((self.tmp / "i18n" / f"{lang}.json").read_text())

    def save(self, lang, data):
        (self.tmp / "i18n" / f"{lang}.json").write_text(json.dumps(data, ensure_ascii=False))

    def edit_page(self, name, old, new):
        path = self.tmp / name
        html = path.read_text()
        self.assertIn(old, html)
        path.write_text(html.replace(old, new, 1))

    def assertReports(self, fragment):
        problems = site_i18n.check()
        self.assertTrue(any(fragment in p for p in problems), problems)

    def test_missing_and_extra_keys(self):
        data = self.strings("de")
        del data["nav.how"]
        data["nav.extra"] = "Extra"
        self.save("de", data)
        self.assertReports("de.json is missing ['nav.how']")
        self.assertReports("de.json has keys English does not: ['nav.extra']")

    def test_tags_must_match(self):
        data = self.strings("fr")
        data["index.faq.broken.a"] = "Consultez les <0>problèmes connus</0> ou écrivez-nous."
        self.save("fr", data)
        self.assertReports("fr.json 'index.faq.broken.a': tags")
        data["index.faq.broken.a"] = "<0>a</0> <1>b</1> <2/>"
        self.save("fr", data)
        self.assertReports("fr.json 'index.faq.broken.a': tags")

    def test_markup_is_refused(self):
        data = self.strings("es")
        data["nav.how"] = "Cómo <b>funciona</b>"
        self.save("es", data)
        self.assertReports("stray < or >")

    def test_email_numbers_and_placeholders_are_kept(self):
        data = self.strings("ja")
        data["download.form.send"] = "送信"
        data["common.req.windows"] = "Windows 10 または 11"
        data["mac.text"] = "EternalMonitor は Windows PC から配信します。"
        self.save("ja", data)
        self.assertReports("ja.json 'download.form.send': missing ['hello@sideband.studio']")
        self.assertReports("ja.json 'common.req.windows': missing ['1809', '64']")
        self.assertReports("ja.json 'mac.text': placeholders differ")

    def test_empty_string(self):
        data = self.strings("ko")
        data["nav.how"] = " "
        self.save("ko", data)
        self.assertReports("ko.json 'nav.how': empty")

    def test_english_edit_without_extract(self):
        self.edit_page("index.html", "Three steps.</h2>", "Four steps.</h2>")
        self.assertReports("en.json 'index.how.title' is 'Three steps.'; the page says 'Four steps.'")

    def test_unknown_key_in_page(self):
        self.edit_page("download.html", 'data-i18n="download.heading"', 'data-i18n="download.headline"')
        self.assertReports("en.json has no 'download.headline'")
        self.assertReports("en.json 'download.heading' is not used anywhere")

    def test_one_key_one_english(self):
        self.edit_page("download.html", 'data-i18n="nav.knownIssues">Known issues</a>\n',
                       'data-i18n="nav.knownIssues">Known problems</a>\n')
        self.assertReports("key 'nav.knownIssues' has two English texts")

    def test_attribute_key_needs_its_attribute(self):
        self.edit_page("index.html", 'data-i18n-alt="index.gpu.alt" alt=', 'data-i18n-alt="index.gpu.alt" title=')
        self.assertReports("data-i18n-alt='index.gpu.alt' on an element without alt")

    def test_extract_rewrites_english(self):
        self.edit_page("index.html", "Three steps.</h2>", "Four steps.</h2>")
        self.assertEqual(site_i18n.extract(), [])
        self.assertEqual(self.strings("en")["index.how.title"], "Four steps.")
        self.assertEqual(self.strings("en")["page.englishOnly"], "This page is in English only.")


if __name__ == "__main__":
    unittest.main()
