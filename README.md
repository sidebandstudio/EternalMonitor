# EternalMonitor

Use your iPad as a second display for Windows, with WiFi or USB and input relay.

[![CI](https://github.com/whoisaldo/EternalMonitor/actions/workflows/ci.yml/badge.svg)](https://github.com/whoisaldo/EternalMonitor/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/whoisaldo/EternalMonitor?labelColor=111&color=e8ff47)](https://github.com/whoisaldo/EternalMonitor/releases/latest)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?style=flat&labelColor=111)](https://eternalmonitor.dev)

![The EternalMonitor Windows app streaming to an iPad Pro](docs/assets/og.png)

A Rust host on the PC captures the desktop with DXGI, encodes on the GPU
(NVENC/AMF/QSV, H.264 or opt-in HEVC), and streams over UDP on the local
network or a framed USB tunnel through Apple's device service. A native Swift
app on the iPad decodes with VideoToolbox and renders
with Metal. Touch, Pencil, pointer and keyboard events travel back to the PC.
PC audio travels to the iPad as Opus. MIT licensed.

Contributions welcome. Transport, encoders, rendering, docs, anything. Ping
`aldobenches285` on Discord to collaborate.

## v0.3.0 candidate

The v0.3.0 work is merged to `main`. A v0.3.0 RC has not been published.
The latest public installer is an older build; it does not contain all the
features described here. The matching candidate installer and TestFlight app
will be linked together when their release gates pass.

- **Mirror or extend.** Capture the primary or another monitor, or create a
  managed virtual display while the iPad is connected.
- **WiFi and USB.** UDP streaming with negotiated fragment repair; a USB cable
  can take over through Apple's device service, with WiFi fallback on unplug.
- **PC audio.** 48 kHz stereo Opus with jitter buffering and packet-loss
  concealment. Mute playback on the iPad without stopping video.
- **Pairing.** Enter the host's six-digit code or scan its token-bearing QR.
  Remembered hosts use a token in the iPad Keychain. Regenerate the host token
  to require pairing again. USB trusts physical access.
- **Touch, keyboard and pointer.** Tap, drag, scroll and hold for right-click;
  hardware and on-screen keyboards, sticky modifiers, secondary/middle buttons
  and hover. Apple Pencil sends native Windows Ink pressure, tilt and supported
  hover. Drawing mode adds a Pencil-only canvas and a USB frame-rate preset.
  See [drawing with Apple Pencil](docs/pencil-drawing.md) for Clip Studio setup.
  iPadOS reserves Globe, ⌘H, ⌘Tab and ⌘Space.
- **Reliability and diagnostics.** Bounded NACK repair, adaptive bitrate up to
  50 Mbps, repaired/lost fragment counts, queue and jitter reports, measured
  latency, reconnect, and supervised pipeline recovery.
- **Codecs and rates.** H.264 by default, HEVC by explicit preference. The host
  offers 30/60/90/120 fps and honors the iPad's lower preference. YUV420 remains
  the default input; BGRA is opt-in until both hardware gates pass.

The full simulator matrix and the 30-minute simulator soak have passed locally.
Native Windows builds and tests have passed, but the reference-PC desktop
campaign on the final build, the 30-minute Windows soak and physical iPad
checks are still pending. See
[HARDWARE_VERIFICATION.md](HARDWARE_VERIFICATION.md) for dated evidence and
remaining checks. No physical USB cable, hardware iPad decoding or 120 Hz panel
result is implied by a simulator pass.

## Install for testing

Use **EternalMonitor-Setup.exe** and the iPad build from the same candidate.
Published candidates appear under
[GitHub prereleases](https://github.com/whoisaldo/EternalMonitor/releases) and
on the site's preview card. TestFlight invite: ask Ali. An unsigned archive is
only a compile check and cannot be installed through TestFlight.

The v0.3 installer sets up the host, bundled
[Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver),
its tasks and TCP/UDP firewall rules. It needs one UAC approval. The installer
is not code-signed; obtain it from the release page and verify its published
SHA-256 before approving a SmartScreen exception. Older installers can still
require a separate firewall prompt.

For WiFi, use a trusted local network; wired Ethernet for the PC helps.
For USB, install the Apple Devices app from the Microsoft Store and keep it
open while you stream; Windows reaches the iPad through it. Use a data cable
and accept Trust This Computer on the iPad. When an iPad is plugged in and
Apple Devices is closed or missing, the host says so and offers to open it or
links to the Store. Pairing controls access but does not encrypt
the stream. The iPad app requires iPadOS 17 or later.

New users should follow the step-by-step [setup guide](SETUP.md). The
installer also adds [QUICKSTART.txt](scripts/QUICKSTART.txt) to the Start menu.
TestFlight setup and the signing workflow are documented in
[FRIENDS_TESTING.md](FRIENDS_TESTING.md).

## Build from source

The workspace contains the Rust host and the pure `eternal-wire` protocol
crate, plus the Swift app in `ios/`. The host's `transport/usbmuxd.rs` module
implements the Apple device-service client.

### Windows host

Requirements: Windows 10 version 1809 or later, Rust 1.98.0 (pinned by `rust-toolchain.toml`, MSVC), an FFmpeg **7.1 shared** SDK, LLVM/libclang
for bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (folder containing bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

`scripts\build-installer.ps1` builds the full Setup.exe (needs Inno Setup and
the same `FFMPEG_DIR`). `scripts\package.ps1` builds the bare zip.

### macOS development loop (no Windows required)

The host builds and runs on macOS with a synthetic capture source. The
protocol, encoder, transport, and supervisor stack all run for real:

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit + golden-vector + synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

If Xcode's command-line tools are the selected developer directory, prefix
Xcode commands with `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

### iPad app

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

Open the generated project in Xcode to run on a physical iPad with your own
signing team.

### Full-system test on one Mac

```bash
./scripts/e2e_ios.sh                 # host (synthetic) → iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh   # same, over HEVC
```

The harness launches the headless host and the simulator app, auto-connects,
and checks decoded frame rate, loss and rendered pixels from the real app.
Run the broader gates with:

```bash
scripts/test_ios.sh         # unit + native UI tests
scripts/e2e_matrix.sh       # UDP, loss/burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800        # 30-minute FPS and host/app RSS check
```

`scripts/e2e_matrix.sh --real` and `scripts/soak.sh --real 1800` target the
reference Windows PC through its interactive runner. Read the hardware runbook
and `scripts/win/README.md` before using those commands.

## How it's tested

- Golden wire vectors (`proto/testdata/`) parsed byte-for-byte by both the
  Rust and Swift codecs, plus fuzz "never crashes" tests on both sides.
- Pure-logic unit tests with injected clocks: session machine, ABR ladder,
  pacer, reassembly, input mapping, gesture state machine, supervisor.
- End-to-end tests that run the real pipeline. The Rust E2E covers
  handshake, lossy ABR step-down, encoder-crash recovery, input relay, and
  HEVC negotiation; the simulator harness above covers the full system.
- CI on every PR: Linux (wire crate), Windows (full host against pinned
  FFmpeg 7.1), macOS (full workspace including E2E), and the iOS simulator
  suite, a separate simulator system job and an unsigned Release archive job.

CI cannot verify real GPU encoders, the virtual display driver, or real
WiFi. [HARDWARE_VERIFICATION.md](HARDWARE_VERIFICATION.md) is the runbook
that covers those before each release.

## Repo layout

```text
host/       Rust host: capture, encode, transport, session, supervisor, egui GUI
proto/      eternal-wire crate: protocol v2 codecs, H.264/HEVC helpers, golden vectors
ios/        Swift iPad app (xcodegen project): receive, decode, render, input relay
installer/  Inno Setup script + bundled-driver staging for EternalMonitor-Setup.exe
scripts/    build-installer.ps1, package.ps1, e2e_ios.sh, QUICKSTART.txt
docs/       eternalmonitor.dev website (GitHub Pages)
```

## Environment variables (host)

| Variable | Effect |
| --- | --- |
| `ETERNAL_ENCODER` | Force an encoder (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1`/`0` overrides the HEVC preference (automation) |
| `ETERNAL_FPS` | Override host FPS; client preference still caps it |
| `ETERNAL_INPUT` | Encoder input: `yuv420` (default), `auto`, or `bgra` |
| `ETERNAL_MAX_DGRAM` | Media datagram size (576–1400 bytes) |
| `ETERNAL_AUDIO` | `synthetic` selects the deterministic audio test source |
| `ETERNAL_USB_DIRECT` | Test-only TCP tunnel endpoint |
| `ETERNAL_REORDER` / `ETERNAL_JITTER_MS` | Test-only first-transmission faults |
| `ETERNAL_CAPTURE` | `synthetic` swaps DXGI for a generated test pattern |
| `ETERNAL_HEADLESS` | `1` runs without the GUI; Ctrl+C shuts down cleanly |
| `ETERNAL_VDD_TIMEOUT_SECS` | Virtual-display attach timeout |
| `ETERNAL_ABR` | `0` disables adaptive bitrate |
| `ETERNAL_DROP` | Test-only: inject fractional datagram loss |
| `ETERNAL_AMF_DIAG` | `1` writes AMF bitstream diagnostics |
| `ETERNAL_LEGACY_PTS` | `1` restores the old frame-counter PTS (escape hatch) |

## Troubleshooting

- iPad can't connect: same WiFi (not a guest network), firewall allowed for
  Private and Public, and manual IP entry beats discovery on tricky
  networks. The host window shows the address and a QR code.
- Choppy video: compare capture/encode/decode FPS, loss, repairs and jitter.
  Try 5 GHz or a wired PC, then check encoder fallback and CPU/GPU load.
- A "Hardware encoder unavailable" banner on the Stream page means the
  hardware encoder failed to open and the host fell back to CPU encoding.
  Update GPU drivers and restart the stream.
- Version mismatch: protocol v2 is a clean break. A v0.1.x app or host
  shows a clear "update the other side" message instead of streaming.

## Reference docs

- [SETUP.md](SETUP.md) is the step-by-step setup guide for users
- [ARCHITECTURE.md](ARCHITECTURE.md) covers the pipeline, protocol v2, and design
- [DECISIONS.md](DECISIONS.md) explains why things are the way they are
- [RELEASE_NOTES.md](RELEASE_NOTES.md)
- [FRIENDS_TESTING.md](FRIENDS_TESTING.md) has organizer notes for beta testing
- [HARDWARE_VERIFICATION.md](HARDWARE_VERIFICATION.md) is the pre-release
  runbook for everything CI can't prove

## Credits

Built by Ali Younes ([@whoisaldo](https://github.com/whoisaldo)).

- Repository: [github.com/whoisaldo/EternalMonitor](https://github.com/whoisaldo/EternalMonitor)
- Questions & concerns: [aliyounes@eternalreverse.com](mailto:aliyounes@eternalreverse.com)

## License

Released under the MIT License. See [LICENSE](LICENSE). © 2026 Ali Younes.
