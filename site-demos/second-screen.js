// Extend your desktop. In the Windows app's own footage the pointer picks
// Second screen (the click lands at 1.47 s) and leaves the window on the
// right. The iPad's stream restarts and it becomes the desktop's second
// display; the film's pointer then carries the reference sheet off the
// monitor onto the iPad, and the finished desk holds for almost four seconds.
// The loop is both screens blinking dark, as they do when Windows rearranges
// its displays, and coming back mirrored.
//
// The root's width picks the framing, so second-screen-m.html shares this.
// readme-demo.html plays it as one scene: there it leaves the background to
// the host and hands its timeline over in window.__films.
(function () {
  const root = document.getElementById('root');
  const hosted = !!document.querySelector('[data-film="second-screen"]');
  const ID = root.dataset.compositionId;
  const W = +root.dataset.width, H = +root.dataset.height;
  const tl = gsap.timeline({ paused: true });

  // World pixels per desktop pixel, and where each desktop sits in the world.
  const MON = { x: 0, y: 0, s: 0.6 };
  const PAD = { x: 880, y: 470, s: 0.45375 };
  const SECOND_AT = 1920;                    // the iPad's display, right of the monitor
  const APP = { x: 60, y: 110 };             // the app window's top-left on the monitor
  const DOC = { x: 1120, y: 96 };            // the sheet on the monitor
  const DOC_TO = { x: SECOND_AT + 340, y: 92 };
  const GRIP = { x: 380, y: 14 };            // where the pointer holds the sheet's title bar

  // Camera poses: the world point at the frame's middle, and its scale.
  const POSES = {
    1480: {
      rest: { x: 768, y: 440, s: 0.92 },
      settle: { x: 760, y: 440, s: 0.89 },
      close: { x: 294, y: 186, s: 2.5 },
      carry: { x: 1000, y: 330, s: 1.15 },
    },
    728: {
      rest: { x: 1150, y: 470, s: 0.8 },
      settle: { x: 1140, y: 470, s: 0.77 },
      close: { x: 300, y: 186, s: 2.1 },
      carry: { x: 1130, y: 360, s: 0.86 },
    },
    1280: {
      rest: { x: 768, y: 466, s: 0.73 },
      settle: { x: 760, y: 466, s: 0.71 },
      close: { x: 300, y: 190, s: 2.2 },
      carry: { x: 1000, y: 400, s: 1.0 },
    },
  }[W];
  const cam = (p) => ({ x: W / 2 - p.x * p.s, y: H / 2 - p.y * p.s, scale: p.s });

  if (!hosted) {
    tl.fromTo('#bg', { scale: 1.06, x: 0 }, { scale: 1, x: -16, duration: 4, ease: 'sine.inOut' }, 0);
    tl.to('#bg', { scale: 1.06, x: 0, duration: 4, ease: 'sine.inOut' }, 4);
  }

  // Camera: in on the app's switch, out to follow the sheet, then the desk.
  tl.set('#world', { ...cam(POSES.rest), transformOrigin: '0 0' }, 0);
  tl.to('#world', { ...cam(POSES.close), duration: 0.85, ease: 'power2.inOut' }, 0.3);
  tl.to('#world', { ...cam(POSES.carry), duration: 0.9, ease: 'power2.inOut' }, 1.75);
  tl.to('#world', { ...cam(POSES.settle), duration: 0.8, ease: 'power2.inOut' }, 3.7);
  tl.to('#world', { ...cam(POSES.rest), duration: 3.1, ease: 'sine.inOut' }, 4.5);

  // Screens: dark at the seam, on at the start.
  tl.fromTo(['#monDark', '#padDark'], { opacity: 1 }, { opacity: 0, duration: 0.35, ease: 'power1.out', immediateRender: true }, 0);
  tl.to(['#monDark', '#padDark'], { opacity: 1, duration: 0.3, ease: 'power1.in' }, 7.68);

  // Mirror, then Second screen: the iPad's stream restarts on the switch.
  tl.set('#extend', { opacity: 0 }, 0);
  tl.set('#mirror', { opacity: 1 }, 0);
  tl.to('#padDark', { opacity: 1, duration: 0.18, ease: 'power1.in' }, 1.62);
  tl.set('#mirror', { opacity: 0 }, 1.95);
  tl.set('#extend', { opacity: 1 }, 1.95);
  tl.to('#padDark', { opacity: 0, duration: 0.3, ease: 'power1.out' }, 2.05);

  // The chip says which one the iPad is.
  const say = (at, show, hide, width) => {
    tl.to(hide, { opacity: 0, y: -8, duration: 0.2, ease: 'power2.in' }, at);
    tl.fromTo(show, { opacity: 0, y: 8 }, { opacity: 1, y: 0, duration: 0.3, ease: 'power3.out', immediateRender: false }, at + 0.15);
    tl.to('#chip', { width, duration: 0.35, ease: 'power3.inOut' }, at + 0.05);
  };
  tl.set('#say-second', { opacity: 0 }, 0);
  tl.set('#chip', { width: 300 }, 0);
  say(1.52, '#say-second', '#say-mirror', 270);
  say(7.74, '#say-mirror', '#say-second', 300);

  // The sheet: one copy per desktop, each placed in its own display's pixels.
  const place = (sel, p, origin) => tl.set(sel, { x: p.x - origin, y: p.y }, 0);
  place('#docA', DOC, 0);
  place('#docM', DOC, 0);
  place('#docB', DOC, SECOND_AT);

  // The pointer: where the app's own pointer left the window (from
  // footage/host-second-screen.json), to the sheet's title bar, then across.
  const HAND = { t: 2.44, x: APP.x + 1036, y: APP.y + 30 + 288 - 10 };
  const grip = { x: DOC.x + GRIP.x, y: DOC.y + GRIP.y };
  tl.set('#ptrA', { x: HAND.x, y: HAND.y }, 0);
  tl.set('#ptrB', { x: HAND.x - SECOND_AT, y: HAND.y }, 0);
  tl.to(['#ptrA', '#ptrB'], { opacity: 1, duration: 0.05 }, HAND.t);
  tl.to('#ptrA', { x: grip.x, y: grip.y, duration: 0.44, ease: 'power2.out' }, HAND.t + 0.02);
  tl.to('#ptrB', { x: grip.x - SECOND_AT, y: grip.y, duration: 0.44, ease: 'power2.out' }, HAND.t + 0.02);
  tl.to(['#ptrA', '#ptrB'], { scale: 0.86, duration: 0.08, ease: 'power2.in' }, 2.9);
  const dx = DOC_TO.x - DOC.x, dy = DOC_TO.y - DOC.y;
  tl.to(['#docA', '#docB', '#ptrA', '#ptrB'], { x: `+=${dx}`, y: `+=${dy}`, duration: 1.15, ease: 'power2.inOut' }, 2.98);
  tl.to(['#ptrA', '#ptrB'], { scale: 1, duration: 0.08, ease: 'power2.out' }, 4.16);
  tl.to(['#ptrA', '#ptrB'], { x: '+=120', y: '+=170', duration: 0.7, ease: 'power2.inOut' }, 4.3);
  tl.to(['#ptrA', '#ptrB'], { opacity: 0, duration: 0.3 }, 4.75);

  if (hosted) (window.__films = window.__films || {})['second-screen'] = tl;
  else window.__timelines[ID] = tl;
})();
