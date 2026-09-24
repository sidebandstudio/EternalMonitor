# EternalMonitor 0.3.0: What to Test

Release candidate work is in progress. Use the matching Windows installer and
iPad build when an RC is published. The Windows desktop campaign, long-run
Windows soak and physical iPad verification are still pending. No RC is published yet.

### What changed

- Lost video fragments can be requested again within a bounded repair window.
  The quality popover reports repaired fragments, dropped frames and jitter.
- A USB connection uses Apple's device service on Windows and the same negotiated
  session as WiFi. The app can take over when a cable connects and reconnect to
  WiFi when it disconnects. Install Apple Devices or desktop iTunes if prompted
  by the host's USB card, then accept Trust This Computer on the iPad.
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

Known issue under investigation: on the reference PC (NVIDIA driver 591.86),
Windows stopped with an NVIDIA kernel error about two seconds after a USB
connection switched a running mirror session to the extended display. Earlier
extended-display sessions on the same build and driver worked. It must be
understood before a release candidate is published.
