# Beta testing (organizer notes)

Notes for coordinating a beta test across mixed hardware. Testers follow
`SETUP.md`, and the installer adds the shorter `scripts/QUICKSTART.txt` to the
Start menu. This file is for you, the person handing the build out.

## Hand out the installer, not the zip

Build `EternalMonitor-Setup.exe` with `scripts/build-installer.ps1` and send that single
file. The tester double-clicks it, approves one Windows (UAC) prompt, and gets the host,
FFmpeg, and the virtual display driver installed in one run — no unzip, no manual driver
steps. To bundle the driver, drop the signed setup into `installer/vendor/vdd/` first (see
that folder's `README.txt`); without it the build still works but produces an app-only
installer and the extended-display option won't appear.

## Getting a tester onto the iPad app (TestFlight)

A tester needs no Xcode and no developer account. They need the TestFlight
app from the App Store and a link from you.

### Automated archive and upload

The `TestFlight` workflow runs on every pull request without signing credentials.
It also accepts `workflow_dispatch` and `v*` tags. Missing secrets produce an
unsigned Release archive as a compile check. This archive cannot be installed
through TestFlight. A signed workflow exports directly to App Store Connect.

Every workflow run uses build number `100 + GITHUB_RUN_NUMBER`; Xcode's automatic
build-number rewriting is disabled. A tag such as `v0.3.0-rc.1` must match
`MARKETING_VERSION: "0.3.0"`. To upload again, start a new workflow dispatch rather
than rerunning an already uploaded build number.

Add these repository Actions secrets in GitHub Settings:

| Secret | Value |
| --- | --- |
| `ASC_KEY_ID` | App Store Connect team API key ID |
| `ASC_ISSUER_ID` | The team's issuer UUID |
| `ASC_KEY_P8` | Base64 of the complete `.p8` private key |

Use an App Manager or Admin team API key with access to this app and signing.
Automatic signing first uses Xcode's cloud-managed distribution path. If the
team refuses cloud signing, add `IOS_DIST_P12_BASE64` and
`IOS_DIST_P12_PASSWORD` for an exported Apple Distribution certificate and its
private key. The script imports that fallback into a temporary keychain and
restores the original search list before removing it on exit. The API key is
still needed for provisioning and upload. [Apple's cloud signing guide](https://developer.apple.com/help/account/certificates/cloud-managed-certificates/)
and [GitHub's certificate guide](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications)
describe the two paths.

Run `gh workflow run testflight.yml --ref main` after the workflow reaches main.
The workflow summary states `dry` or `upload`, the version and build number.
Its `EternalMonitor-iPad-<build>` artifact contains the archive, logs and test notes.

### Local equivalent on devbox1

The installed Xcode 16.4 can prove the unsigned Release archive compiles:

```bash
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  BUILD=106 scripts/testflight-local.sh --dry-run
```

App Store Connect has required Xcode 26 and the iOS 26 SDK since April 28, 2026.
The hosted workflow selects Xcode 26.3. A local upload requires a separately
installed supported Xcode and an explicit `DEVELOPER_DIR`; the simulator suite
uses Xcode 16.4 and iOS 18.6. [Apple's SDK requirements](https://developer.apple.com/news/upcoming-requirements/)

Place the key at `~/private_keys/AuthKey_<ID>.p8`, then select an unused build
number above the last uploaded build:

```bash
DEVELOPER_DIR=/Applications/Xcode_26.3.app/Contents/Developer \
  ASC_KEY_ID=<ID> ASC_ISSUER_ID=<issuer-uuid> BUILD=<unused-number> \
  scripts/testflight-local.sh
```

The wrapper generates the Xcode project and runs these two operations with the
selected key. The export plist's `destination: upload` performs the upload:

```bash
xcodebuild -project ios/EternalMonitor.xcodeproj -scheme EternalMonitor \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath build/testflight/EternalMonitor.xcarchive CURRENT_PROJECT_VERSION="$BUILD" \
  -allowProvisioningUpdates -allowProvisioningDeviceRegistration \
  -authenticationKeyPath "$HOME/private_keys/AuthKey_${ASC_KEY_ID}.p8" \
  -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID" archive
xcodebuild -exportArchive -archivePath build/testflight/EternalMonitor.xcarchive \
  -exportOptionsPlist ios/exportOptions.plist -exportPath build/testflight/export \
  -allowProvisioningUpdates -authenticationKeyPath "$HOME/private_keys/AuthKey_${ASC_KEY_ID}.p8" \
  -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID"
```

Prefix these commands with the selected `DEVELOPER_DIR` or export it in that
shell. Neither path switches the developer account used by this workspace.

### Then, in App Store Connect

1. TestFlight tab, wait for the build to finish processing (usually a few
   minutes; you get an email).
2. Check the build's export-compliance status. The app declares
   `ITSAppUsesNonExemptEncryption` as false for the current unencrypted stream.
3. Create an external testing group, add the build, and fill in "What to
   Test" from `ios/TESTFLIGHT_NOTES.md`. Regenerate that file with
   `python3 scripts/testflight_notes.py` after changing the current release notes.
4. Submit for Beta App Review. The first build for external testers is
   reviewed by Apple; wait for its result before sharing the external invite.
5. Once approved, enable the group's public link and send that to your
   tester. Anyone with the link can install; you can cap the number of
   testers on the same screen. Paste the resulting `https://testflight.apple.com/join/...`
   link into `docs/script.js` as `TESTFLIGHT_URL`. Until then the site says
   "TestFlight invite: ask Ali".

Internal testers skip review entirely and get builds immediately, but they
must be users on your App Store Connect team, so that route only makes sense
for people you want inside the developer account.

### What to send the tester

Two links, and they must match:

- The Windows installer for the same version. While a build is still in
  testing it is published as a GitHub pre-release and appears on
  eternalmonitor.dev/download.html under "Preview build for testers",
  marked as a test build.
- The TestFlight public link for the matching iPad build.

Use the matching candidate pair. v0.1 and v2 builds cannot stream together.
v0.3 keeps the v2 prefix and negotiates new features, but mixing candidate and
older builds is not a supported beta-test configuration.

## Extended display vs mirror

By default the iPad mirrors the primary screen. To test the iPad as a real extended desktop:
**connect the iPad first**, then on the host's Stream page set **Display** to **"Extended display
(iPad)"** (also under Settings → Display to stream) and click **Restart now**. Dragging a window past the edge of the main screen should
land it on the iPad. The virtual monitor is created **on demand and only while the iPad is
connected** — there is intentionally no second display when idle, so don't expect to see it in
Windows Display settings before connecting. If the extended display can't start, the host shows an
amber "Extended display unavailable" banner and mirrors the primary screen — re-run the installer
so its display task is registered.

## Build parity and candidate status

Hand out the Windows installer and iPad app from the same candidate, and record
both versions and the TestFlight build number. v0.3.0-rc.1 passed the automated
gates on the reference PC and a physical iPad on 2026-09-24; see
`docs/hardware-verification.md`. The checks there that need a person are what testers
should focus on. Do not present an unsigned archive or older release as the
finished v0.3 product. Point new testers to `SETUP.md`.

## What to test in v0.3

Use `docs/hardware-verification.md` for the complete checklist and expected results.

- Pair with a wrong code, then the correct code; reconnect from Keychain, scan
  the token QR, regenerate the token, and exercise the failed-code cooldown.
- Verify H.264 and opt-in HEVC hardware decode on the iPad. Check the host's
  actual encoder and software-fallback banner.
- Connect by WiFi, attach USB and accept Trust, then unplug/replug. Record the
  USB card and link badge throughout. The cable and Apple device-service path
  has not been proven by the loopback tunnel tests.
- Play PC audio, change the default Windows output, mute on the iPad, and
  compare audio/video timing. Report endpoint, buffer and loss statistics.
- Type through both keyboards; test Ctrl shortcuts, sticky modifiers, arrows,
  pointer buttons/scroll, Pencil position/hover and touch in every corner.
  Test Apple Pencil pressure and tilt in a Windows Ink app; see
  `docs/pencil-drawing.md`.
- Background/resume, kill/restart the host, and move toward poor WiFi coverage.
  Watch repaired fragments separately from unrecovered drops and latency.
- Try the extended display at 120 Hz on a ProMotion iPad; record the achieved
  decode rate rather than assuming the requested rate was reached.

## USB tester setup

Install the Apple Devices app from the Microsoft Store and keep it open while
streaming; its device service runs only while the app runs. Use a data cable,
unlock the iPad and accept Trust This Computer. Keep the EternalMonitor app
open. The host shows a notice when an iPad is plugged in and Apple Devices is
closed or missing. On Windows the service listens at 127.0.0.1:27015; the app
listens on iPad loopback port 9877. Do not open that iPad port to the LAN. If
the iPad does not appear after Apple Devices opens, unplug and replug the
cable, and record the host's USB card text.

## GPU coverage

Test NVIDIA NVENC, AMD AMF and Intel QSV separately. The reference PC has NVENC
and AMF; QSV needs another tester. Keep H.264 and YUV420P as defaults. The BGRA
option needs both color comparison and ten-minute NVENC/AMF runs before its
baseline can change. AMF normalization, startup-IDR retries and forced-intra
handling remain in place.

An amber software-fallback banner means a hardware encoder failed to open.
Capture that error before changing drivers or settings. A fallback that produces
video is not proof that the requested hardware encoder worked.

## Evidence to collect

1. GPU, active codec/input format, capture display/resolution, both versions,
   iPad model/iPadOS, WiFi/USB and requested/effective FPS.
2. Copy logs from the host's Performance page, or collect
   `%APPDATA%\EternalMonitor\logs\eternal-host-session.log` and `.1`/`.2`.
3. App diagnostics and a recording or screenshot of the visible failure.
4. For an AMF H.264 investigation, explicitly enable `ETERNAL_AMF_DIAG=1` before
   launch and retain `%APPDATA%\EternalMonitor\diagnostics\amf-first-120-packets.h264`
   plus its validation log. Capture is opt-in; ordinary runs do not create it.

Pairing tokens and token-bearing QR codes grant access. Redact them before
posting evidence publicly. Keep screenshots of a tester's desktop private.

## Troubleshooting and known limits

The v0.3 installer creates TCP and UDP firewall rules. For an older install or
another security product, check the host's allowed-app rule and network profile.
Guest networks and subnet boundaries can block discovery; try a reachable manual
LAN address. Use the packet/repair, encoder and decoder diagnostics to distinguish
network trouble from resource pressure or a codec problem.

Video NACK repair, USB framing and audio are implemented. They do not replace the
pending hardware campaign. FEC and encryption remain deferred; pairing is access
control on a trusted network, not confidentiality. HEVC and BGRA remain opt-in.
Neither the simulator's software decoder nor its refresh rate proves hardware
decoding or 120 Hz on a physical iPad.
