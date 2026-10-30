/* EternalMonitor site languages. No dependencies.

   English is the HTML itself, which is what search engines and visitors
   without JavaScript get. Every other language is docs/i18n/<code>.json,
   loaded only for the language in use.

   A small script inline in every page's <head> (the same on each; see
   scripts/test_site_i18n.py) picks the language first: a ?lang= link, the
   saved choice, then the browser's languages. For English it does nothing
   else, so English visitors never wait on a download. For another language
   it starts loading the strings and keeps the page hidden until they are in,
   never longer than 3 s, so nothing flashes in the wrong language. It leaves
   what it decided in window.emLang. This file loads async and does the rest.

   Pages whose bootstrap has data-translated (index, download) are
   translated. On the others the copy stays English and a line says so in the
   visitor's language.

   In the HTML, data-i18n="key" replaces an element's text and
   data-i18n-<attribute>="key" an attribute (alt, aria-label, placeholder,
   content). In a string, <0>text</0> is the element's first child element
   (icons not counted) with new text, and <0/> keeps that child as it is, so
   links, versions and code stay in the HTML. The page's English is kept the
   first time an element is translated and put back from there, so English
   never needs a download. scripts/site_i18n.py checks every language file
   against docs/i18n/en.json, and en.json against the HTML. */
(function () {
  'use strict';

  var LANGS = [
    ['en', 'English'], ['es', 'Español'], ['ar', 'العربية'], ['zh-Hans', '简体中文'],
    ['hi', 'हिन्दी'], ['ru', 'Русский'], ['ja', '日本語'], ['pt-BR', 'Português (Brasil)'],
    ['fr', 'Français'], ['de', 'Deutsch'], ['ko', '한국어']
  ];
  var NAMES = {};
  LANGS.forEach(function (l) { NAMES[l[0]] = l[1]; });
  var STORE = 'em-lang';
  var root = document.documentElement;
  var boot = window.emLang || { lang: 'en' };
  var translated = !!boot.translated;
  var save = function (lang) { try { localStorage.setItem(STORE, lang); } catch (e) {} };

  var current = 'en';  // the language the page shows
  var latest = 0;      // the newest request; answers to older ones are dropped
  var cache = {};
  var remember = function (lang, strings) {
    cache[lang] = strings;
    strings.catch(function () { delete cache[lang]; });
    return strings;
  };
  var load = function (lang) {
    return cache[lang] || remember(lang, fetch('i18n/' + lang + '.json')
      .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); }));
  };
  if (boot.strings) remember(boot.lang, boot.strings);
  var setRootLang = function (lang) {
    root.classList.toggle('not-english', lang !== 'en');
    var shown = translated ? lang : 'en';
    root.lang = shown;
    root.dir = shown === 'ar' ? 'rtl' : 'ltr';
  };

  var reveal = function () { clearTimeout(boot.timer); root.classList.remove('i18n-wait'); };
  var domReady = new Promise(function (resolve) {
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', resolve);
    else resolve();
  });

  /* --- Applying strings ---------------------------------------------------- */
  var TOKEN = /<(\d+)>([^<]*)<\/\1>|<(\d+)\/>/g;
  var isIcon = function (el) { return /^(svg|img)$/i.test(el.tagName); };

  // The page's English for each element translated so far, kept before the
  // first change: its child nodes when its text is replaced, and attributes.
  var touched = [];
  var kept = function (el) {
    if (!el.__i18nEn) { el.__i18nEn = {}; touched.push(el); }
    return el.__i18nEn;
  };
  var restore = function () {
    touched.forEach(function (el) {
      var en = el.__i18nEn;
      if (en.nodes) {
        el.textContent = '';
        en.nodes.forEach(function (n) { el.appendChild(n); });
      }
      for (var attr in en.attrs) el.setAttribute(attr, en.attrs[attr]);
    });
  };

  var fill = function (el, text) {
    // The element's own children are captured once and moved, never cloned,
    // so other scripts keep their references (the iPad build, star counts).
    var en = kept(el);
    if (!en.nodes) en.nodes = Array.prototype.slice.call(el.childNodes);
    var kids = el.__i18nKids || (el.__i18nKids = Array.prototype.filter.call(el.children, function (c) { return !isIcon(c); }));
    Array.prototype.slice.call(el.childNodes).forEach(function (n) {
      if (n.nodeType === 3 || kids.indexOf(n) >= 0) el.removeChild(n);
    });
    var frag = document.createDocumentFragment(), at = 0, m;
    TOKEN.lastIndex = 0;
    while ((m = TOKEN.exec(text))) {
      if (m.index > at) frag.appendChild(document.createTextNode(text.slice(at, m.index)));
      var kid = kids[+(m[1] || m[3])];
      if (kid) {
        if (m[1]) {
          var kidEn = kept(kid);
          if (!kidEn.nodes) kidEn.nodes = Array.prototype.slice.call(kid.childNodes);
          kid.textContent = m[2];
        }
        frag.appendChild(kid);
      }
      at = TOKEN.lastIndex;
    }
    if (at < text.length) frag.appendChild(document.createTextNode(text.slice(at)));
    el.appendChild(frag);
  };

  var ATTRS = ['alt', 'aria-label', 'placeholder', 'content'];
  var translate = function (scope, dict) {
    Array.prototype.forEach.call(scope.querySelectorAll('[data-i18n]'), function (el) {
      var text = dict[el.getAttribute('data-i18n')];
      if (typeof text === 'string') fill(el, text);
    });
    ATTRS.forEach(function (attr) {
      Array.prototype.forEach.call(scope.querySelectorAll('[data-i18n-' + attr + ']'), function (el) {
        var text = dict[el.getAttribute('data-i18n-' + attr)];
        if (typeof text !== 'string') return;
        var en = kept(el);
        en.attrs = en.attrs || {};
        if (!(attr in en.attrs)) en.attrs[attr] = el.getAttribute(attr);
        el.setAttribute(attr, text);
      });
    });
  };

  var active = {};
  var apply = function (lang, dict) {
    var scope = translated ? document : document.querySelector('[data-lang]');
    if (scope) translate(scope, dict);
    show(lang, dict);
  };
  // English from the page itself, with no download: also what a timeout or a
  // failed download shows.
  var showEnglish = function () {
    restore();
    show('en', {});
  };
  // Everything around the copy that follows the language: the root's lang and
  // dir, the switcher, the English toggle and the English-only note.
  var show = function (lang, dict) {
    active = dict;
    current = lang;
    setRootLang(lang);
    var box = document.querySelector('[data-lang]');
    if (box) {
      if (!translated) box.lang = lang;
      var name = box.querySelector('[data-lang-name]');
      if (name) { name.textContent = NAMES[lang]; name.lang = lang; }
      Array.prototype.forEach.call(box.querySelectorAll('[data-lang-code]'), function (a) {
        if (a.getAttribute('data-lang-code') === lang) a.setAttribute('aria-current', 'true');
        else a.removeAttribute('aria-current');
      });
    }
    var toggle = document.querySelector('[data-lang-english]');
    if (toggle) toggle.hidden = lang === 'en';
    if (!translated) englishOnlyNote(lang, dict);
    reveal();
    document.dispatchEvent(new CustomEvent('em:language', { detail: { lang: lang } }));
  };

  // Untranslated pages say so, in the visitor's language, under the title.
  var englishOnlyNote = function (lang, dict) {
    var note = document.getElementById('lang-note');
    if (lang === 'en' || !dict['page.englishOnly']) { if (note) note.remove(); return; }
    if (!note) {
      var head = document.querySelector('.page-head .container') || document.querySelector('main');
      if (!head) return;
      note = document.createElement('p');
      note.id = 'lang-note';
      note.className = 'lang-note';
      head.appendChild(note);
    }
    note.lang = lang;
    note.dir = lang === 'ar' ? 'rtl' : 'ltr';
    note.textContent = dict['page.englishOnly'];
  };

  var request = function (lang, strings) {
    var id = ++latest;
    if (lang === 'en') showEnglish();
    else if (lang !== current) {
      Promise.all([strings || load(lang), domReady]).then(function (r) {
        if (id === latest) apply(lang, r[0]);
      }, function () {
        // No strings (offline, blocked): the page is English. The choice
        // stays saved, so the next page or visit tries again.
        if (id === latest) showEnglish();
      });
    }
    return id;
  };

  var choose = function (lang) {
    save(lang);
    // A ?lang= left in the address would bring the old choice back on reload.
    var url = new URL(window.location.href);
    if (url.searchParams.has('lang')) {
      url.searchParams.delete('lang');
      history.replaceState(history.state, '', url.pathname + url.search + url.hash);
    }
    request(lang);
  };

  /* --- The switcher ---------------------------------------------------------- */
  var wire = function () {
    var box = document.querySelector('[data-lang]');
    var english = document.querySelector('[data-lang-english]');
    if (english) {
      english.addEventListener('click', function (e) { e.preventDefault(); choose('en'); });
    }
    if (!box) return;
    var button = box.querySelector('button');
    var menu = box.querySelector('ul');
    if (!button || !menu) return;
    LANGS.forEach(function (l) {
      var url = new URL(window.location.href);
      url.searchParams.set('lang', l[0]);
      var li = document.createElement('li');
      var a = document.createElement('a');
      a.href = url.pathname + url.search + url.hash;
      a.lang = l[0];
      a.hreflang = l[0];
      a.textContent = l[1];
      a.setAttribute('data-lang-code', l[0]);
      if (l[0] === current) a.setAttribute('aria-current', 'true');
      li.appendChild(a);
      menu.appendChild(li);
    });
    var links = Array.prototype.slice.call(menu.querySelectorAll('a'));
    var open = function (focusFirst) {
      menu.hidden = false;
      button.setAttribute('aria-expanded', 'true');
      if (focusFirst) (menu.querySelector('[aria-current]') || links[0]).focus();
    };
    var close = function (refocus) {
      if (menu.hidden) return;
      menu.hidden = true;
      button.setAttribute('aria-expanded', 'false');
      if (refocus) button.focus();
    };
    button.addEventListener('click', function () { if (menu.hidden) open(false); else close(false); });
    button.addEventListener('keydown', function (e) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); open(true); }
    });
    menu.addEventListener('click', function (e) {
      var a = e.target.closest('a[data-lang-code]');
      if (!a) return;
      e.preventDefault();
      close(true);
      choose(a.getAttribute('data-lang-code'));
    });
    menu.addEventListener('keydown', function (e) {
      var i = links.indexOf(document.activeElement);
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        links[(i + (e.key === 'ArrowDown' ? 1 : links.length - 1)) % links.length].focus();
      } else if (e.key === 'Home' || e.key === 'End') {
        e.preventDefault();
        links[e.key === 'Home' ? 0 : links.length - 1].focus();
      }
    });
    box.addEventListener('keydown', function (e) { if (e.key === 'Escape') close(true); });
    box.addEventListener('focusout', function (e) { if (!box.contains(e.relatedTarget)) close(false); });
    document.addEventListener('click', function (e) { if (!box.contains(e.target)) close(false); });
  };

  domReady.then(wire);
  if (boot.strings) {
    var first = request(boot.lang, boot.strings);
    // The bootstrap's 3 s limit, or its strings failing: English, and an
    // answer that comes in later is dropped.
    boot.fail = function () {
      if (latest === first) { latest++; showEnglish(); }
    };
  }

  // For docs/script.js: the active language's text for a key, else English.
  window.emI18n = {
    t: function (key, english) {
      var text = translated && current !== 'en' ? active[key] : null;
      return typeof text === 'string' ? text : english;
    }
  };
})();
