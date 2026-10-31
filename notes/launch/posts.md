# Launch posts

Three posts for three audiences. Post them on different days, from your own
account, and stay in the thread for the first few hours to answer questions.
Each one links straight to the repository:
https://github.com/sidebandstudio/EternalMonitor

Every claim below comes from README.md, docs/pencil-drawing.md,
ARCHITECTURE.md or docs/index.html. If you edit a post, keep it to what those
files say. The download count is from `scripts/growth_report.py` on
2026-10-31; run it again on the day you post.

---

## 1. An extra monitor for a Windows PC

**Venue.** r/ipad. Second choice r/apple, as a developer self-post on a Sunday.

**Venue rules (note for Ali).** I could not open r/ipad's rules page from the
build machine, because Reddit blocks it without a login. Read
https://www.reddit.com/r/ipad/about/rules before posting and look for a weekly
self-promotion thread. A rules index (threadfox.vip, read 2026-09-26) says
r/apple allows developer self-promotion on Sundays only, Pacific time, as a
self-post, from accounts that also take part in the subreddit. Reddit's own
moderator help says many communities expect most of your activity there to be
something other than your own project. Say in the post that you made it.

**Title.** I made a free app that turns an iPad into a second monitor for a Windows PC, over Wi-Fi or USB

**Attach.** The launch film, `launch-film.mp4` (README.md, "Launch film"): 35 s
of the site's hero switching from Wi-Fi to USB, a window carried onto the iPad
as a second screen, Apple Pencil strokes, three GPU makers and the name. It
shows everything this post lists, so it suits this post best, and it is the one
to use on X and in Discord too. For a short loop instead, attach
`docs/assets/demos/second-screen.mp4` (a window carried from the monitor onto
the iPad); on a phone-first venue use `second-screen-m.mp4`.

**Body.**

I made EternalMonitor, a Windows app and an iPad app that turn the iPad into a
monitor for your PC. It's free, open source under MIT, and needs no account.
(Add a line here about why you built it. Your own reason will beat anything
written for you.)

What it does:

- Adds the iPad as an extra display beside your monitor, or mirrors any screen
  you have. Drag windows onto it like any other monitor.
- Streams over your home Wi-Fi, or over a USB cable for a steadier picture that
  also charges the iPad. Plug the cable in during a Wi-Fi session and it takes
  over; unplug it and the stream carries on over Wi-Fi.
- Your graphics card does the encoding: NVIDIA, AMD or Intel, at 30, 60, 90 or
  120 fps (120 on iPads with ProMotion). The bitrate adapts between 4 and
  50 Mbps.
- When Wi-Fi drops a packet, the iPad asks for that piece again right away, so
  one lost packet rarely costs a frame.
- Touch works as a mouse: tap to click, two fingers to scroll, hold to
  right-click. Keyboards, trackpads and mice paired to the iPad work too.
- PC sound plays on the iPad, and you can mute it without stopping the video.
- Apple Pencil arrives in Windows as a real pen with pressure and tilt.
- Pair once with a six-digit code or a QR scan. Over USB there's no code at all.
- Updates take one click.

You need Windows 10 (1809 or later) or 11 with NVIDIA, AMD or Intel graphics,
and an iPad on iPadOS 17 or later. The Windows installer is on GitHub and
eternalmonitor.dev, and the iPad app comes through TestFlight. The Windows
installer has been downloaded over 2,000 times so far.

Repo: https://github.com/sidebandstudio/EternalMonitor
Website: https://eternalmonitor.dev

Happy to answer questions about setups. I'd love to hear what you pair it with.

---

## 2. Apple Pencil in Windows art apps

**Venue.** r/ClipStudio. Clip Studio Paint is the one art app the docs cover
step by step, so this post names only it.

**Venue rules (note for Ali).** I could not open r/ClipStudio's rules page or
find a quoted copy. Read https://www.reddit.com/r/ClipStudio/about/rules before
posting. Art subreddits often limit tool promotion to a flair or a weekly
thread, so look for both. Lead with the drawing, not the download link, and
say you made the app.

**Title.** Apple Pencil as a pressure-sensitive pen in Clip Studio Paint on Windows, with a free app I made

**Attach.** `docs/assets/demos/pencil.mp4` for now (pressure strokes and tilt
shading). Swap in the real recording from shots.md, shot 2, once it's filmed.
A real canvas in CSP will land much better here than any animation.

**Body.**

If you draw in Clip Studio Paint on a Windows PC and own an iPad and Apple
Pencil, EternalMonitor turns them into a pen display for that PC. It's free and
open source (MIT), with no account.

- Windows receives the Pencil as a Windows Ink pen, with pressure and tilt, not
  as a mouse. Tilt works with any brush that has tilt turned on.
- Drawing mode ignores your palm and fingers on the canvas, so you can rest
  your hand while you draw. The toolbar stays one tap away.
- The pen lands under the tip on any monitor layout, including a monitor to the
  left of or above your main one.
- Over a USB cable, Drawing mode asks for the iPad's highest refresh rate, and
  the PC can stream at up to 120 fps.
- Every Pencil sample reaches Windows once, in order. The iPad sends the
  precise positions it measures and no predicted points, which CSP couldn't
  take back once painted.

Setup in CSP takes one setting: Preferences > Tablet > Using tablet service >
Tablet PC. The full guide, including pressure settings, is here:
https://github.com/sidebandstudio/EternalMonitor/blob/main/docs/pencil-drawing.md

The Windows app is on eternalmonitor.dev, and the iPad app comes through
TestFlight. Apple Pencil 1st generation, 2nd generation and Pro all send
pressure.

Repo: https://github.com/sidebandstudio/EternalMonitor

If you try it, I'd love to see what you draw with it.

---

## 3. Developers: a Rust and Swift streaming implementation

**Venue.** Show HN first. Then r/rust a few days later, with the Rust details
up front.

**Venue rules (note for Ali).**

- Show HN (https://news.ycombinator.com/showhn.html, read 2026-10-31). The
  title must start with "Show HN". It has to be something people can try, and
  you have to be around to discuss it. Release posts ("Foo 1.3.1 is out") don't
  count, so post the project as a whole, not a version. Don't ask anyone to
  upvote or comment. Post the repo URL; HN has no image attachments. Add the
  first comment yourself with the body below.
- r/rust. I could not open the rules page. A quoted copy (threadfox.vip, read
  2026-09-26) shows no self-promotion rule, but posts must be about Rust,
  low-effort posts are removed, and the Rust Code of Conduct applies. Check
  https://www.reddit.com/r/rust/about/rules before posting. Keep the Rust
  details first and the app second.

**Show HN title.** Show HN: EternalMonitor, an iPad as a second monitor for Windows, in Rust and Swift

**r/rust title.** EternalMonitor: a Rust host that streams a Windows desktop to an iPad with UDP loss repair and Windows Ink pen injection

**Attach (r/rust).** `docs/assets/readme/how-it-works-dark.svg` exported to
PNG, or `docs/assets/demos/performance.mp4` (three PCs, one per GPU maker, each
encoding on its own card).

**Body.**

EternalMonitor streams a Windows desktop to an iPad and sends touch, keyboard
and Apple Pencil input back. The host is Rust, the iPad app is native Swift,
and it's MIT licensed.

How the pipeline works:

- Capture uses DXGI desktop duplication on its own thread and publishes only
  the latest frame, so a slow encoder skips stale frames instead of queueing
  them.
- Encoding goes through FFmpeg to NVENC, AMF or Quick Sync (H.264 by default,
  HEVC optional), at 30, 60, 90 or 120 fps. Encoded frames travel on a bounded
  channel, because dropping an arbitrary P-frame would corrupt the GOP.
- Each frame goes out as numbered UDP fragments. The host keeps the exact
  first-transmission datagrams for the last 96 frames (12 MiB at most). The
  iPad NACKs a missing fragment as soon as a later one shows the gap, and holds
  up to eight frames behind an incomplete one for an RTT-aware 8 to 25 ms
  repair window. Keyframe requests are the fallback.
- Adaptive bitrate moves along a 4 to 50 Mbps ladder from receiver reports sent
  every 500 ms: unrecovered loss, repair pressure, jitter and queue depth.
- Over USB, the host talks to Apple's usbmuxd service and carries the same
  datagrams through a length-framed tunnel, so one protocol serves both links.
  A cable takes over a live Wi-Fi session.
- On the iPad, Swift reassembles fragments, decodes with VideoToolbox and draws
  NV12 with Metal.
- PC audio is WASAPI loopback to 20 ms Opus packets, played through a jitter
  buffer that starts at 60 ms and grows to 120 ms only after repeated
  underruns. Lost packets use Opus concealment.
- Apple Pencil becomes a native Windows pen: the host creates a PT_PEN device
  and injects pressure, signed tilt and hover with
  InjectSyntheticPointerInput, in physical desktop pixels.

Rust and Swift share golden vectors for the wire format. CI runs the
Rust suites on Linux, macOS and Windows, the iPad tests, and a streaming system
test on every pull request. On macOS the host swaps DXGI for a synthetic test
pattern, so the protocol, encoder, transport and supervisor all run for real
without Windows, and the end-to-end matrix checks decoded frame rate, drops,
repairs and rendered pixels.

ARCHITECTURE.md walks through the whole pipeline, and docs/decisions.md says
why each part works the way it does, with measurements.

Repo: https://github.com/sidebandstudio/EternalMonitor

I'm happy to go into any part of it: the repair window, the pen injection, or
running the host on a Mac for development.
