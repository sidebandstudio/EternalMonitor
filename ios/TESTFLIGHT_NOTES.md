# EternalMonitor 0.3.0: What to Test

v0.3.0-rc.1 is the release candidate for testers. Install the Windows preview
and the matching iPad build from TestFlight together. It passed the automated
tests on the reference PC and a physical iPad; the hands-on checks below remain.
New users can follow SETUP.md.

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
  task permissions. These Windows runtime checks remain part of the RC gate.

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
