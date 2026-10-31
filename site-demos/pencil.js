// Apple Pencil film. Everything that moves is a function of the timeline's
// time, so any frame can be rendered on its own:
//
// - The camera starts on the whole iPad, where the app's own bar says Live,
//   pushes in on the painting as the bar hides itself, and pulls back out.
// - The pencil paints seven blades of moonlit grass with pressure (thick where
//   it presses, a thin flick where it lifts), then lays over and shades the
//   water with the side of the tip. Ink is perfect-freehand fed each sample's
//   pressure; the shading is a dab brush on the paper's tooth that grows as
//   the pencil tilts.
// - The pencil's angle, tilt (seen as a longer pencil and a nearer shadow) and
//   height follow the same samples as the strokes.
//
// Coordinates are the iPad screen's own pixels (2420 x 1668). The root's
// width picks the framing, so pencil-m.html shares this script. readme-demo.html
// and launch-film.html play it as one scene: there it leaves the background to
// the host and hands its timeline over in window.__films.
(function () {
  const root = document.getElementById('root');
  const hosted = !!document.querySelector('[data-film="pencil"]');
  const ID = root.dataset.compositionId;
  const W = +root.dataset.width, H = +root.dataset.height;
  const SCREEN = { x: 40, y: 40 };           // the screen inside the iPad frame
  const TIP = { x: 52, y: 740 };             // the pencil's tip in its 800px square
  const SHOTS = {
    // centre of the shot in screen pixels, and frame pixels per screen pixel
    1104: { wide: { x: 1210, y: 834, s: 0.4 }, close: { x: 1790, y: 1250, s: 0.72 } },
    728: { wide: { x: 1500, y: 930, s: 0.4 }, close: { x: 1790, y: 1350, s: 0.62 } },
    1280: { wide: { x: 1210, y: 834, s: 0.38 }, close: { x: 1690, y: 1250, s: 0.7 } },
    // launch-film.html: lower in the frame, under the film's title.
    1920: { wide: { x: 1210, y: 621, s: 0.47 }, close: { x: 1790, y: 1190, s: 0.9 } },
  };
  const shot = SHOTS[W];
  const ease = gsap.parseEase('power2.inOut');
  const sine = gsap.parseEase('sine.inOut');
  const lerp = (a, b, k) => a + (b - a) * k;
  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
  const smooth = (a, b, v) => { const k = clamp((v - a) / (b - a), 0, 1); return k * k * (3 - 2 * k); };

  // Seeded randomness, so every render is the same.
  function rng(seed) {
    let s = seed >>> 0;
    return () => {
      s = (s + 0x6d2b79f5) >>> 0;
      let t = s;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  // Smooth noise in 0..1, for a hand that is never quite steady.
  function wobble(seed) {
    const r = rng(seed), v = Array.from({ length: 64 }, r);
    return (x) => {
      const i = Math.floor(x), f = x - i, u = f * f * (3 - 2 * f);
      return lerp(v[((i % 64) + 64) % 64], v[(((i + 1) % 64) + 64) % 64], u);
    };
  }

  // --- The strokes ---------------------------------------------------------
  // A sample is { t, x, y, p (pressure 0..1), alt (degrees from the glass),
  // az (degrees, the direction from the tip to the pencil's end) }.
  const N = 90;
  function blade(seed, p0, c, p1, t0, t1, weight) {
    const n = wobble(seed), samples = [];
    for (let i = 0; i < N; i++) {
      const tau = i / (N - 1);
      const u = Math.pow(tau, 1.35);           // slow press, quick flick
      const x = (1 - u) * (1 - u) * p0[0] + 2 * (1 - u) * u * c[0] + u * u * p1[0];
      const y = (1 - u) * (1 - u) * p0[1] + 2 * (1 - u) * u * c[1] + u * u * p1[1];
      const press = Math.pow(Math.min(1, u / 0.06), 0.7) * (0.96 - 0.8 * Math.pow(u, 1.25));
      samples.push({
        t: lerp(t0, t1, tau), x, y,
        p: clamp(weight * press + 0.07 * (n(u * 7) - 0.5), 0.04, 1),
        alt: 60 - 7 * u + 4 * (n(u * 3 + 9) - 0.5),
        az: 36 + 8 * (n(u * 2 + 20) - 0.5),
      });
    }
    return { kind: 'ink', t0, t1, samples };
  }
  function shading(seed, pts, t0, t1) {
    // A zig-zag, resampled every few pixels, at an even hand speed.
    const n = wobble(seed), path = [];
    let total = 0;
    for (let i = 1; i < pts.length; i++) total += Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]);
    const count = Math.round(total / 5);
    for (let k = 0; k <= count; k++) {
      let d = (k / count) * total, i = 1;
      for (; i < pts.length; i++) {
        const seg = Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]);
        if (d <= seg) break;
        d -= seg;
      }
      i = Math.min(i, pts.length - 1);
      const a = pts[i - 1], b = pts[i];
      const seg = Math.hypot(b[0] - a[0], b[1] - a[1]) || 1;
      const tau = k / count;
      path.push({
        t: lerp(t0, t1, sine(tau) * 0.5 + tau * 0.5),
        x: lerp(a[0], b[0], d / seg), y: lerp(a[1], b[1], d / seg) + 6 * (n(tau * 9) - 0.5),
        p: clamp(0.5 + 0.35 * (n(tau * 6 + 4) - 0.5) + 0.15 * Math.sin(tau * Math.PI), 0.2, 1),
        alt: 23 + 4 * (n(tau * 4 + 30) - 0.5),
        az: 30 + 6 * (n(tau * 3 + 40) - 0.5),
        dir: Math.atan2(b[1] - a[1], b[0] - a[0]),
        // Each pass fades in and out where the hand turns.
        turn: Math.min(d, seg - d),
      });
    }
    return { kind: 'shade', t0, t1, samples: path };
  }
  const strokes = [
    blade(11, [1790, 1512], [1776, 1320], [1700, 1080], 1.95, 2.45, 1),
    blade(12, [1832, 1520], [1846, 1340], [1900, 1130], 2.58, 3.0, 0.95),
    blade(13, [1744, 1520], [1726, 1380], [1630, 1220], 3.12, 3.46, 0.85),
    blade(14, [1876, 1526], [1904, 1400], [1990, 1270], 3.58, 3.9, 0.8),
    blade(15, [1812, 1520], [1806, 1380], [1772, 1190], 4.0, 4.3, 0.9),
    blade(16, [1700, 1526], [1690, 1440], [1612, 1350], 4.4, 4.62, 0.7),
    blade(17, [1920, 1530], [1950, 1460], [2040, 1380], 4.72, 4.92, 0.65),
    shading(18, [[1560, 1108], [1960, 1116], [1620, 1142], [1975, 1150], [1540, 1178], [1965, 1188], [1660, 1214], [1950, 1224], [1720, 1250], [1930, 1258]], 5.05, 6.45),
  ];

  // --- The pencil between strokes -------------------------------------------
  // Off the bottom right, it comes in, lifts between strokes and leaves.
  const OFF = { x: 2560, y: 2060, alt: 55, az: 36 };
  const first = (s) => s.samples[0], last = (s) => s.samples[s.samples.length - 1];
  const moves = [{ t0: 0.85, t1: 1.95, from: OFF, to: first(strokes[0]) }];
  for (let i = 1; i < strokes.length; i++) moves.push({ t0: strokes[i - 1].t1, t1: strokes[i].t0, from: last(strokes[i - 1]), to: first(strokes[i]) });
  const REST = { x: 2150, y: 1380, alt: 55, az: 38 };
  moves.push({ t0: strokes[strokes.length - 1].t1, t1: 7.0, from: last(strokes[strokes.length - 1]), to: REST, settle: true });
  moves.push({ t0: 7.0, t1: 7.9, from: REST, to: OFF });

  function sampleAt(s, t) {
    const a = s.samples;
    const f = clamp((t - s.t0) / (s.t1 - s.t0), 0, 1) * (a.length - 1);
    const i = Math.min(Math.floor(f), a.length - 2), k = f - i;
    const A = a[i], B = a[i + 1];
    return { x: lerp(A.x, B.x, k), y: lerp(A.y, B.y, k), alt: lerp(A.alt, B.alt, k), az: lerp(A.az, B.az, k), h: 0 };
  }
  function pen(t) {
    for (const s of strokes) if (t >= s.t0 && t <= s.t1) return sampleAt(s, t);
    for (const m of moves) {
      if (t >= m.t0 && t <= m.t1) {
        const k = (t - m.t0) / (m.t1 - m.t0), e = ease(k);
        // Lift-off and touch-down are quick; the hover in between is high.
        const lift = m.settle ? smooth(0, 0.25, k) : m.from === OFF || m.to === OFF ? 1 : Math.sin(Math.PI * clamp(k, 0, 1)) ** 0.6 * clamp((m.t1 - m.t0) / 0.3, 0.35, 1);
        return { x: lerp(m.from.x, m.to.x, e), y: lerp(m.from.y, m.to.y, e) - 30 * lift, alt: lerp(m.from.alt, m.to.alt, e), az: lerp(m.from.az, m.to.az, e), h: lift };
      }
    }
    return { ...OFF, h: 1 };
  }

  // --- The camera ---------------------------------------------------------------
  function camera(t) {
    const w = shot.wide, c = shot.close;
    const breathe = 1 + 0.035 * sine(clamp((t - 2.1) / 4.9, 0, 1));
    const close = { x: c.x, y: c.y, s: c.s * breathe };
    let k = 0;
    if (t >= 1.0 && t < 2.1) k = ease((t - 1.0) / 1.1);
    else if (t >= 2.1 && t < 7.0) k = 1;
    else if (t >= 7.0 && t < 7.95) k = 1 - ease((t - 7.0) / 0.95);
    const s = w.s * Math.pow(close.s / w.s, k);
    return { x: lerp(w.x, close.x, k), y: lerp(w.y, close.y, k), s };
  }
  const toFrame = (cam, p) => ({ x: W / 2 + (p.x - cam.x) * cam.s, y: H / 2 + (p.y - cam.y) * cam.s });

  // --- Brushes --------------------------------------------------------------------
  const ink = document.getElementById('ink'), ctx = ink.getContext('2d');
  const layer = () => { const c = document.createElement('canvas'); c.width = ink.width; c.height = ink.height; return c; };
  const inkLayer = layer(), shadeLayer = layer();
  // Paper grain: fine noise, a little streaked along the pencil.
  const grain = document.createElement('canvas');
  grain.width = grain.height = 256;
  {
    const g = grain.getContext('2d'), img = g.createImageData(256, 256), r = rng(7);
    for (let i = 0; i < 256 * 256; i++) {
      const v = r();
      img.data[i * 4] = img.data[i * 4 + 1] = img.data[i * 4 + 2] = 255;
      img.data[i * 4 + 3] = Math.round(255 * (0.25 + 0.75 * v * v));
    }
    g.putImageData(img, 0, 0);
  }
  const grainBlur = document.createElement('canvas');
  grainBlur.width = grainBlur.height = 256;
  {
    const g = grainBlur.getContext('2d');
    g.filter = 'blur(0.6px)';
    for (const dx of [-256, 0, 256]) for (const dy of [-256, 0, 256]) g.drawImage(grain, dx, dy);
  }
  // The paper's tooth for shading: coarse, high-contrast, drawn out sideways.
  const tooth = document.createElement('canvas');
  tooth.width = 1024; tooth.height = 256;
  {
    const g = tooth.getContext('2d'), img = g.createImageData(256, 256), r = rng(9);
    for (let i = 0; i < 256 * 256; i++) {
      const v = r();
      img.data[i * 4] = img.data[i * 4 + 1] = img.data[i * 4 + 2] = 255;
      img.data[i * 4 + 3] = Math.round(255 * smooth(0.3, 0.8, v));
    }
    const src = document.createElement('canvas');
    src.width = src.height = 256;
    src.getContext('2d').putImageData(img, 0, 0);
    g.filter = 'blur(0.8px)';
    for (const dx of [-1024, 0, 1024]) for (const dy of [-256, 0, 256]) g.drawImage(src, dx, dy, 1024, 256);
  }
  // A soft round dab, stretched and turned for each sample.
  const dab = document.createElement('canvas');
  dab.width = dab.height = 64;
  {
    const g = dab.getContext('2d'), rg = g.createRadialGradient(32, 32, 0, 32, 32, 32);
    rg.addColorStop(0, 'rgba(255,255,255,1)');
    rg.addColorStop(0.55, 'rgba(255,255,255,0.75)');
    rg.addColorStop(1, 'rgba(255,255,255,0)');
    g.fillStyle = rg;
    g.fillRect(0, 0, 64, 64);
  }

  function texture(c, strength) {
    const g = c.getContext('2d');
    g.save();
    g.globalCompositeOperation = 'destination-out';
    g.globalAlpha = strength;
    g.fillStyle = g.createPattern(grainBlur, 'repeat');
    g.fillRect(0, 0, c.width, c.height);
    g.restore();
  }
  function tint(c, color) {
    const g = c.getContext('2d');
    g.save();
    g.globalCompositeOperation = 'source-in';
    g.fillStyle = color;
    g.fillRect(0, 0, c.width, c.height);
    g.restore();
  }

  function paint(t) {
    const g = inkLayer.getContext('2d'), sg = shadeLayer.getContext('2d');
    g.clearRect(0, 0, inkLayer.width, inkLayer.height);
    sg.clearRect(0, 0, shadeLayer.width, shadeLayer.height);
    let anyInk = false, anyShade = false;
    for (const s of strokes) {
      if (t < s.t0) continue;
      const done = t >= s.t1;
      const pts = s.samples.filter((p) => p.t <= t);
      if (!done) { const now = sampleAt(s, t); pts.push({ ...now, p: pts.length ? pts[pts.length - 1].p : 0.1 }); }
      if (pts.length < 2) continue;
      if (s.kind === 'ink') {
        // A translucent body and a brighter core: the edges stay soft like paint.
        for (const [size, alpha] of [[18, 0.55], [9, 0.6]]) {
          const outline = getStroke(pts.map((p) => [p.x, p.y, p.p]), {
            size, thinning: 0.82, smoothing: 0.62, streamline: 0.32,
            simulatePressure: false, last: done,
          });
          if (outline.length < 3) continue;
          g.beginPath();
          g.moveTo(outline[0][0], outline[0][1]);
          for (let i = 1; i < outline.length; i++) {
            const a = outline[i], b = outline[(i + 1) % outline.length];
            g.quadraticCurveTo(a[0], a[1], (a[0] + b[0]) / 2, (a[1] + b[1]) / 2);
          }
          g.closePath();
          g.globalAlpha = alpha;
          g.fillStyle = '#fff';
          g.fill();
          g.globalAlpha = 1;
        }
        anyInk = true;
      } else {
        for (const p of pts) {
          // Laid over, the tip leaves a broad mark; it thins where each pass turns.
          const tiltK = Math.cos((p.alt * Math.PI) / 180) ** 2;
          const ends = smooth(0, 60, p.turn);
          const rx = 14 + 30 * tiltK, ry = (6 + 9 * p.p) * (0.5 + 0.5 * ends);
          sg.save();
          sg.translate(p.x, p.y);
          sg.rotate(p.dir);
          sg.globalAlpha = (0.05 + 0.06 * p.p) * (0.3 + 0.7 * ends);
          sg.drawImage(dab, -rx, -ry, rx * 2, ry * 2);
          sg.restore();
        }
        anyShade = true;
      }
    }
    ctx.clearRect(0, 0, ink.width, ink.height);
    if (anyShade) {
      sg.save();
      sg.globalCompositeOperation = 'destination-in';
      sg.fillStyle = sg.createPattern(tooth, 'repeat');
      sg.fillRect(0, 0, shadeLayer.width, shadeLayer.height);
      sg.restore();
      tint(shadeLayer, '#d4e2e4');
      ctx.drawImage(shadeLayer, 0, 0);
    }
    if (anyInk) {
      texture(inkLayer, 0.38);
      tint(inkLayer, 'rgb(214, 226, 225)');
      ctx.save();
      ctx.shadowColor = 'rgba(214, 232, 232, 0.35)';
      ctx.shadowBlur = 8;
      ctx.drawImage(inkLayer, 0, 0);
      ctx.restore();
    }
  }

  // --- One frame ------------------------------------------------------------------
  const cam = document.getElementById('cam');
  const pencil = document.getElementById('pencil');
  const shadow = document.getElementById('shadow');
  const dip = document.getElementById('dip');
  function frame(t) {
    const c = camera(t);
    cam.style.transform = `translate(${W / 2 - (SCREEN.x + c.x) * c.s}px, ${H / 2 - (SCREEN.y + c.y) * c.s}px) scale(${c.s})`;
    const p = pen(t);
    const f = toFrame(c, p);
    const k = c.s / 0.72;
    const fore = 0.62 + 0.75 * Math.cos((p.alt * Math.PI) / 180);
    pencil.style.left = `${f.x - TIP.x}px`;
    pencil.style.top = `${f.y - TIP.y}px`;
    pencil.style.transform = `scale(${k}) rotate(${p.az + 45}deg) rotate(-45deg) scaleX(${fore}) rotate(45deg)`;
    const lowness = 1 - Math.sin((p.alt * Math.PI) / 180);
    shadow.style.filter = `drop-shadow(${-(12 + 34 * p.h - 6 * lowness) * k}px ${(16 + 40 * p.h - 6 * lowness) * k}px ${(14 + 24 * p.h) * k}px rgba(0, 0, 0, ${0.58 - 0.18 * p.h}))`;
    dip.style.opacity = t < 0.3 ? 1 - smooth(0, 0.3, t) : smooth(7.62, 7.98, t);
    paint(t);
  }

  const tl = gsap.timeline({ paused: true });
  if (!hosted) {
    tl.fromTo('#bg', { scale: 1.06, x: 0 }, { scale: 1, x: -16, duration: 4, ease: 'sine.inOut' }, 0);
    tl.to('#bg', { scale: 1.06, x: 0, duration: 4, ease: 'sine.inOut' }, 4);
  }
  // GSAP writes the time into this setter on every seek, callbacks or not.
  let now = 0;
  const clock = { get t() { return now; }, set t(v) { now = v; frame(v); } };
  tl.fromTo(clock, { t: 0 }, { t: 8, duration: 8, ease: 'none', immediateRender: false }, 0);
  frame(0);
  if (hosted) (window.__films = window.__films || {}).pencil = tl;
  else window.__timelines[ID] = tl;
})();
