<div align="center">

**English** · [Español](i18n/README.es.md) · [العربية](i18n/README.ar.md) · [简体中文](i18n/README.zh-Hans.md) · [हिन्दी](i18n/README.hi.md) · [Русский](i18n/README.ru.md) · [日本語](i18n/README.ja.md) · [Português (Brasil)](i18n/README.pt-BR.md) · [Français](i18n/README.fr.md) · [Deutsch](i18n/README.de.md) · [한국어](i18n/README.ko.md)

<img src="docs/assets/icon.png" width="88" height="88" alt="EternalMonitor logo">

# EternalMonitor

A free, open-source second monitor for Windows.<br>
Connect your iPad over Wi-Fi or USB, with touch and Apple Pencil support.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Download](#download) · [Setup guide](SETUP.md) · [Website](https://eternalmonitor.dev) · [Roadmap](ROADMAP.md) · [Release notes](RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="docs/assets/readme-demo.webp" width="860" alt="A Windows PC and an iPad. On the PC, the EternalMonitor window switches the iPad to Second screen, and a window is dragged off the monitor onto the iPad. Then an Apple Pencil draws blades of grass on a painting on the iPad, the strokes thick where it presses and thin where it lifts.">
</p>

EternalMonitor turns an iPad into a monitor for your Windows PC. It is free, open
source under the MIT license, and needs no account. The PC streams at up to
120 fps, with hardware encoding on supported graphics cards and hardware decoding
on the iPad, and every tap, keystroke and Pencil stroke goes back to the PC.

<a id="download"></a>

## Quick start

[![Download for Windows](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![Get the iPad app on TestFlight](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **Install the Windows app.** Run [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
   on Windows 10 (version 1809 or later) or Windows 11, 64-bit, with NVIDIA, AMD
   or Intel graphics. If Windows SmartScreen asks, choose **More info**, then
   **Run anyway**. The [release page](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3)
   lists the file's SHA-256.
2. **Install the iPad app.** On an iPad with iPadOS 17 or later, install
   **TestFlight** from the App Store, then open the
   [EternalMonitor TestFlight link](https://testflight.apple.com/join/ppc9QfXc)
   on the iPad and tap **Install**.
3. **Connect.** With both apps open on the same network, tap your PC on the iPad
   or scan the QR code the PC shows. For USB, on Windows 10 22H2 or Windows 11,
   plug in the cable and keep Apple's free
   [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) app open on the PC.

The [setup guide](SETUP.md) walks through every step with the exact button names.
The [download page](https://eternalmonitor.dev/download.html) lists the Windows and
iPad versions that go together.

If EternalMonitor is useful to you, a star helps other people find it.

## Features

<img src="docs/assets/ipad-app.webp" align="right" width="270" alt="The iPad app's connect screen with Connect, Scan QR code and Find PCs">

- **Extend or mirror.** Add the iPad as a second display beside your monitor, or
  mirror any screen you have. The second display stays for 10 seconds after the
  iPad disconnects, so a quick reconnect reuses it and Windows never keeps a
  phantom monitor around.
- **Wi-Fi or USB.** Stream over your home network, or plug in a cable for a
  steadier picture that can also charge the iPad from a USB-C port. A cable takes over a running
  Wi-Fi session, and unplugging it falls back to Wi-Fi.
- **Apple Pencil as a real pen.** Windows receives Windows Ink pressure and tilt,
  and the pen lands under the tip on any monitor layout. Drawing mode ignores your
  palm on the canvas, and Clip Studio Paint works with its Tablet PC setting.
- **Touch, keyboard and pointer.** Tap to click, drag, scroll with two fingers and
  hold to right-click. Hardware keyboards, trackpads and mice work too, with
  sticky modifiers and middle-click.
- **Hardware encoding.** Supported NVIDIA, AMD and Intel graphics encode the
  stream at 30, 60, 90 or 120 fps.
- **Loss repair on Wi-Fi.** When a packet goes missing, the iPad asks for it again
  right away, so one dropped packet rarely costs a frame. The bitrate follows your
  network.
- **PC audio.** Sound plays on the iPad, and you can mute it without stopping the
  video.
- **Fit or fill.** Fit shows the whole PC picture, with bars where the shapes
  differ. Fill uses the iPad's entire screen and trims the edges that don't fit.
- **Pairing.** The first Wi-Fi connection asks for a six-digit code or a QR scan.
  After that the iPad remembers the PC. USB needs no code.
- **Recovers and explains.** The picture comes back by itself after the lock
  screen, a display change or a stream restart. Both apps say what happened and
  what to do, and **Send feedback** writes an email to the studio with your setup
  filled in.
- **Updates in one click.** A popup tells you when a new version is out. On
  Windows, **Update now** checks the download and installs it with one Windows
  prompt; on the iPad, it opens TestFlight. The Windows app runs without a
  console window.

This README and the [website](https://eternalmonitor.dev) are available in 11 languages.

<br clear="right">

## Learn more

- [Use an iPad as a second monitor for Windows](https://eternalmonitor.dev/ipad-second-monitor-windows.html): the full guide.
- [Compatibility](https://eternalmonitor.dev/compatibility.html): PCs and iPads people run EternalMonitor on.
- [Alternatives](https://eternalmonitor.dev/alternatives.html): how EternalMonitor compares with other ways to use an iPad as a screen.
- [Roadmap](ROADMAP.md): what is coming next.

**Share your setup.** Tell us which PC and iPad you use with the
[compatibility report](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml),
or post it on the [Sideband Discord](https://discord.gg/Xc7NykBZ6z). With your
permission, it goes on the compatibility page.

## Status

The current release is v0.3.3 for Windows, with v0.3.3 (build 364) for the iPad.
It brings **Second screen** to more laptops with Intel and NVIDIA graphics: the
iPad shows the PC's main screen right away, then switches to the second screen
once Windows adds it. [RELEASE_NOTES.md](RELEASE_NOTES.md) covers every version,
and [ROADMAP.md](ROADMAP.md) what comes next.

The v0.3.3 release checks passed on Windows, macOS and iPad simulators, and
[docs/hardware-verification.md](docs/hardware-verification.md) records hands-on
results on a real PC and iPad from v0.3.0.

Both apps share diagnostics so problems can be found and fixed: device details,
app errors and stream statistics, including PC and iPad names, audio device names
and network addresses. Diagnostics never include screen content, audio or pairing
credentials. Turn sharing off in Settings. [Automatic diagnostics](SETUP.md#automatic-diagnostics)
and the [privacy notice](https://eternalmonitor.dev/privacy.html) list exactly what
is sent and how long it is kept. On iPad, open Settings > Support > Diagnostics to
copy or export a report.

## How it works

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/readme/how-it-works-dark.svg">
  <img src="docs/assets/readme/how-it-works-light.svg" width="1120" alt="The Windows PC captures the desktop, encodes the video on the GPU and sends it over Wi-Fi or USB. The iPad reassembles the video, asking for missing fragments on Wi-Fi, decodes it with VideoToolbox and shows it with Metal. PC audio is captured with WASAPI, encoded as Opus and played on the iPad. Touch, Pencil, keyboard and trackpad input travels back to the PC, which injects it.">
</picture>

The Windows host is written in Rust. It captures the desktop with DXGI, encodes
through FFmpeg with NVIDIA NVENC, AMD AMF or Intel Quick Sync on supported
graphics hardware (H.264 by default, or HEVC), and sends each frame as numbered UDP fragments. Over USB
the same messages travel through a framed tunnel that Apple's device service
provides. The iPad app is native Swift. It reassembles fragments, decodes with
VideoToolbox and renders with Metal. On Wi-Fi it also requests any fragments
that are missing. PC audio is captured with WASAPI and sent as Opus; the iPad
plays it through a jitter buffer and conceals lost audio instead of requesting it
again. Input and receiver reports go back on the same session, and the host uses
those reports to adapt the bitrate between 4 and 50 Mbps.

[ARCHITECTURE.md](ARCHITECTURE.md) describes the pipeline and protocol v2, and
[docs/decisions.md](docs/decisions.md) explains why each part works the way it
does.

## Build from source

The workspace holds the Rust host (`host/`), the protocol crate `eternal-wire`
(`proto/`) and the Swift iPad app (`ios/`).

<details>
<summary><b>Windows host</b></summary>

Requirements: Windows 10 version 1809 or later, Rust 1.98.0 (pinned by
`rust-toolchain.toml`, MSVC), an FFmpeg **7.1 shared** SDK and LLVM/libclang for
bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

The host is a Windows GUI program, so it opens no console window. Started
from a terminal it prints its log there, but the prompt returns at once;
`cargo run -p eternal-host` waits for it. With `ETERNAL_HEADLESS=1` and output
redirected (`> host.log`) it gets its own console, so Ctrl+C in the terminal
can't reach it: press Ctrl+C in that console's window, or use Task Manager.
`scripts\win\Stop-Host.ps1` stops the hosts that `remote.sh run-host` started and
leaves any others running.

`scripts\build-installer.ps1` builds the full Setup.exe with Inno Setup and the
same `FFMPEG_DIR`. `scripts\package.ps1` builds a bare zip.

</details>

<details>
<summary><b>macOS development loop</b> (no Windows needed)</summary>

On macOS the host swaps screen capture for a synthetic test pattern. The
protocol, encoder, transport and supervisor all run for real.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

If Xcode's command-line tools are the selected developer directory, prefix Xcode
commands with `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

</details>

<details>
<summary><b>iPad app</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

To run on a physical iPad, open the generated project in Xcode and pick your own
signing team.

</details>

<details>
<summary><b>End-to-end tests</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

Each row checks the decoded frame rate (at least 55 fps), drops, repairs and the
rendered pixels. `e2e_matrix.sh --real` runs the Windows hardware rows through the
runner in `scripts/win/`; read [scripts/win/README.md](scripts/win/README.md)
first. CI runs the Rust suites on Linux, macOS and Windows, the iPad tests and a
streaming system test on every pull request.

</details>

<details>
<summary><b>Host environment variables</b></summary>

| Variable | Effect |
| --- | --- |
| `ETERNAL_ENCODER` | Force an encoder (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` or `0` overrides the HEVC preference |
| `ETERNAL_FPS` | Override the host frame rate; the iPad's preference still caps it |
| `ETERNAL_INPUT` | Encoder input: `yuv420` (default), `auto` or `bgra` |
| `ETERNAL_MAX_DGRAM` | Media datagram size, 576 to 1400 bytes |
| `ETERNAL_CAPTURE` | `synthetic` swaps DXGI for a generated test pattern |
| `ETERNAL_AUDIO` | `synthetic` selects the deterministic audio test source |
| `ETERNAL_HEADLESS` | `1` runs without the window; Ctrl+C shuts down cleanly |
| `ETERNAL_ABR` | `0` disables adaptive bitrate |
| `ETERNAL_VDD_TIMEOUT_SECS` | How long to wait for the virtual display to attach (default 30) |
| `ETERNAL_AMF_DIAG` | `1` writes AMF bitstream diagnostics |
| `ETERNAL_LEGACY_PTS` | `1` restores the old frame-counter timestamps |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | Test only: inject loss, reordering or jitter |
| `ETERNAL_USB_DIRECT` | Test only: a TCP endpoint in place of the USB tunnel |

</details>

## Documentation

| Document | What it covers |
| --- | --- |
| [SETUP.md](SETUP.md) | Step-by-step setup for anyone, including USB and drawing |
| [Known issues](https://eternalmonitor.dev/known-issues.html) | Problems we know about, and what to do about each (`docs/known-issues.html`) |
| [Privacy](https://eternalmonitor.dev/privacy.html) | What the apps send, why, how long it is kept and how to turn it off (`docs/privacy.html`) |
| [ARCHITECTURE.md](ARCHITECTURE.md) | The pipeline, protocol v2 and each component |
| [docs/decisions.md](docs/decisions.md) | Why things are the way they are, with measurements |
| [docs/pencil-drawing.md](docs/pencil-drawing.md) | Apple Pencil, Windows Ink and Clip Studio Paint |
| [docs/pairing.md](docs/pairing.md), [docs/keyboard-input.md](docs/keyboard-input.md), [docs/host-settings.md](docs/host-settings.md), [docs/audio-codec.md](docs/audio-codec.md) | Feature details |
| [docs/hardware-verification.md](docs/hardware-verification.md) | The release test record and the hands-on checklist |
| [Release testing](docs/beta-testing.md) | Shipping a build through TestFlight: signing and what to test |
| [docs/design.md](docs/design.md) | The shared look of the apps and the website |
| [RELEASE_NOTES.md](RELEASE_NOTES.md) | What changed in each version |
| [ROADMAP.md](ROADMAP.md) | What is coming next |

## Contributing

Contributions are welcome, from transport and encoders to rendering and docs.
Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request; it lists
good first contributions, and [ROADMAP.md](ROADMAP.md) shows the larger work ahead. To
talk an idea through first, ask on the [Sideband Discord](https://discord.gg/Xc7NykBZ6z).

## Contact

The [Sideband Discord](https://discord.gg/Xc7NykBZ6z) is the place for questions,
setup help and ideas. You can also email
[hello@sideband.studio](mailto:hello@sideband.studio).

Found a bug? [Open an issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)
with both app versions and the host log, or use the
[feedback form on the download page](https://eternalmonitor.dev/download.html#feedback),
which emails the studio. Report security problems privately as
described in [SECURITY.md](SECURITY.md), not on Discord or in an issue.

## License

EternalMonitor is released under the [MIT License](LICENSE), © 2026 Sideband Studio.
The Windows installer also ships third-party components under their own licenses,
including the [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)
and FFmpeg.

Built by [Sideband Studio](https://sideband.studio).
