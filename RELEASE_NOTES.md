## EternalMonitor v0.3.4

Released [date set at release].
Windows and iPad versions listed on the download page work together.
Each 0.3.4 app works with the other app on 0.3.3.

This release fixes the problems reported with 0.3.3 and keeps the stream
going when links, disks or drivers are slow.

### Problems you reported

- EternalMonitor no longer loads AMD's OpenGL driver, which closed it at
  start on PCs with AMD graphics. On Intel graphics it draws with DirectX 12,
  not Intel's Vulkan driver, which crashed some PCs.
- On laptops with two graphics cards and no picture, the PC spots that
  Windows runs it on the wrong card and switches it in one click. If the
  screen moves cards, later connections retry the second screen.
- Over USB on slower PCs, the picture no longer freezes after a big screen
  change. Quality steps down instead.
- After a lost frame on Wi-Fi, the iPad keeps the last clear picture instead
  of smeared blocks or error -12909, and says **Restoring the picture…** if
  that takes over a second.
- If EternalMonitor can't start and a newer version is out, it offers the
  download page. When an update doesn't finish, the PC says why.
- If another program holds the stream port (9876), the Stream page names it.
  Streaming starts once the port is free.
- With the laptop lid closed, the PC says **No display is on** and how to
  keep streaming, and resumes when a screen is back.
- When the PC's address belongs to a VPN or virtual adapter, the PC also
  shows its home network address.

### Settings

- Both apps come in 11 languages, picked in first-run setup or in Settings.
  Buttons, settings and setup are translated; messages and diagnostics stay
  in English.
- A new **Troubleshooting** section replaces settings support had you make by
  hand: graphics card, how the window draws, video encoder and smaller
  packets for VPNs.
- Pairing and input handling are stricter. To let iPads connect over one
  network only, pick it in **Listen on** under Troubleshooting.

### Reliability

- A PC whose second screen can't start is remembered after a restart: it
  mirrors and retries later, or at once after a driver, Windows or app
  update.
- A stuck graphics driver no longer freezes a session. A slow encoder start
  keeps hardware encoding; a stuck one uses the CPU until the driver answers.
- On a slow disk, falling back from the second screen no longer pauses the
  stream.
- After a stall, Apple Pencil strokes no longer lose points.
- On Wi-Fi the iPad waits longer for missing picture data, so a still
  desktop flashes less and short PC stalls end without an extra freeze.
- When iPadOS blocks Local Network access, the iPad offers **Open Settings**
  and says USB works without it.
- PC audio comes back by itself after iPadOS resets its media services.
- The pairing code goes as soon as you type the sixth digit.
- With the on-screen keyboard up, a hardware keyboard no longer drops keys.
- A rare iPad crash when the picture closes is fixed.

### Diagnostics and privacy

- Diagnostics now remove your PC's and iPad's names, Windows user name and
  user folder. Audio devices are sent only by kind, such as headphones.
  **Copy logs** and the iPad's Copy and Export keep the names.
- If you share diagnostics, the iPad app reports crashes, hangs and
  unexpected exits: app and system details only.
- Settings says whether sharing is on and when diagnostics were last sent.

### What to test

Over Wi-Fi far from the router, move windows on the PC: the picture should
never smear and should recover after **Restoring the picture…**. A busy PC
may pause it, but not for long.

Turn off EternalMonitor in iPad Settings > Privacy & Security > Local
Network: the iPad should offer **Open Settings**, and USB should still
connect.

Keep **Automatically share diagnostics** on. If the app crashes or freezes,
send us the **Support ID** from Settings > Support > Diagnostics with the
time. Copy and Export should stay put while the status loads.

## EternalMonitor v0.3.3

Released October 27, 2026. Windows and iPad versions listed on the download page
work together. Each 0.3.3 app still works with the other on 0.3.1 or 0.3.2.

This release fixes **Second screen** on PCs where it didn't start, most often
laptops with Intel and NVIDIA graphics. The iPad then said **Your PC answered,
but no picture arrived**, and over USB the PC's screen blinked and Windows
played its connect and disconnect sounds every few seconds until the cable was
unplugged. Choosing **Mirror my screen** on the PC was the way around it.

### What changed

- The iPad shows the PC's main screen as soon as it connects, while the second
  screen starts. Once Windows adds it, the PC switches to it; if it shows no
  picture within 5 seconds, the iPad goes back to the mirror. Windows now gets
  up to 30 seconds to add it, not 10; if it never does, the mirror stays. Either way the PC says the second screen
  didn't start, and later connections mirror until you click **Try again**,
  restart the stream or change the mode.
- The PC turns the second screen on or off at most once every 10 seconds and
  keeps it on for 10 seconds after the iPad leaves, so a quick reconnect reuses
  it. The screen no longer blinks in a loop.
- The PC tries to start the second screen on the graphics card that drives the
  main screen. Before, the driver chose the card with the most memory, which on
  Intel + NVIDIA laptops drives no screen. This should help those laptops; if
  the second screen still doesn't start, **Mirror my screen** works. The driver
  still chooses when two cards share a name, when the PC can't write the
  driver's settings, or when another app installed the driver and chose a card.
- With **Match the iPad's resolution** on, as it is by default, the second
  screen starts at the iPad's resolution instead of a small, blurry 800×600.
  When the driver doesn't offer that size, it uses the largest one that fits.
- When the second screen doesn't appear, the PC's Stream page says why and what
  to do: run Setup again, restart the PC, or choose Extend with Windows key + P.
  Running Setup again repairs a display driver Setup installed whose device
  was removed or stopped working.
- The first picture arrives sooner on NVIDIA and Intel graphics, especially on
  a still second screen. **Mirror my screen** no longer waits
  for the second screen to turn off at startup.
- After two attempts in a row where the PC answered but sent no picture, the
  iPad stops reconnecting and says to choose **Mirror my screen** on the PC
  (**Main display (mirror)** in 0.3.1). **Try again** starts over. This also
  ends the blinking with PCs still on 0.3.1 or 0.3.2.
- The iPad shows a **Support ID** in Settings > Diagnostics, with **Copy
  Support ID**, and feedback emails include it. PC Settings says **Match the
  iPad's resolution** applies the next time the second screen starts.
- After a failed attempt, the support diagnostics and **Copy logs** hold a
  display snapshot: why the second screen didn't start, and the graphics cards
  and displays Windows reports. The snapshot leaves out the PC's name and the
  user's name; the rest of the log still contains them. Diagnostics name
  Windows 11 correctly.

### What to test

On a laptop with Intel and NVIDIA graphics, choose **Second screen** and connect
the iPad over Wi-Fi, then over USB. The PC's screen should appear at once, then
the second screen a few seconds later, at the iPad's resolution (or the largest
size that fits) while **Match the iPad's resolution** is on. If the second
screen doesn't come up, the iPad should keep the PC's screen with no blinking or
connect sounds, and the PC should say why. Send the PC's **Copy logs** text with
the time; it includes the PC's name.

With the iPad on 0.3.3 and a PC on 0.3.2 whose second screen fails, the iPad
should stop after two tries and say to choose **Mirror my screen**.

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
