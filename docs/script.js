/* EternalMonitor — Site Scripts */

(function () {
  'use strict';

  var reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var nav = document.querySelector('.nav');

  /* --- macOS Notice --- */
  // iPadOS reports platform 'MacIntel' too, so require a non-touch device.
  var isMac = /Mac/.test(navigator.platform) && navigator.maxTouchPoints <= 1;
  if (isMac && nav && sessionStorage.getItem('mac-notice-dismissed') !== '1') {
    var notice = document.createElement('aside');
    notice.className = 'mac-notice';
    notice.setAttribute('aria-label', 'Notice for Mac visitors');
    notice.innerHTML =
      '<div class="container mac-notice-inner">' +
      '<p><strong>On a Mac?</strong>EternalMonitor streams from Windows PCs. ' +
      'macOS already does this with <a href="https://support.apple.com/en-us/102386" target="_blank" rel="noopener">Sidecar</a>, ' +
      'which turns an iPad into a second display for free.</p>' +
      '<button type="button" class="mac-notice-close" aria-label="Dismiss">&times;</button>' +
      '</div>';
    notice.querySelector('.mac-notice-close').addEventListener('click', function () {
      sessionStorage.setItem('mac-notice-dismissed', '1');
      notice.remove();
    });
    nav.insertAdjacentElement('afterend', notice);
  }

  /* --- Nav: solid once the page scrolls, and the section in view --- */
  if (nav) {
    var navTick = false;
    var updateNav = function () {
      nav.classList.toggle('scrolled', window.scrollY > 8);
      navTick = false;
    };
    window.addEventListener('scroll', function () {
      if (!navTick) {
        navTick = true;
        window.requestAnimationFrame(updateNav);
      }
    }, { passive: true });
    updateNav();
  }

  var sectionLinks = Array.prototype.slice.call(document.querySelectorAll('.nav-link[href^="#"]'));
  if (sectionLinks.length && 'IntersectionObserver' in window) {
    var linkFor = {};
    sectionLinks.forEach(function (link) { linkFor[link.getAttribute('href').slice(1)] = link; });
    // A thin band just above the middle of the viewport decides which
    // section is "current".
    var sectionObserver = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        var link = linkFor[entry.target.id];
        if (entry.isIntersecting) {
          sectionLinks.forEach(function (l) { l.classList.remove('active'); });
          link.classList.add('active');
        } else {
          link.classList.remove('active');
        }
      });
    }, { rootMargin: '-45% 0px -50% 0px' });
    Object.keys(linkFor).forEach(function (id) {
      var section = document.getElementById(id);
      if (section) sectionObserver.observe(section);
    });
  }

  /* --- Scroll Reveal (IntersectionObserver) --- */
  var reveals = document.querySelectorAll('.reveal');
  if (reveals.length && 'IntersectionObserver' in window) {
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('visible');
          observer.unobserve(entry.target);
        }
      });
    }, { threshold: 0.15, rootMargin: '0px 0px -40px 0px' });

    reveals.forEach(function (el) { observer.observe(el); });
  } else {
    // Fallback: show everything immediately
    reveals.forEach(function (el) { el.classList.add('visible'); });
  }

  /* --- Launch film: one lime button, native controls once it runs --- */
  var launch = document.getElementById('launch-video');
  var launchPlay = document.getElementById('launch-play');
  if (launch && launchPlay) {
    launch.removeAttribute('controls');
    launchPlay.hidden = false;
    launchPlay.addEventListener('click', function () {
      launch.setAttribute('controls', '');
      var playing = launch.play();
      if (playing && playing.catch) playing.catch(function () {});
    });
    launch.addEventListener('play', function () { launchPlay.hidden = true; });
    launch.addEventListener('ended', function () { launchPlay.hidden = false; });
  }

  /* --- Card spotlight follows the pointer --- */
  if (window.matchMedia('(hover: hover)').matches) {
    document.querySelectorAll('.feature, .step, .req-card, .download-card').forEach(function (card) {
      card.addEventListener('pointermove', function (e) {
        var rect = card.getBoundingClientRect();
        card.style.setProperty('--mx', (e.clientX - rect.left) + 'px');
        card.style.setProperty('--my', (e.clientY - rect.top) + 'px');
      });
    });
  }

  /* --- Smooth Scroll for Anchor Links --- */
  document.querySelectorAll('a[href^="#"]').forEach(function (link) {
    link.addEventListener('click', function (e) {
      var target = document.querySelector(this.getAttribute('href'));
      if (target) {
        e.preventDefault();
        target.scrollIntoView({ behavior: reducedMotion ? 'auto' : 'smooth' });
      }
    });
  });

  /* --- GitHub Releases API Fetch --- */
  var downloadBtn = document.getElementById('download-btn');
  var versionEl = document.getElementById('release-version');
  var metaEl = document.getElementById('release-meta');
  var sha256El = document.getElementById('sha256-value');

  if (!downloadBtn) return; // Not on download page

  function formatBytes(bytes) {
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1048576) return (bytes / 1024).toFixed(1) + ' KB';
    return (bytes / 1048576).toFixed(1) + ' MB';
  }

  // Keep the Windows download paired with the TestFlight build shown in the HTML.
  fetch('https://api.github.com/repos/sidebandstudio/EternalMonitor/releases/tags/v0.3.0', {
    headers: { 'Accept': 'application/vnd.github.v3+json' }
  })
    .then(function (res) {
      if (!res.ok) throw new Error('HTTP ' + res.status);
      return res.json();
    })
    .then(function (release) {
      var asset = release.assets.find(function (a) {
        return a.name === 'EternalMonitor-Setup.exe';
      });

      if (!asset) throw new Error('No Windows asset found');

      downloadBtn.href = asset.browser_download_url;
      if (versionEl) versionEl.textContent = release.tag_name;
      if (metaEl) metaEl.textContent = asset.name + ' \u00B7 ' + formatBytes(asset.size);

      // Try to extract SHA256 from release body
      if (sha256El && release.body) {
        var match = release.body.match(/[a-fA-F0-9]{64}/);
        if (match) sha256El.textContent = match[0];
      }
    })
    .catch(function () {
      // The matching download, size and checksum are already present in the HTML.
    });
})();
