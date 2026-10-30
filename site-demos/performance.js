// Your graphics card does the work. Three PCs, one per GPU maker, each in
// its own session with its own counters and its own encode-time trace. The
// film slides from one window to the next at 2.67 s, 5.33 s and on the loop,
// and the chip names whose card is doing the work. Each window is placed by a
// function of the timeline's time, so the loop's slide meets itself.
//
// The root's width picks the framing, so performance-m.html shares this.
(function () {
  const root = document.getElementById('root');
  const ID = root.dataset.compositionId;
  const W = +root.dataset.width, H = +root.dataset.height;
  const tl = gsap.timeline({ paused: true });
  const SEAM = 8 / 3;                        // one session's turn
  const HALF = 0.22;                         // half a slide
  // The chart and the Encoder and Session cards, a little closer by the end.
  const FRAME = {
    1480: { cx: 620, cy: 548, zoom: 1.25, push: 1.33, at: { x: 760, y: 548 } },
    728: { cx: 632, cy: 479, zoom: 0.96, push: 0.99, at: { x: 364, y: 520 } },
  }[W];
  const GAP = W + 120;                       // how far the next window waits
  const slide = gsap.parseEase('power3.inOut');
  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));

  // Session k is centred on screen from k * SEAM to (k + 1) * SEAM; before
  // and after, it is sliding in from the right or out to the left.
  // `from` is when the window's own session starts in film time.
  const windows = [
    { el: document.getElementById('win-i0'), from: -SEAM },
    { el: document.getElementById('win-n'), from: 0 },
    { el: document.getElementById('win-a'), from: SEAM },
    { el: document.getElementById('win-i'), from: 2 * SEAM },
    { el: document.getElementById('win-n2'), from: 3 * SEAM },
  ];
  function place(win, t) {
    const local = t - win.from;
    let off;
    if (local < -HALF || local > SEAM + HALF) off = null;
    else if (local < HALF) off = GAP * (1 - slide((local + HALF) / (2 * HALF)));
    else if (local > SEAM - HALF) off = -GAP * slide((local - SEAM + HALF) / (2 * HALF));
    else off = 0;
    if (off === null) { win.el.style.opacity = 0; return; }
    win.el.style.opacity = 1;
    const k = clamp(local / SEAM, 0, 1);
    const p = pose(FRAME.cx, FRAME.cy, FRAME.zoom + (FRAME.push - FRAME.zoom) * k, FRAME.at);
    win.el.style.transform = `translate(${p.x + off}px, ${p.y}px) scale(${p.scale})`;
  }
  // The chip lights the maker whose window is on screen, crossing over mid-slide.
  const names = [0, 1, 2].map((i) => document.getElementById('g' + i));
  const lit = gsap.utils.interpolate('#71717a', '#f4f4f5');
  function chip(t) {
    names.forEach((el, i) => {
      // Distance from this session's middle, wrapping around the loop.
      let d = Math.abs(((t - (i + 0.5) * SEAM + 12) % 8) - 4);
      const on = 1 - clamp((d - SEAM / 2 + 0.12) / 0.24, 0, 1);
      el.style.color = lit(on);
    });
  }
  function frame(t) {
    windows.forEach((w) => place(w, t));
    chip(t);
  }

  tl.fromTo('#bg', { scale: 1.06, x: 0 }, { scale: 1, x: -16, duration: 4, ease: 'sine.inOut' }, 0);
  tl.to('#bg', { scale: 1.06, x: 0, duration: 4, ease: 'sine.inOut' }, 4);
  let now = 0;
  const clock = { get t() { return now; }, set t(v) { now = v; frame(v); } };
  tl.fromTo(clock, { t: 0 }, { t: 8, duration: 8, ease: 'none', immediateRender: false }, 0);
  frame(0);
  window.__timelines[ID] = tl;
})();
