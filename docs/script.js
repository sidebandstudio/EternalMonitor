/* EternalMonitor site. No dependencies, no analytics. */
(function () {
  'use strict';

  var reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var CACHE_TTL = 6 * 3600000;
  // A cache entry counts only with a finite timestamp from the last six hours;
  // anything else (missing, malformed, in the future) is a miss.
  var isFresh = function (at) {
    if (typeof at !== 'number' || !isFinite(at)) return false;
    var age = Date.now() - at;
    return age >= 0 && age <= CACHE_TTL;
  };
  // True when version a (e.g. v0.3.10 against v0.3.2) is strictly newer than b, compared
  // number by number; unparseable versions are never newer.
  var isNewer = function (a, b) {
    var pa = String(a).replace(/^v/, '').split('.'), pb = String(b).replace(/^v/, '').split('.');
    if (!pa.every(function (x) { return /^\d+$/.test(x); }) || !pb.every(function (x) { return /^\d+$/.test(x); })) return false;
    for (var i = 0; i < Math.max(pa.length, pb.length); i++) {
      var x = parseInt(pa[i] || '0', 10), y = parseInt(pb[i] || '0', 10);
      if (x !== y) return x > y;
    }
    return false;
  };
  var REPO = 'sidebandstudio/EternalMonitor';
  var FEEDBACK_TO = 'hello@sideband.studio';
  // Text this script writes, in the page's language (docs/i18n.js), else the
  // English given here. Each is redrawn when the visitor changes language.
  var t = function (key, english) { return window.emI18n ? window.emI18n.t(key, english) : english; };
  var onLanguage = function (redraw) { document.addEventListener('em:language', redraw); };

  /* --- Nav: hairline once the page scrolls ------------------------------ */
  var nav = document.querySelector('.nav');
  if (nav) {
    var ticking = false;
    var updateNav = function () {
      nav.classList.toggle('scrolled', window.scrollY > 4);
      ticking = false;
    };
    window.addEventListener('scroll', function () {
      if (!ticking) { ticking = true; window.requestAnimationFrame(updateNav); }
    }, { passive: true });
    updateNav();
  }

  /* --- Mac visitors: one line under the header, dismissable ------------ */
  // Each page's head script adds html.mac before the first paint, so the line
  // never moves the page. iPadOS reports 'MacIntel' too, so it requires a
  // non-touch device.
  var macClose = document.querySelector('.mac-notice-close');
  if (macClose) {
    macClose.addEventListener('click', function () {
      sessionStorage.setItem('mac-notice', 'off');
      document.documentElement.classList.remove('mac');
    });
  }

  /* --- Hero: second screen or mirror, USB or Wi-Fi ---------------------
     The radios set the stage's data-mode and data-link; each screen picture
     names the state it belongs to in data-for and shows while all of its
     words match. The other states' pictures load once the page has. */
  var stage = document.querySelector('.stage[data-mode]');
  if (stage) {
    var later = stage.querySelectorAll('img[data-src]');
    var fetched = false;
    var fetchStates = function () {
      if (fetched) return;
      fetched = true;
      later.forEach(function (img) {
        img.srcset = img.getAttribute('data-srcset');
        img.src = img.getAttribute('data-src');
      });
    };
    if (document.readyState === 'complete') setTimeout(fetchStates, 300);
    else window.addEventListener('load', function () { setTimeout(fetchStates, 300); });
    var showState = function () {
      var state = [stage.getAttribute('data-mode'), stage.getAttribute('data-link')];
      var has = function (words) {
        return words.split(' ').every(function (w) { return state.indexOf(w) >= 0; });
      };
      stage.querySelectorAll('.scr[data-for]').forEach(function (img) {
        img.classList.toggle('on', has(img.getAttribute('data-for')));
      });
      stage.querySelectorAll('[data-say]').forEach(function (line) {
        line.hidden = !has(line.getAttribute('data-say'));
      });
    };
    stage.addEventListener('change', function (e) {
      fetchStates();
      if (e.target.name === 'hero-mode') stage.setAttribute('data-mode', e.target.value);
      if (e.target.name === 'hero-link') stage.setAttribute('data-link', e.target.value);
      showState();
    });
    // A restored page can come back with other radios checked.
    stage.querySelectorAll('input:checked').forEach(function (input) {
      stage.setAttribute(input.name === 'hero-mode' ? 'data-mode' : 'data-link', input.value);
    });
    showState();
  }

  /* --- Scroll reveal ----------------------------------------------------- */
  var reveals = document.querySelectorAll('.reveal');
  if (reveals.length && 'IntersectionObserver' in window && !reducedMotion) {
    var revealer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('visible');
          revealer.unobserve(entry.target);
        }
      });
    }, { threshold: 0.12, rootMargin: '0px 0px -32px 0px' });
    reveals.forEach(function (el) { revealer.observe(el); });
  } else {
    reveals.forEach(function (el) { el.classList.add('visible'); });
  }

  /* --- Connection clip -------------------------------------------------------
     The short clip in How it works plays silently while it is in view,
     unless the visitor prefers reduced motion; the Play button covers both. */
  var clip = document.getElementById('clip');
  if (clip) {
    var playBtn = document.getElementById('clip-play');
    var wantsPlay = !reducedMotion;
    // Without JS the clip keeps its native controls; with JS the button takes over.
    clip.removeAttribute('controls');

    var setPlayLabel = function () {
      if (!playBtn) return;
      var playing = !clip.paused && !clip.ended;
      playBtn.querySelector('.label').textContent = playing ? t('clip.pause', 'Pause') : t('clip.play', 'Play');
      playBtn.setAttribute('aria-pressed', playing ? 'true' : 'false');
      // SVG elements have no `hidden` property, so toggle the attribute.
      playBtn.querySelector('.ico-play').toggleAttribute('hidden', playing);
      playBtn.querySelector('.ico-pause').toggleAttribute('hidden', !playing);
    };
    var tryPlay = function () {
      var p = clip.play();
      if (p && p.catch) p.catch(function () { wantsPlay = false; setPlayLabel(); });
    };
    clip.addEventListener('play', setPlayLabel);
    clip.addEventListener('pause', setPlayLabel);
    setPlayLabel();
    onLanguage(setPlayLabel);

    if ('IntersectionObserver' in window) {
      new IntersectionObserver(function (entries) {
        if (entries[0].isIntersecting) { if (wantsPlay) tryPlay(); } else clip.pause();
      }, { threshold: 0.35 }).observe(clip);
    } else if (wantsPlay) {
      tryPlay();
    }
    if (playBtn) {
      playBtn.addEventListener('click', function () {
        if (clip.paused) { wantsPlay = true; tryPlay(); } else { wantsPlay = false; clip.pause(); }
      });
    }
  }

  /* --- Feature films ---------------------------------------------------------
     Each film in Watch it work plays while it is mostly on screen and pauses
     when it leaves or the tab is hidden. Every film has its own Pause button,
     and a film the visitor paused stays paused until they play it again. With
     reduced motion the posters stay until a film's Play button is pressed. */
  var films = Array.prototype.slice.call(document.querySelectorAll('.demo-film'));
  if (films.length && 'IntersectionObserver' in window) {
    var inView = new Set();
    var held = new Set();
    var ICONS = '<svg class="ico-pause" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><rect x="3.5" y="2.5" width="3" height="11" rx="1"/><rect x="9.5" y="2.5" width="3" height="11" rx="1"/></svg>' +
      '<svg class="ico-play" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M4.5 2.9v10.2a.8.8 0 0 0 1.2.7l8.3-5.1a.8.8 0 0 0 0-1.4L5.7 2.2a.8.8 0 0 0-1.2.7z"/></svg>';
    var playFilm = function (film) {
      if (held.has(film) || document.hidden) return;
      var p = film.play();
      if (p && p.catch) p.catch(function () {});
    };
    films.forEach(function (film) {
      if (reducedMotion) held.add(film);
      var card = film.closest('.demo');
      var title = card && card.querySelector('h3');
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'demo-toggle';
      btn.innerHTML = ICONS;
      var show = function () {
        var playing = !held.has(film);
        // The heading follows the language, so its text is read each time.
        var name = title ? title.textContent.trim() : '';
        btn.setAttribute('aria-label', (playing ? t('film.pause', 'Pause film: {name}') : t('film.play', 'Play film: {name}')).replace('{name}', name));
        btn.classList.toggle('is-paused', !playing);
      };
      btn.addEventListener('click', function () {
        if (held.has(film)) { held.delete(film); if (inView.has(film)) playFilm(film); }
        else { held.add(film); film.pause(); }
        show();
      });
      show();
      onLanguage(show);
      card.appendChild(btn);
    });
    var filmWatcher = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) { inView.add(entry.target); playFilm(entry.target); }
        else { inView.delete(entry.target); entry.target.pause(); }
      });
    }, { threshold: 0.4 });
    films.forEach(function (film) { filmWatcher.observe(film); });
    document.addEventListener('visibilitychange', function () {
      inView.forEach(function (film) { if (document.hidden) film.pause(); else playFilm(film); });
    });
  }

  /* --- GitHub stars -------------------------------------------------------------
     Shields' cached JSON first: it is shared by everyone, so a campus full of
     visitors behind one address can't exhaust GitHub's 60-an-hour limit. The
     GitHub API is only the fallback. The count is kept for six hours, and if
     both fail the link simply reads "GitHub". */
  var starEls = document.querySelectorAll('[data-stars]');
  if (starEls.length) {
    var STARS_KEY = 'gh-stars-v2';
    var isCount = function (text) { return typeof text === 'string' && /^\d+(\.\d+)?[kKmM]?$/.test(text); };
    var SVG = 'http://www.w3.org/2000/svg';
    var goldStar = function () {
      // Decorative: the count's own text already says "stars".
      var svg = document.createElementNS(SVG, 'svg');
      svg.setAttribute('class', 'count-star');
      svg.setAttribute('viewBox', '0 0 16 16');
      svg.setAttribute('aria-hidden', 'true');
      svg.setAttribute('focusable', 'false');
      var path = document.createElementNS(SVG, 'path');
      path.setAttribute('d', 'M8 1.1l2.15 4.36 4.81.7-3.48 3.39.82 4.79L8 12.08l-4.3 2.26.82-4.79L1.04 6.16l4.81-.7z');
      svg.appendChild(path);
      return svg;
    };
    var shownStars = null;
    var showStars = function (text) {
      shownStars = text;
      // Screen readers hear "GitHub, 180 stars", not a bare number.
      starEls.forEach(function (el) {
        el.textContent = '';
        el.appendChild(goldStar());
        el.appendChild(document.createTextNode(text));
        var unit = document.createElement('span');
        unit.className = 'visually-hidden';
        unit.textContent = ' ' + (text === '1' ? t('stars.one', 'star') : t('stars.other', 'stars'));
        el.appendChild(unit);
      });
    };
    onLanguage(function () { if (shownStars) showStars(shownStars); });
    var keepStars = function (text) {
      showStars(text);
      try { localStorage.setItem(STARS_KEY, JSON.stringify({ text: text, at: Date.now() })); } catch (e) {}
    };
    var cached = null;
    try { cached = JSON.parse(localStorage.getItem(STARS_KEY) || 'null'); } catch (e) {}
    if (cached && isCount(cached.text) && isFresh(cached.at)) {
      showStars(cached.text);
    } else {
      var fromShields = function () {
        return fetch('https://img.shields.io/github/stars/' + REPO + '.json')
          .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
          .then(function (badge) { if (!badge || !isCount(badge.value)) throw new Error('no count'); return badge.value; });
      };
      var fromGitHub = function () {
        return fetch('https://api.github.com/repos/' + REPO, { headers: { Accept: 'application/vnd.github+json' } })
          .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
          .then(function (repo) {
            var n = repo && repo.stargazers_count;
            if (typeof n !== 'number') throw new Error('no count');
            return n >= 1000 ? (n / 1000).toFixed(n >= 10000 ? 0 : 1).replace(/\.0$/, '') + 'k' : String(n);
          });
      };
      fromShields().catch(fromGitHub).then(keepStars).catch(function () {});
    }
  }

  /* --- Known issues: highlight the section in view -------------------------- */
  var tocLinks = Array.prototype.slice.call(document.querySelectorAll('.toc a[href^="#"]'));
  if (tocLinks.length && 'IntersectionObserver' in window) {
    var byId = {};
    tocLinks.forEach(function (a) { byId[a.getAttribute('href').slice(1)] = a; });
    var current = null;
    var sectionObserver = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          if (current) current.classList.remove('active');
          current = byId[entry.target.id];
          if (current) current.classList.add('active');
        }
      });
    }, { rootMargin: '-20% 0px -70% 0px' });
    Object.keys(byId).forEach(function (id) {
      var el = document.getElementById(id);
      if (el) sectionObserver.observe(el);
    });
  }

  /* --- Copy buttons ------------------------------------------------------------ */
  var copyText = function (text, done) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, function () {});
      return;
    }
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try { if (document.execCommand('copy')) done(); } catch (e) {}
    document.body.removeChild(ta);
  };
  document.querySelectorAll('[data-copy]').forEach(function (btn) {
    var reset = null;
    var shown = null;
    // While "Copied" shows, the button's own label nodes wait in __i18nHold,
    // where docs/i18n.js takes them as the English if it translates the button.
    var release = function () {
      clearTimeout(reset);
      reset = null;
      var held = btn.__i18nHold || [];
      btn.__i18nHold = null;
      // "Copied" still showing means nothing relabelled the button (the timer,
      // English chosen again, or a language that fell back): its label goes back.
      if (shown && shown.parentNode === btn) {
        btn.textContent = '';
        held.forEach(function (n) { btn.appendChild(n); });
      }
      shown = null;
    };
    onLanguage(release);
    btn.addEventListener('click', function () {
      var target = document.getElementById(btn.getAttribute('data-copy'));
      if (!target) return;
      copyText(target.textContent.trim(), function () {
        if (!reset) btn.__i18nHold = Array.prototype.slice.call(btn.childNodes);
        clearTimeout(reset);
        shown = document.createTextNode(t('common.copied', 'Copied'));
        btn.textContent = '';
        btn.appendChild(shown);
        reset = setTimeout(release, 1800);
      });
    });
  });

  /* --- Feedback form: opens the visitor's mail app with the message filled in --- */
  var form = document.getElementById('feedback-form');
  if (form) {
    var stepsField = document.getElementById('fb-steps-field');
    var fallback = document.getElementById('feedback-fallback');
    var preview = document.getElementById('feedback-preview');
    var value = function (id) {
      var el = document.getElementById(id);
      return el ? el.value.trim() : '';
    };
    var kind = function () {
      var checked = form.querySelector('input[name="kind"]:checked');
      return checked ? checked.value : 'Bug';
    };
    var syncKind = function () {
      if (stepsField) stepsField.hidden = kind() !== 'Bug';
      var msg = document.getElementById('fb-message');
      if (msg) {
        msg.placeholder = kind() === 'Bug'
          ? t('download.form.placeholder.bug', 'What you expected, what happened instead, and any message word for word.')
          : kind() === 'Idea'
            ? t('download.form.placeholder.idea', 'What would make EternalMonitor more useful for you?')
            : t('download.form.placeholder.hello', 'Anything at all. How you use it, what you study, what you draw.');
      }
    };
    Array.prototype.forEach.call(form.querySelectorAll('input[name="kind"]'), function (r) {
      r.addEventListener('change', syncKind);
    });
    syncKind();
    onLanguage(syncKind);

    var compose = function () {
      var lines = [value('fb-message'), ''];
      if (kind() === 'Bug' && value('fb-steps')) lines.push('Steps to reproduce:', value('fb-steps'), '');
      lines.push(
        'iPad: ' + (value('fb-ipad') || 'not given'),
        'PC: ' + (value('fb-pc') || 'not given'),
        'Connection: ' + (value('fb-connection') || 'not sure'),
        'Release: ' + (form.getAttribute('data-release') || 'not given'),
        'Sent from eternalmonitor.dev/download.html'
      );
      return lines.join('\n');
    };

    form.addEventListener('submit', function (e) {
      e.preventDefault();
      if (!form.reportValidity()) return;
      var tag = { Bug: 'bug', Idea: 'idea', Hello: 'hello' }[kind()] || 'feedback';
      var subject = '[EternalMonitor ' + tag + '] ' + value('fb-message').split('\n')[0].slice(0, 70);
      var body = compose();
      if (preview) preview.textContent = 'To: ' + FEEDBACK_TO + '\nSubject: ' + subject + '\n\n' + body;
      if (fallback) { fallback.hidden = false; fallback.scrollIntoView({ block: 'nearest', behavior: reducedMotion ? 'auto' : 'smooth' }); }
      window.location.href = 'mailto:' + FEEDBACK_TO +
        '?subject=' + encodeURIComponent(subject) + '&body=' + encodeURIComponent(body);
    });
  }

  /* --- Download page: the published release comes from latest.json ------------
     Both apps read the same file for their update checks. The HTML carries
     the same values, so nothing breaks if the fetch fails. */
  var downloadBtn = document.getElementById('download-btn');
  if (downloadBtn) {
    var versionEls = document.querySelectorAll('[data-win-version]');
    var ipadEls = document.querySelectorAll('[data-ipad-version]');
    var metaEl = document.getElementById('release-meta');
    var shaEl = document.getElementById('sha256-value');
    var tfLink = document.getElementById('testflight-link');
    var notesLink = document.getElementById('release-notes-link');
    var htmlTag = versionEls.length ? versionEls[0].textContent.trim() : '';

    var formatBytes = function (n) {
      if (n < 1048576) return (n / 1024).toFixed(0) + ' KB';
      return (n / 1048576).toFixed(1) + ' MB';
    };

    fetch('latest.json', { cache: 'no-cache' })
      .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
      .then(function (latest) {
        var win = latest && latest.windows;
        if (!win || !/^\d+\.\d+\.\d+$/.test(win.version)) throw new Error('no windows release');
        var tag = 'v' + win.version;
        var installer = 'https://github.com/' + REPO + '/releases/download/' + tag + '/EternalMonitor-Setup.exe';
        if (win.installer !== installer) throw new Error('unexpected installer');
        downloadBtn.href = installer;
        versionEls.forEach(function (el) { el.textContent = tag; });
        if (shaEl && /^[0-9a-f]{64}$/.test(win.sha256)) shaEl.textContent = win.sha256;
        if (notesLink) notesLink.href = 'https://github.com/' + REPO + '/releases/tag/' + tag;
        var ipad = latest.ipad || {};
        if (ipad.version && ipad.build) {
          ipadEls.forEach(function (el) { el.textContent = 'v' + ipad.version + ', build ' + ipad.build; });
        }
        if (tfLink && /^https:\/\/testflight\.apple\.com\/join\/[A-Za-z0-9]+$/.test(ipad.testflight || '')) {
          tfLink.href = ipad.testflight;
        }
        if (form) {
          form.setAttribute('data-release', 'Windows ' + tag +
            (ipad.version ? ' · iPad v' + ipad.version + (ipad.build ? ', TestFlight build ' + ipad.build : '') : ''));
        }
        // The HTML already shows the installer size for its own version, so
        // GitHub is only asked when latest.json names a strictly newer one,
        // and the answer is kept for six hours.
        if (!metaEl || !isNewer(tag, htmlTag)) return;
        var sizeKey = 'gh-setup-size-' + tag, hit = null;
        try { hit = JSON.parse(localStorage.getItem(sizeKey) || 'null'); } catch (e) {}
        if (hit && typeof hit.size === 'number' && isFinite(hit.size) && isFresh(hit.at)) {
          metaEl.textContent = 'EternalMonitor-Setup.exe · ' + formatBytes(hit.size);
          return;
        }
        return fetch('https://api.github.com/repos/' + REPO + '/releases/tags/' + tag, {
          headers: { Accept: 'application/vnd.github+json' }
        })
          .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
          .then(function (release) {
            var asset = (release.assets || []).filter(function (a) { return a.name === 'EternalMonitor-Setup.exe'; })[0];
            if (!asset) return;
            metaEl.textContent = asset.name + ' · ' + formatBytes(asset.size);
            try { localStorage.setItem(sizeKey, JSON.stringify({ size: asset.size, at: Date.now() })); } catch (e) {}
          });
      })
      .catch(function () { /* The HTML already carries the current release. */ });
  }

  /* --- Download page: Stable or Beta channel ------------------------------------
     ?channel=beta (or the older ?channel=nightly) shows the Beta build, which
     .github/workflows/nightly.yml publishes each night under the moving "beta"
     release. Its link never changes; GitHub is asked only for the version,
     date, commit and SHA-256, kept for half an hour. */
  var channelLinks = document.querySelectorAll('[data-channel]');
  var betaCard = document.getElementById('windows-beta');
  if (channelLinks.length && betaCard) {
    var BETA_KEY = 'gh-beta-v1', BETA_TTL = 1800000;
    var betaLoaded = false;
    var betaBuild = document.getElementById('beta-build');
    // What the build line shows: null (the page's text), 'missing' (no Beta
    // release yet) or the release. Redrawn when the language changes.
    var betaShown = null;
    var drawBuild = function () {
      if (!betaBuild) return;
      if (betaShown === 'missing') {
        betaBuild.textContent = t('download.beta.missing', 'The first Beta build is on its way. Use Stable until then.');
      } else if (betaShown) {
        betaShown();
      } else {
        betaBuild.textContent = t('download.beta.build', 'A new build comes out each night with the fixes that landed that day.');
      }
    };
    drawBuild();
    onLanguage(drawBuild);

    var showBeta = function (release) {
      var body = release.body || '';
      var asset = (release.assets || []).filter(function (a) { return a.name === 'EternalMonitor-Setup-beta.exe'; })[0];
      var version = (body.match(/\b(\d+\.\d+\.\d+-beta\.(\d{4})(\d{2})(\d{2})\.\d+)\b/) || []);
      var commit = (body.match(/\/commit\/([0-9a-f]{40})\b/) || [])[1];
      var when = asset && asset.updated_at ? new Date(asset.updated_at)
        : version[1] ? new Date(Date.UTC(+version[2], +version[3] - 1, +version[4])) : null;
      if (betaBuild && version[1]) {
        var link = null;
        if (commit) {
          link = document.createElement('a');
          link.href = 'https://github.com/' + REPO + '/commit/' + commit;
          link.target = '_blank';
          link.rel = 'noopener';
          link.textContent = commit.slice(0, 7);
        }
        betaShown = function () {
          var dated = when && isFinite(when);
          var line = dated && link ? t('download.beta.versionBuiltFrom', 'Version {version}, built {date} from {commit}')
            : dated ? t('download.beta.versionBuilt', 'Version {version}, built {date}')
            : link ? t('download.beta.versionFrom', 'Version {version} from {commit}')
            : t('download.beta.version', 'Version {version}');
          line = line.replace('{version}', version[1]);
          if (dated) {
            line = line.replace('{date}', when.toLocaleDateString(document.documentElement.lang || 'en-US',
              { month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' }));
          }
          var around = line.split('{commit}');
          betaBuild.textContent = around[0];
          if (link) betaBuild.append(link, around[1] || '');
        };
        drawBuild();
      }
      var meta = document.getElementById('beta-meta');
      if (meta && asset && typeof asset.size === 'number') {
        meta.textContent = asset.name + ' · ' + (asset.size / 1048576).toFixed(1) + ' MB';
      }
      var sha = ((asset && asset.digest || '').match(/^sha256:([0-9a-f]{64})$/) ||
                 body.match(/SHA-256[\s\S]*?\b([0-9a-f]{64})\b/) || [])[1];
      var shaBox = document.getElementById('beta-sha'), shaEl = document.getElementById('beta-sha256-value');
      if (sha && shaBox && shaEl) { shaEl.textContent = sha; shaBox.hidden = false; }
    };

    var loadBeta = function () {
      if (betaLoaded) return;
      betaLoaded = true;
      var hit = null;
      try { hit = JSON.parse(localStorage.getItem(BETA_KEY) || 'null'); } catch (e) {}
      var age = hit && typeof hit.at === 'number' ? Date.now() - hit.at : -1;
      if (hit && hit.release && age >= 0 && age <= BETA_TTL) { showBeta(hit.release); return; }
      fetch('https://api.github.com/repos/' + REPO + '/releases/tags/beta', {
        headers: { Accept: 'application/vnd.github+json' }
      })
        .then(function (r) {
          if (r.status === 404) { betaShown = 'missing'; drawBuild(); }
          if (!r.ok) throw new Error(r.status);
          return r.json();
        })
        .then(function (release) {
          var kept = { body: release.body, assets: (release.assets || []).map(function (a) {
            return { name: a.name, size: a.size, updated_at: a.updated_at, digest: a.digest };
          }) };
          showBeta(kept);
          try { localStorage.setItem(BETA_KEY, JSON.stringify({ release: kept, at: Date.now() })); } catch (e) {}
        })
        .catch(function () { /* The card still links the newest Beta build. */ });
    };

    // html.channel-beta shows the Beta card and the night sky (style.css). The
    // inline script in <head> sets it before first paint for ?channel=beta.
    var root = document.documentElement, switchTimer = 0;

    // The sky's animations pause while nobody can see them (.sky-paused).
    var sky = document.querySelector('.night-sky');
    if (sky) {
      var skyInView = true;
      var pauseSky = function () { root.classList.toggle('sky-paused', document.hidden || !skyInView); };
      document.addEventListener('visibilitychange', pauseSky);
      if ('IntersectionObserver' in window) {
        new IntersectionObserver(function (entries) {
          skyInView = entries[entries.length - 1].isIntersecting;
          pauseSky();
        }).observe(sky);
      }
      pauseSky();
    }

    var showChannel = function (channel) {
      var beta = channel === 'beta';
      if (beta && !root.classList.contains('night-ready')) {
        // Render the night layers at their day state first, so they fade in.
        root.classList.add('night-ready');
        void root.offsetWidth;
      }
      root.classList.toggle('channel-beta', beta);
      channelLinks.forEach(function (a) {
        if (a.getAttribute('data-channel') === channel) a.setAttribute('aria-current', 'page');
        else a.removeAttribute('aria-current');
      });
      if (beta) loadBeta();
    };
    var asked = new URLSearchParams(window.location.search).get('channel');
    showChannel(asked === 'beta' || asked === 'nightly' ? 'beta' : 'stable');
    channelLinks.forEach(function (a) {
      a.addEventListener('click', function (e) {
        e.preventDefault();
        var channel = a.getAttribute('data-channel');
        if ((channel === 'beta') === root.classList.contains('channel-beta')) return;
        history.replaceState(null, '', a.getAttribute('href'));
        // Lets the newly shown card ease in; the sky has its own transition.
        root.classList.add('channel-switching');
        clearTimeout(switchTimer);
        switchTimer = setTimeout(function () { root.classList.remove('channel-switching'); }, 600);
        showChannel(channel);
      });
    });
  }
})();
