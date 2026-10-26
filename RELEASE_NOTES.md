## Next Windows release

Not released yet.

- When the second screen doesn't appear, the Windows Stream page says why and
  what to do: the display driver isn't installed (run Setup again), Windows
  couldn't verify the driver's signature, the driver stopped with an error
  (with the Windows code: restart, then run Setup again), or the driver ran but
  Windows added no screen. When Windows adds the screen but keeps the desktop
  off it, the page says to choose Extend with Windows key + P or in Display
  settings, and the next try waits 30 seconds for it. EternalMonitor does not
  change the display layout itself. The display task no longer reports success
  when the display cannot appear. After any failed attempt the support
  diagnostics and **Copy logs** hold a display snapshot: the device state, the
  graphics cards and the display paths Windows reports. The snapshot leaves out
  the PC's name and the user's name; the rest of the log still contains them.
  Before, every case read "no display appeared in time".
- The second screen is drawn by the graphics card that drives the main screen,
  whether or not **Match the iPad's resolution** is on. Before, the driver
  chose the card with the most memory, which on laptops with Intel and NVIDIA
  graphics drives no screen; in 0.3.1 diagnostics none of those laptops got the
  second screen. When two cards have the same name, the driver still chooses.
- The second screen comes up at the iPad's resolution. Windows restores the
  size it last saved for the second screen, and in 0.3.1 diagnostics 4 of the 5
  PCs whose second screen appeared got a tiny, blurry 800×600 one. EternalMonitor
  now sets the second screen's size itself, and only that display's: the
  iPad's size, the sharpest size that fits when the driver doesn't offer it, or
  1920×1080 while the iPad's size isn't known. The log records the size once.
- A PC set to **Mirror my screen** shows the first picture without waiting for
  the virtual display to be turned off at startup. A slow or stuck display task
  held it for up to 19 s in 0.3.1, and about 2 s on a healthy PC.
- Settings says that **Match the iPad's resolution** applies the next time the
  second screen starts, when the iPad connects again. Before, changing it
  seemed to do nothing.
- Running EternalMonitor-Setup.exe again repairs a virtual display driver that
  Setup installed when none of its devices works, for example after the device
  was removed in Device Manager. Working devices are never removed, and a
  driver another app installed is left alone with a message saying so. Before,
  Setup kept the existing driver and stopped with "Windows did not finish
  installing the virtual display driver".
- Support diagnostics name Windows 11 from the build number. Windows 11 keeps
  "Windows 10" in the registry, which earlier versions reported as the system.

## EternalMonitor v0.3.2

Released October 22, 2026. Install the Windows and iPad 0.3.2 updates together;
an iPad on 0.3.1 still works with the Windows changes.

### What changed

- Apple Pencil lands under the tip when another monitor sits left of or above
  the main display, a cancelled stroke resumes instead of dropping, and Pencil
  works as a mouse if Windows cannot create its pen device.
- The iPad can fill its whole screen: choose **Picture: Fill** in Settings or
  tap the arrows next to the settings button. Fill trims the edges of the PC
  picture that don't fit; **Fit** still shows all of it, with bars.
- Plain-language messages in both apps say what happened and what to do, with
  **Try again**, **Search again** or **Open Settings**. The iPad looks for PCs
  as soon as it opens and spots a pairing code or router address typed as the
  PC's address. The PC's Stream page asks **Use the iPad as** (**Mirror my
  screen** or **Second screen**) and says why the picture paused while Windows
  withholds the screen.
- Windows updates itself: **Update now** checks the installer's SHA-256,
  installs it and reopens EternalMonitor, waiting for streaming to stop or for
  **Stop and update**. The iPad says when the PC is updating and when a new
  TestFlight build is ready. Both apps say which one to update when versions
  differ. A popup recommends each new version, and the PC says when the iPad
  app is outdated.
- The PC window opens on graphics drivers that crashed 0.3.1 at startup. If
  the host can't start or stops unexpectedly, it names the error and log file,
  and a helper process reports the crash and tries to turn the second screen
  off. Opening EternalMonitor again brings the running window to the front.
- EternalMonitor no longer opens a black console window next to the app.
- A USB connection stays up when the stream restarts. Capture blocked by the
  lock screen or a display change retries by itself. A hung or missing display
  task no longer stalls the stream. At quit, EternalMonitor tries to turn the
  second screen off even while Windows' display task is slow, and if it can't,
  a helper process tries again.
- PC audio starts cleanly and survives a large packet. With no iPad connected
  the PC stops encoding its screen.
- The installer allows display traffic on work or school networks and names
  the Windows version needed on older Windows. With VoiceOver on, the iPad's
  stream controls stay until you hide them.
- Logs collapse repeated lines and keep their last lines at quit. A Known
  issues page and the setup guide's tables list every message and what to do.
- **Send feedback** in both apps writes an email to the studio with the app
  versions and setup filled in; nothing is sent until you press Send. The iPad
  attaches its Diagnostics report, which you can remove, and problem cards
  offer **Report this problem**. Without an email app, **Copy message** works
  instead. After a few long sessions each app asks once how it's going, and
  the PC sidebar has a card for starring EternalMonitor on GitHub.

### What to test

On a PC where 0.3.1 closed right after opening, install this build: the window
should appear, or a message should name the error and log file. Send that file
with the time.

On a PC with 0.3.1, download the 0.3.2 installer and run it over 0.3.1;
0.3.1's banner only offers **View release**. From 0.3.2 on, **Update now**
installs a later build in place: once one is out, click it, approve the
prompt, and check that the new version opens, once while streaming with
**Stop and update**. On the iPad, tap **Open TestFlight** from the banner.

With a second monitor left of the main display, touch each corner of the iPad
picture with Apple Pencil; the pointer should be under the tip. Repeat with the
extended iPad display and with 125% scaling.

Tap **Send feedback** in Settings on the iPad and click it in the PC's sidebar:
check the email it writes, and tell us what you think.

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
