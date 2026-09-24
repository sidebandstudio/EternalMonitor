## EternalMonitor v0.3.0

Release candidate work is in progress. Use the matching Windows installer and
iPad build when an RC is published. The Windows desktop campaign, long-run
Windows soak and physical iPad verification are still pending. No RC is published yet.

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
- The host offers 30/60/90/120 fps; the iPad requests 30/60/120 fps. The lower limit
  wins. HEVC remains optional. YUV420 remains the default encoder input because
  the AMD encoder does not accept BGRA; BGRA input is opt-in.
- The host shows client, USB and audio status, rotates its session log, and can
  check for updates. The Windows installer configures its firewall rules and VDD
  task permissions. These Windows runtime checks remain part of the RC gate.

### Please exercise on a physical iPad

Pair on the local network, test H.264 and HEVC, then type and scroll through the
keyboard and trackpad. Connect, unplug and reconnect USB. Play audio and check
sync, mute and a Windows output-device change. Walk toward the WiFi edge and
watch repairs rise without long freezes. On a ProMotion iPad, try 120 fps with
the virtual display at 120 Hz. Report the GPU, codec, host log and visible error.

On the reference PC a physical iPad streamed a moving 2732×2048 extended desktop
over USB-C for 643 seconds while charging. First-install trust, timed takeover and
WiFi fallback still need the physical iPad pass. Pairing controls access but does
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
