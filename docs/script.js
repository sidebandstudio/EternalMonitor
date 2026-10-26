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

  /* --- Mac visitors: one line, dismissable ------------------------------ */
  // iPadOS reports 'MacIntel' too, so require a non-touch device.
  var isMac = /Mac/.test(navigator.platform) && navigator.maxTouchPoints <= 1;
  if (isMac && nav && sessionStorage.getItem('mac-notice') !== 'off') {
    var notice = document.createElement('aside');
    notice.className = 'mac-notice';
    notice.setAttribute('aria-label', 'Note for Mac visitors');
    notice.innerHTML =
      '<div class="container mac-notice-inner">' +
      '<p><strong>On a Mac?</strong>EternalMonitor streams from Windows PCs. macOS already has ' +
      '<a href="https://support.apple.com/en-us/102386" target="_blank" rel="noopener">Sidecar</a> for this.</p>' +
      '<button type="button" class="mac-notice-close" aria-label="Dismiss">&times;</button></div>';
    notice.querySelector('.mac-notice-close').addEventListener('click', function () {
      sessionStorage.setItem('mac-notice', 'off');
      notice.remove();
    });
    document.body.appendChild(notice);
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

  /* --- Feature switches play once -------------------------------------------
     Without JS the switches rest on Second screen and Fill. With JS (and
     motion allowed) each starts on the other option and flips the first
     time it scrolls into view, so the change is seen; clicks work as before. */
  if (!reducedMotion && 'IntersectionObserver' in window) {
    [['mode-mirror', 'mode-second'], ['pic-fit', 'pic-fill']].forEach(function (pair) {
      var from = document.getElementById(pair[0]), to = document.getElementById(pair[1]);
      if (!from || !to) return;
      var touched = false;
      from.checked = true;
      [from, to].forEach(function (el) { el.addEventListener('change', function () { touched = true; }); });
      var io = new IntersectionObserver(function (entries) {
        if (!entries[0].isIntersecting) return;
        io.disconnect();
        setTimeout(function () { if (!touched) to.checked = true; }, 700);
      }, { threshold: 0.35 });
      io.observe(from.closest('.switch-demo'));
    });
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
      playBtn.querySelector('.label').textContent = playing ? 'Pause' : 'Play';
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
    var showStars = function (text) {
      // Screen readers hear "GitHub, 180 stars", not a bare number.
      starEls.forEach(function (el) {
        el.textContent = '';
        el.appendChild(goldStar());
        el.appendChild(document.createTextNode(text));
        var unit = document.createElement('span');
        unit.className = 'visually-hidden';
        unit.textContent = text === '1' ? ' star' : ' stars';
        el.appendChild(unit);
      });
    };
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
    btn.addEventListener('click', function () {
      var target = document.getElementById(btn.getAttribute('data-copy'));
      if (!target) return;
      var original = btn.textContent;
      copyText(target.textContent.trim(), function () {
        btn.textContent = 'Copied';
        setTimeout(function () { btn.textContent = original; }, 1800);
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
          ? 'What you expected, what happened instead, and any message word for word.'
          : kind() === 'Idea'
            ? 'What would make EternalMonitor more useful for you?'
            : 'Anything at all. How you use it, what you study, what you draw.';
      }
    };
    Array.prototype.forEach.call(form.querySelectorAll('input[name="kind"]'), function (r) {
      r.addEventListener('change', syncKind);
    });
    syncKind();

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
})();
