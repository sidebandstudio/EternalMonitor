#!/usr/bin/env node
/* Checks the website's language loading in headless Chrome when the network
   is slow or blocked: English never waits for a script, English comes back
   with no download, late answers never override a newer choice, and a
   timeout leaves a whole English page. docs/ is served from disk through
   Playwright's request routing (no server, no port); everything off-site is
   blocked.

   Playwright is not a dependency of this repository. Run with one installed:
     NODE_PATH=/path/to/node_modules node scripts/site_i18n_browser.js
   It uses the installed Google Chrome (PLAYWRIGHT_CHANNEL= to use
   Playwright's own Chromium; SITE_DOCS= to check another copy of docs/).
   Exits 1 if a check fails. */
'use strict';
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');

const DOCS = path.resolve(process.env.SITE_DOCS || path.join(__dirname, '..', 'docs'));
const SITE = 'http://em-site.test/';
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.json': 'application/json', '.css': 'text/css', '.svg': 'image/svg+xml', '.webp': 'image/webp', '.png': 'image/png', '.woff2': 'font/woff2', '.mp4': 'video/mp4' };
const sleep = ms => new Promise(r => setTimeout(r, ms));

let fails = 0;
const check = (ok, what, detail) => {
  console.log(`${ok ? 'PASS' : 'FAIL'} ${what}${!ok && detail !== undefined ? ': ' + JSON.stringify(detail) : ''}`);
  if (!ok) fails++;
};

// rules: { 'i18n.js': { delay: 4200 }, 'i18n/en.json': { block: true } }
const open = async (browser, { locale = 'en-US', saved, rules = {} } = {}) => {
  const context = await browser.newContext({ locale, viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
  const requests = [];
  const errors = [];
  await context.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.origin + '/' !== SITE) return route.abort();
    const file = decodeURIComponent(url.pathname.slice(1)) || 'index.html';
    requests.push(file);
    const rule = rules[file] || {};
    if (rule.delay) await sleep(rule.delay);
    if (rule.block) return route.abort().catch(() => {});
    const full = path.join(DOCS, file);
    if (!full.startsWith(DOCS) || !fs.existsSync(full)) return route.fulfill({ status: 404, body: 'not found' }).catch(() => {});
    return route.fulfill({ status: 200, contentType: TYPES[path.extname(file)] || 'application/octet-stream', body: fs.readFileSync(full) }).catch(() => {});
  });
  await context.addInitScript(saved => {
    sessionStorage.setItem('mac-notice', 'off');
    if (saved && !sessionStorage.getItem('seeded')) { localStorage.setItem('em-lang', saved); sessionStorage.setItem('seeded', '1'); }
  }, saved);
  const page = await context.newPage();
  page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
  page.on('pageerror', e => errors.push(String(e)));
  return { context, page, requests, errors };
};

// What a visitor sees and the parts of the page that follow the language.
const state = page => page.evaluate(() => {
  const root = document.documentElement;
  const toggle = document.querySelector('[data-lang-english]');
  return {
    lang: root.lang,
    dir: root.dir,
    classes: ['not-english', 'i18n-wait'].filter(c => root.classList.contains(c)).join(' '),
    visible: !!document.body && getComputedStyle(document.body).visibility === 'visible',
    title: document.title,
    h1: (document.querySelector('h1') || {}).textContent,
    switcher: (document.querySelector('[data-lang-name]') || {}).textContent,
    toggleShown: !!toggle && !toggle.hidden,
    saved: localStorage.getItem('em-lang'),
    i18nRan: !!window.emI18n
  };
});

// Every translatable text and attribute, to compare a page with its English.
const copy = page => page.evaluate(() => {
  const out = {};
  document.querySelectorAll('[data-i18n]').forEach((el, i) => { out['text' + i] = el.innerHTML; });
  ['alt', 'aria-label', 'placeholder', 'content'].forEach(a => document.querySelectorAll(`[data-i18n-${a}]`).forEach((el, i) => { out[a + i] = el.getAttribute(a); }));
  return out;
});
const diff = (a, b) => Object.keys(a).filter(k => a[k] !== b[k]).slice(0, 3).map(k => [k, a[k], b[k]]);

const ENGLISH = { 'index.html': 'Your Windows PC, on your iPad.', 'download.html': null, 'privacy.html': 'Privacy' };

(async () => {
  const browser = await chromium.launch(process.env.PLAYWRIGHT_CHANNEL === '' ? {} : { channel: process.env.PLAYWRIGHT_CHANNEL || 'chrome' });
  const allErrors = [];
  const done = async v => { allErrors.push(...v.errors); await v.context.close(); };

  // English as the page itself has it, for later comparisons.
  const reference = {};
  for (const name of ['index.html', 'download.html', 'privacy.html']) {
    const v = await open(browser);
    await v.page.goto(SITE + name, { waitUntil: 'load' });
    await v.page.waitForFunction(() => window.emI18n);
    reference[name] = { copy: await copy(v.page), state: await state(v.page) };
    if (!ENGLISH[name]) ENGLISH[name] = reference[name].state.h1;
    await done(v);
  }

  console.log('# 1. English visitors never wait for i18n.js (delayed 4.2 s)');
  for (const name of ['index.html', 'download.html', 'privacy.html']) {
    const v = await open(browser, { rules: { 'i18n.js': { delay: 4200 } } });
    const started = Date.now();
    await v.page.goto(SITE + name, { waitUntil: 'domcontentloaded' });
    await v.page.waitForFunction(() => document.querySelector('.reveal.visible') || !document.querySelector('.reveal'), null, { timeout: 2500 }).catch(() => {});
    const s = await state(v.page);
    const took = Date.now() - started;
    check(!s.i18nRan && took < 3000 && s.visible && s.lang === 'en' && s.title.length > 0 && s.h1 === ENGLISH[name] && s.classes === '',
      `${name}: English page shown in ${took} ms while i18n.js is still loading`, s);
    if (name !== 'privacy.html') {
      const revealed = await v.page.evaluate(() => !document.querySelector('.reveal') || !!document.querySelector('.reveal.visible'));
      check(revealed, `${name}: script.js ran (reveals visible) without waiting for i18n.js`);
    }
    await sleep(4500);
    check((await state(v.page)).i18nRan, `${name}: i18n.js arrived later and the page is still fine`);
    await done(v);
  }
  {
    // English visitors don't download any strings at all.
    const v = await open(browser);
    await v.page.goto(SITE + 'index.html', { waitUntil: 'networkidle' });
    check(!v.requests.some(r => r.startsWith('i18n/')), 'English visit loads no language file', v.requests.filter(r => r.startsWith('i18n/')));
    await done(v);
  }

  console.log('# 2. English comes back with no download (en.json blocked)');
  for (const name of ['index.html', 'download.html']) {
    const v = await open(browser, { locale: 'es-MX', rules: { 'i18n/en.json': { block: true } } });
    await v.page.goto(SITE + name, { waitUntil: 'networkidle' });
    const es = await state(v.page);
    check(es.lang === 'es' && es.toggleShown && es.h1 !== ENGLISH[name], `${name}: auto-switched to Spanish`, es);
    await v.page.click('[data-lang-english]');
    await sleep(200);
    const en = await state(v.page);
    check(en.lang === 'en' && en.dir === 'ltr' && en.h1 === ENGLISH[name] && en.title === reference[name].state.title && !en.toggleShown && en.switcher === 'English' && en.saved === 'en',
      `${name}: English toggle gives English and saves it`, en);
    const d = diff(reference[name].copy, await copy(v.page));
    check(d.length === 0, `${name}: every translated text and attribute is back to the page's English`, d);
    check(!v.requests.includes('i18n/en.json'), `${name}: no request for en.json`);
    // Round trip through Arabic and back again.
    await v.page.click('.lang-button');
    await v.page.click('.lang-menu a[data-lang-code="ar"]');
    await v.page.waitForFunction(() => document.documentElement.lang === 'ar');
    await v.page.click('[data-lang-english]');
    await sleep(200);
    const d2 = diff(reference[name].copy, await copy(v.page));
    check(d2.length === 0 && (await state(v.page)).dir === 'ltr', `${name}: English again after Arabic, identical`, d2);
    await done(v);
  }
  {
    // Untranslated pages put the switcher's own English back too.
    const v = await open(browser, { saved: 'de', rules: { 'i18n/en.json': { block: true } } });
    await v.page.goto(SITE + 'privacy.html', { waitUntil: 'networkidle' });
    const de = await v.page.evaluate(() => [document.querySelector('.lang-button .visually-hidden').textContent, !!document.getElementById('lang-note')]);
    await v.page.click('[data-lang-english]');
    await sleep(200);
    const en = await v.page.evaluate(() => [document.querySelector('.lang-button .visually-hidden').textContent, !!document.getElementById('lang-note')]);
    check(de[0] !== 'Language' && de[1] && en[0] === 'Language' && !en[1], 'privacy.html: switcher label and note go back to English', { de, en });
    await done(v);
  }

  console.log('# 3. A late answer never overrides a newer choice');
  {
    // Arabic still loading at first visit; the visitor picks English.
    const v = await open(browser, { saved: 'ar', rules: { 'i18n/ar.json': { delay: 2000 } } });
    await v.page.goto(SITE + 'index.html', { waitUntil: 'domcontentloaded' });
    await v.page.waitForFunction(() => window.emI18n);
    await v.page.evaluate(() => document.querySelector('[data-lang-english]').click());
    await sleep(2600);
    const s = await state(v.page);
    check(s.lang === 'en' && s.dir === 'ltr' && s.visible && s.h1 === ENGLISH['index.html'] && s.saved === 'en' && s.classes === '',
      'first-visit Arabic arriving after English was chosen is dropped', s);
    await done(v);
  }
  {
    // Later choices: Arabic (slow), then Spanish (fast).
    const v = await open(browser, { rules: { 'i18n/ar.json': { delay: 1500 } } });
    await v.page.goto(SITE + 'index.html', { waitUntil: 'networkidle' });
    await v.page.click('.lang-button');
    await v.page.click('.lang-menu a[data-lang-code="ar"]');
    await v.page.click('.lang-button');
    await v.page.click('.lang-menu a[data-lang-code="es"]');
    await sleep(2000);
    const s = await state(v.page);
    check(s.lang === 'es' && s.dir === 'ltr' && s.switcher === 'Español' && s.saved === 'es', 'Arabic arriving after Spanish was chosen is dropped', s);
    await done(v);
  }
  {
    const v = await open(browser, { rules: { 'i18n/ja.json': { delay: 1500 } } });
    await v.page.goto(SITE + 'download.html', { waitUntil: 'networkidle' });
    await v.page.click('.lang-button');
    await v.page.click('.lang-menu a[data-lang-code="ja"]');
    await v.page.click('.lang-button');
    await v.page.click('.lang-menu a[data-lang-code="en"]');
    await sleep(2000);
    const s = await state(v.page);
    check(s.lang === 'en' && s.h1 === ENGLISH['download.html'] && s.saved === 'en' && !s.toggleShown, 'Japanese arriving after English was chosen from the menu is dropped', s);
    await done(v);
  }

  console.log('# 4. A timeout or failure leaves a whole English page');
  const englishAll = (s, name) => s.lang === 'en' && s.dir === 'ltr' && s.classes === '' && s.visible && s.h1 === ENGLISH[name] &&
    s.title === reference[name].state.title && s.switcher === 'English' && !s.toggleShown;
  {
    const v = await open(browser, { saved: 'ar', rules: { 'i18n/ar.json': { delay: 5000 } } });
    await v.page.goto(SITE + 'index.html', { waitUntil: 'domcontentloaded' });
    await sleep(1000);
    const waiting = await state(v.page);
    check(!waiting.visible && waiting.lang === 'ar' && waiting.dir === 'rtl', 'Arabic: page held back while its strings load', waiting);
    await sleep(2500);
    const s = await state(v.page);
    check(englishAll(s, 'index.html') && s.saved === 'ar', 'after 3 s: whole English page (lang, dir, switcher, toggle, title); choice stays saved', s);
    const meta = await v.page.getAttribute('meta[name=description]', 'content');
    check(meta === reference['index.html'].copy[Object.keys(reference['index.html'].copy).find(k => k.startsWith('content'))], 'meta description English');
    await sleep(2000);
    const late = await state(v.page);
    check(englishAll(late, 'index.html'), 'the Arabic that arrives at 5 s is dropped', late);
    await done(v);
  }
  {
    // i18n.js itself slower than the limit: the bootstrap does the fallback.
    const v = await open(browser, { saved: 'ar', rules: { 'i18n.js': { delay: 4200 } } });
    await v.page.goto(SITE + 'download.html', { waitUntil: 'domcontentloaded' });
    await sleep(3400);
    const s = await state(v.page);
    check(!s.i18nRan && s.lang === 'en' && s.dir === 'ltr' && s.classes === '' && s.visible && s.h1 === ENGLISH['download.html'],
      'i18n.js delayed past 3 s: the bootstrap shows English', s);
    await sleep(1500);
    const after = await state(v.page);
    check(after.i18nRan && englishAll(after, 'download.html'), 'and stays English, switcher on English, once i18n.js runs', after);
    await done(v);
  }
  for (const name of ['index.html', 'privacy.html']) {
    const v = await open(browser, { saved: 'ar', rules: { 'i18n/ar.json': { block: true } } });
    await v.page.goto(SITE + name, { waitUntil: 'networkidle' });
    await sleep(200);
    const s = await state(v.page);
    check(englishAll(s, name) && s.saved === 'ar', `${name}: Arabic blocked: whole English page at once, choice stays saved`, s);
    await done(v);
  }
  {
    // A later choice that fails also lands on English, not a half-switched page.
    const v = await open(browser, { locale: 'es-ES', rules: { 'i18n/ko.json': { block: true } } });
    await v.page.goto(SITE + 'index.html', { waitUntil: 'networkidle' });
    await v.page.click('.lang-button');
    await v.page.click('.lang-menu a[data-lang-code="ko"]');
    await sleep(300);
    const s = await state(v.page);
    check(englishAll(s, 'index.html') && s.saved === 'ko', 'Korean blocked after Spanish: whole English page', s);
    await done(v);
  }

  console.log('# Normal loading still works');
  for (const [locale, want] of [['es-MX', 'es'], ['ar-EG', 'ar'], ['zh-TW', 'en'], ['pt-PT', 'pt-BR']]) {
    const v = await open(browser, { locale });
    await v.page.goto(SITE + 'index.html', { waitUntil: 'networkidle' });
    const s = await state(v.page);
    check(s.lang === want && s.visible && s.classes === (want === 'en' ? '' : 'not-english') && s.toggleShown === (want !== 'en'), `browser ${locale} -> ${want}`, s);
    await done(v);
  }
  {
    const v = await open(browser);
    await v.page.goto(SITE + 'download.html?lang=fr', { waitUntil: 'networkidle' });
    const s = await state(v.page);
    check(s.lang === 'fr' && s.saved === 'fr', '?lang=fr selects and saves French', s);
    await v.page.goto(SITE + 'index.html', { waitUntil: 'networkidle' });
    check((await state(v.page)).lang === 'fr', 'French carries to the next page');
    await done(v);
  }

  // Aborted requests log "Failed to load resource" by design; anything else is a bug.
  const real = allErrors.filter(e => !/Failed to load resource/.test(e));
  check(real.length === 0, 'no script errors', real);
  await browser.close();
  console.log(fails ? `${fails} failed` : 'all passed');
  process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
