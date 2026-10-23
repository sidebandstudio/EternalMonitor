## EternalMonitor (unreleased)

Apple Pencil placement on PCs with more than one monitor, plain-language setup
and problem messages in both apps, in-app updates, and Windows reliability and
startup fixes from 0.3.1 support diagnostics. iPads on 0.3.1 work with the
Windows changes.

### What changed

- Apple Pencil lands under the tip when another monitor sits to the left of or
  above the main display, including an extended iPad display placed there.
  Previously the pen moved left or up by that monitor's size, and strokes near
  the far edge stopped with "Windows rejected native pen input".
- After Wi-Fi or the PC stalls for a moment, the Apple Pencil samples that
  arrive together are sent to Windows a quarter of a millisecond apart.
  Windows refused some of them when they came faster, with "Windows
  rejected native pen input" (0x80070057), even on a PC with one monitor.
- When the app you draw or type in runs as administrator, Windows blocks
  Pencil, touch and keyboard input from EternalMonitor. The Stream page now
  names that app and says to open both the same way.
- After Windows cancels a pen contact, for example when a display changes mode,
  the stroke continues from the same Pencil sample instead of being dropped
  until the Pencil lifts. A Pencil held still follows a display that moves.
- The Windows host uses physical pixels from startup on scaled displays, and
  touch clicks land on the exact pixel on every monitor layout, including very
  wide desktops.
- If Windows cannot create its pen device, Pencil outside Drawing mode works as
  a mouse instead of doing nothing.
- The host log records the input mapping geometry for each session.
- The Windows Stream page asks **Use the iPad as** with **Mirror my screen**
  and **Second screen** at the top, above the fold. A change applies straight
  away: a running stream restarts and the iPad reconnects by itself. The iPad
  app uses the same names and explains that the choice is made on the PC.
- The iPad looks for PCs as soon as it opens, and tapping a found or recent PC
  connects. Connection problems say what happened and what to do, with
  **Try again** or **Open Settings**: the PC didn't answer (including the
  firewall case), the PC answered but sent no picture, the connection was lost,
  the cable was unplugged, and Local Network access is off. Technical detail
  stays in Connection details and the support log.
- Windows notices for the CPU encoder fallback and a second screen that
  couldn't start use plain words and offer **Use recommended settings**,
  **Try again** or **Download installer**. Encoder, packet size, HEVC and the
  quality limit moved under **Advanced** in Settings.
- Drawing mode is explained in one sentence, and **Reconnect now to apply**
  appears when it changes mid-session.
- Windows shows a banner on every page and one notification when a new
  version is ready. **Update now** downloads the installer, checks it against
  the SHA-256 published with the release, installs it with only the Windows
  permission prompt and a progress window, and restarts EternalMonitor. If an
  iPad is streaming, the update waits until streaming stops, or until you
  click **Stop and update**. If anything fails, the current version keeps
  running and the banner links to the website.
- Windows Settings has **Check for updates automatically** and **Check now**.
  **Later** hides the banner for that version for a day.
- Opening EternalMonitor on the PC while it is already running brings its
  window to the front instead of starting a second copy.
- After **Stop and update** on the PC, the iPad says the PC is updating
  EternalMonitor. With Auto-reconnect on it connects again once the new
  version is running; otherwise, or if the PC takes longer, it says to try
  again once EternalMonitor has opened on the PC.
- The iPad connect screen and Settings say when a newer build is ready in
  TestFlight, with an **Open TestFlight** button.
- When the PC and iPad cannot connect because one is older, both apps say which
  one to update and how.
- Both apps read https://eternalmonitor.dev/latest.json, which is published
  only after Apple approves the matching iPad build. Requests carry no
  identifiers.
- The Windows Stream page says why the picture stopped while Windows keeps
  the screen from EternalMonitor: the lock screen or a permission prompt, a
  display change, another app recording the screen, a disconnected Remote
  Desktop session, or a laptop graphics setting, with the Windows setting to
  change. A second screen that fails because the display driver stopped
  responding asks for a Windows restart; one whose driver is missing links to
  the installer. Each message goes away when the problem ends.
- The iPad says when the typed address is the pairing code, and when nothing
  answers at an address ending in .1, which usually belongs to the router.
- With VoiceOver running, the iPad's stream controls stay on screen until you
  hide them.
- The Windows window no longer closes without a word on PCs whose graphics
  driver crashes while it starts, such as Intel UHD 620 with a 2018 driver.
  The host prefers a fully supported graphics backend on the same GPU, usually
  DirectX 12 where the Vulkan driver is incomplete. If a launch still closes
  before its window appears, the next launch uses another backend.
- When the Windows host cannot start, or stops unexpectedly, it shows a message
  with the error and the path of the log file. The error is written to the log
  and queued for automatic support diagnostics before the process ends. A
  small second EternalMonitor process reports crashes and turns the virtual
  display off if the app itself cannot.
- A USB cable connection now stays up when the PC restarts its stream, for
  example after a settings change or when the extended display turns on. In
  0.3.1 each restart dropped the cable connection, and with the extended
  display selected the iPad could keep reconnecting without ever showing a
  picture.
- When Windows blocks screen capture (lock screen, UAC prompt, display mode
  change, or a laptop GPU it will not capture from), the host keeps retrying
  by itself instead of stopping the stream until someone clicks Start. A
  connected iPad keeps the last picture meanwhile. The retries back off
  instead of running in a tight loop that filled the logs.
- Turning on the extended display no longer triggers repeated stream restarts
  while Windows runs the display task. When that task hangs or is missing, the
  host mirrors the main screen immediately instead of waiting for it each time.
- With no iPad connected, the PC no longer encodes its screen. It reads the
  screen at most once a second, so an iPad that connects still sees the
  current desktop.
- PC audio no longer turns off for the rest of a session after a single large
  sound packet.
- PC audio starts cleanly when the iPad connects, without a short burst of
  stale sound or a gap in the first second.
- The installer also allows display traffic on domain (work or school)
  networks.
- A log line repeated many times in a row is written once, followed by a
  line with the repeat count and the first and last times. The count appears
  about 30 seconds after the burst starts, or when the app quits, so logs and
  support reports keep the lines that explain a problem.
- The host log and support diagnostics keep their final lines when the app
  quits normally, so a normal quit can be told apart from a crash. Quitting
  waits at most two seconds for uploads; the rest are sent on the next launch.

### What to test

On a PC where EternalMonitor 0.3.1 opened a console window that closed after a
few seconds, install this build and open EternalMonitor. The window should
appear. If it does not, a message names the error and the log file; send that
file with the time you opened the app.

Install the previous version on Windows, then click **Update now** when the
banner appears. Approve the Windows prompt and check that EternalMonitor opens
again on the new version. Try it once while streaming. On the iPad, tap
**Open TestFlight** from the banner.

With two monitors, place the second one to the left of the main display in
Windows Display settings. Mirror the main display, then touch each corner and
the centre of the iPad picture with Apple Pencil; the pointer should appear
under the tip each time. Repeat with the extended iPad display placed to the
left of and above the main display, and with display scaling at 125%.

## EternalMonitor v0.3.1

Install the Windows and iPad 0.3.1 updates together. This release includes Intel
hardware-encoding fixes and automatic support diagnostics. The affected Windows
10 / Intel Iris Xe device and physical iPad Pro M5 still need customer testing.

### What changed

- Intel Quick Sync uses its supported NV12 input format for H.264 and HEVC.
  Windows tries the remaining hardware encoders before falling back to CPU
  encoding and records each startup failure.
- AMD keyframe requests survive delayed encoder output, improving recovery when
  the iPad requests a fresh frame.
- The Windows host sends queued audio between video packets, reducing audio
  loss during large video bursts.
- Windows and iPad retain bounded logs across launches, including device and
  build details, encoder or decoder errors, connection history and stream
  statistics. Windows Copy logs includes native FFmpeg errors. On iPad, open
  Settings > Support > Diagnostics to copy or export a report.
- Both apps automatically send support diagnostics while running and retry
  queued events after reconnecting. Reports include device details, PC names
  and network addresses. Screen content, audio and pairing credentials are
  excluded. Automatic sharing can be disabled in Settings.

### What to test

Update both apps, keep the PC and iPad online, then connect over USB with Apple
Devices open on Windows. Stream for two minutes and check whether the hardware
encoder warning returns. Test H.264 first, then HEVC if you use it. If a problem
continues, send the time and time zone, a screenshot of the warning and the
Diagnostics upload status so support can find the reports.

## EternalMonitor v0.3.0

v0.3.0 is the current stable release. Install the Windows app with iPad
v0.3.0, TestFlight build 143. The Windows installer is unchanged from
v0.3.0-rc.1. It passed the automated tests on the reference PC and a physical
iPad; the hands-on checks below remain. New users can follow SETUP.md.

### What changed

- Lost video fragments can be requested again within a bounded repair window.
  The window follows real WiFi timing: it waits for late and reordered packets
  instead of discarding their frames. The quality popover reports repaired
  fragments, dropped frames and jitter.
- A USB connection uses Apple's device service on Windows and the same negotiated
  session as WiFi. The app can take over when a cable connects and reconnect to
  WiFi when it disconnects. USB needs Apple's Apple Devices app from the
  Microsoft Store, open while you stream. When an iPad is plugged in and Apple
  Devices is closed or missing, the host says so and offers to open it or links
  to the Store. Accept Trust This Computer on the iPad.
- PC audio uses 48 kHz stereo Opus. The iPad's audio setting mutes playback without
  disconnecting video. Quiet packets are compact; Opus packet-loss concealment
  covers missing audio. This version does not provide Opus in-band FEC.
- Pair with the six-digit host code or its QR code. Remembered hosts use a token
  stored in the iPad Keychain. Regenerating the host token requires pairing again.
- Hardware and on-screen keyboards, sticky modifiers, indirect pointer buttons
  and hover extend the existing touch and Pencil controls.
- Apple Pencil draws as a Windows Ink pen with pressure and tilt. Drawing mode
  ignores fingers on the canvas, mutes PC audio and asks for the highest USB
  frame rate. In Clip Studio Paint, choose the Tablet PC setting.
- The host offers 30/60/90/120 fps; the iPad requests 30/60/120 fps. The lower limit
  wins. HEVC remains optional. YUV420 remains the default encoder input because
  the AMD encoder does not accept BGRA; BGRA input is opt-in.
- The host shows client, USB and audio status, rotates its session log, and can
  check for updates. The Windows installer configures its firewall rules and VDD
  task permissions. Continue checking these Windows behaviors on your hardware.

### Please exercise on a physical iPad

Pair on the local network, test H.264 and HEVC, then type and scroll through the
keyboard and trackpad. Connect, unplug and reconnect USB. Play audio and check
sync, mute and a Windows output-device change. Walk toward the WiFi edge and
watch repairs rise without long freezes. On a ProMotion iPad, try 120 fps with
the virtual display at 120 Hz. Draw in Clip Studio Paint with Drawing mode over
USB, including pressure, tilt and a resting palm. Report the GPU, codec, host
log and visible error.

On the reference PC a physical iPad streamed the 2732×2048 extended desktop over
USB-C for 30 minutes at 58 FPS with no dropped frames, and 3440×1440 over WiFi
for 30 minutes with 19 of 105,600 frames dropped. First-install trust, timed
takeover and WiFi fallback still need the hands-on pass. Pairing controls access but does
not encrypt video, audio or input, so use a trusted local network.

Known issue: on the reference PC (NVIDIA driver 591.86), Windows once stopped
with an NVIDIA kernel error about two seconds after a USB connection switched a
running mirror session to the extended display. A bounded retest of the same
build and driver repeated that switch five times, and later runs repeated it
again, without a crash. The driver fault is not understood. If it happens,
report the GPU driver version and the time.

## EternalMonitor v0.2.0

A ground-up revamp of the streaming core. Clean break: the v0.2.0 host and
iPad app only work with each other. Each side shows a clear "update the
other half" message if it meets a v0.1.x peer.

### Control the PC from the iPad
- Tap to click, drag to move the mouse, two-finger scroll, half-second hold
  for a right-click, Apple Pencil with pressure. On by default ("Control PC
  with touch" in the iPad Settings) and negotiated per session; the host
  never injects for a session that didn't ask.
- While control is on, a three-finger tap toggles the stats HUD.

### Protocol v2
- A real session: handshake with capability negotiation, busy rejection for a
  second device, instant reconnect takeover, liveness tracking, and clean
  goodbyes (including when the app is backgrounded).
- Host heartbeats, client receiver reports, keyframe requests, and NTP-style
  clock sync. The HUD's latency number is now a real end-to-end measurement.
- Media is raw Annex B in a fixed 32-byte header; FlatBuffers is gone.

### Reliability
- Adaptive bitrate: the host slider is now the ceiling, and the stream steps
  down under loss and back up when the network recovers.
- Keyframe recovery after loss (client-requested, host rate-limited), packet
  pacing on keyframe bursts, and automatic reconnect with backoff after
  "SIGNAL LOST".
- Host supervisor v2: an encoder crash auto-restarts the pipeline in about a
  second and the iPad resumes on the same session, with no reconnect and no
  re-handshake. Wedge watchdogs catch silent stalls.

### Video
- HEVC/H.265 as an experimental opt-in ("Prefer HEVC" on the host):
  negotiated per client, live mid-session codec switching, automatic H.264
  fallback.
- Real capture-time PTS (rate control finally sees true frame cadence), NV12
  decode output with proper BT.601/709 handling, aspect-fit rendering, and
  draw-on-demand (no more free-running 120 Hz redraw).
- The extended (virtual) display now offers the iPad's native resolution
  and refresh rate, and tears down when the client disconnects.

### Quality of life
- Settings apply from the first frame (including headless runs), atomic
  settings writes, mDNS advertisement without the periodic re-registration
  gap plus goodbye packets on exit, live host info (name, resolution, codec,
  bitrate) in the iPad Settings, "Keep screen awake" and "Resume after
  switching apps" toggles, VoiceOver labels, and the Syne display font
  actually rendering.
- The whole stack is now covered by tests: golden wire vectors parsed
  byte-for-byte by both languages, pure-logic suites with injected clocks,
  and end-to-end tests (including a full host→simulator stream in both
  codecs) running in CI on Linux, Windows, and macOS.

---

## EternalMonitor v0.1.2-mirror

Reliability release focused on the AMD encode path and a seamless, on-demand extended display.

### Extended display & virtual-display lifecycle
- **On-demand only:** the bundled virtual display driver now turns on **only once an iPad
  actually connects** and is torn down on exit — so an idle PC never shows a phantom second
  monitor even when "Extended display" is the saved capture target.
- **Crash-safe:** the virtual display is disabled unconditionally at startup and via a panic
  hook, so a crash or force-kill can't strand a phantom monitor.
- **Robust device control:** the installer's enable/disable scheduled tasks now resolve the
  virtual-display device at trigger time (name-agnostic) instead of baking a guessed device id,
  and the installer verifies the tasks registered.
- **Correct output:** on a PC that already has a second monitor, selecting the extended display
  no longer grabs the wrong screen; and the driver is disabled if it fails to attach.
- **Idle keepalive:** a blank/static extended display still delivers a first keyframe, so the
  iPad connects instead of timing out on black.

### AMD / encoding
- AMF now prepends SPS/PPS on forced non-IDR intra frames (not just IDRs), and retries the
  startup keyframe if parameter sets aren't available yet — fewer black-screen/desync cases.
- Configurable virtual-display attach timeout (`ETERNAL_VDD_TIMEOUT_SECS`).

### Diagnostics & networking
- Session log and the AMF packet capture now write to `%APPDATA%\EternalMonitor\{logs,
  diagnostics}` so they work under Program Files (they previously failed silently there).
- Per-frame log spam removed from the hot path.
- Fragment header now carries a per-run stream epoch for instant restart resync; the iPad's
  connect timeout extends once data starts flowing on a slow/jittery network.
- The displayed address / QR fall back to local-adapter enumeration when there's no default
  route, and the GUI shows a banner if the extended display falls back to mirroring.

### Carried forward from the in-development line
- Selectable capture display picker (Settings tab), active-capture-source readouts, the one-step
  Windows installer (`EternalMonitor-Setup.exe`), and GUI/iPad credits.

---

## EternalMonitor v0.1.1-mirror

### What's new
- Multi-vendor GPU support: automatic detection of NVIDIA, AMD, and Intel GPUs via DXGI
- Encoder fallback chain: NVENC → AMF → QSV → libx264 (software)
- Per-encoder optimized low-latency settings
- AMD H.264 hotfix: AMF output is now normalized for VideoToolbox decode, with AMF-only flag guards and stronger bitstream diagnostics
- Startup banner showing detected GPU, encoder, and listen address
- Suppressed mDNS multicast log spam from Tailscale interfaces

---

## EternalMonitor v0.1.0-mirror

First public release. Use your iPad as a wireless second monitor for Windows over local WiFi.

### What works
- Desktop mirroring at up to 60fps
- H.264 hardware encoding via NVENC (NVIDIA GPU required)
- WiFi transport over UDP
- Native Metal rendering on iPad at 120Hz ProMotion
- Auto-discovery via mDNS (or manual IP entry)

### Known limitations
- Mirror only — extended display requires the virtual driver (coming in v0.2.0)
- WiFi only — USB transport coming in v0.2.0
- No touch/input relay yet — coming in v0.3.0
- NVIDIA GPU required for this release — AMD support planned

### Requirements
- Windows 10 or 11 (64-bit)
- NVIDIA RTX GPU
- iPad running iPadOS 16 or later
- Both devices on the same WiFi network

### Installation
Download `EternalMonitor-v0.1.0-mirror-windows.zip`, extract, run `EternalMonitor-host.exe`, open the iPad app, enter the IP shown, tap Connect.
