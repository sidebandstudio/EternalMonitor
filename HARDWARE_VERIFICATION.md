# v0.3.0 hardware verification

Status as of 2026-09-23: no release candidate has been published. The reference
PC has installer `0975ad8`, including the USB capture/logging fixes and verified
VDD binding checks. Extended desktop and charging passed together through the
rear USB-C port and a USB-C-to-USB-C data cable. Later, Windows crashed during
virtual-display activation; GPU, display and installer tests are suspended.
The full campaign also has failing UDP reliability rows, and final validation
on `main` remains pending.

The results below belong to the stated phase revisions. They do not establish
that a later candidate passed. Before release, run both complete matrices and
both 30-minute soaks on `main`, then repeat R-baseline with the published
installer. Keep failures with their original evidence.

## Verified by the automated campaign on the reference PC

Reference hardware: Windows 11, Ryzen 7 7800X3D with Radeon integrated graphics,
GeForce RTX 5080. The primary display was 3440×1440 during the desktop campaign.
The attached iPad ran app 0.3.0 build 6 on iPadOS 26.2 at 2732×2048.
All paths below are relative to the private `EternalMonitor-Handoff` directory.
Desktop images, pairing data and diagnostic captures must stay outside git and
public PRs. All recorded runs in these tables are dated 2026-09-23.

| Check | Recorded result | Evidence |
| --- | --- | --- |
| Native build, lint and tests | Release build, strict clippy and 191 Windows Rust tests passed for installer `0975ad8` | `evidence/windows/installer-0975ad8-verified-build.log`, `evidence/windows-native-current-repair.log` |
| Physical USB extended desktop under motion | `7532347`: 642.93 seconds, 57.16 assembled FPS, zero drops, no capture watchdog or restart storm | `evidence/windows/usbc-7532347-motion-summary.json` |
| Installed host after driver restoration | `0975ad8`: 113.49 seconds, 57.17 assembled FPS, zero drops; direct iPad screenshot passed pixel checks | `evidence/windows/usbc-restored-0975ad8-motion-summary.json`, `evidence/windows/ipad-usbc-restored-motion-0975ad8.png` |
| Charging while displaying the extended desktop | Rear USB-C with C-to-C data cable reports a 15 W, 3 A source. Battery rose from 9% to 10% during the ten-minute motion run, with positive battery current. At 17:34 UTC the idle desktop remained connected and battery was 25%, charging at +2270 mA | `evidence/windows/ipad-battery-usbc-motion-*.json`, `evidence/windows/ipad-battery-usbc-1738.json` |
| Interactive task execution | Capture runs in console session 1 with a Limited user token and normal scheduling priorities. New completed Limited test tasks remove their own registrations | `evidence/windows/new-task-priorities.log`, `evidence/windows/limited-task-cleanup-after-2.log` |
| Installer fresh install, owned uninstall and restoration | All three stages passed at `0975ad8` without a reboot. Existing user settings and original vendor XML were preserved. The final installation retains the pre-existing driver's external ownership | `evidence/windows/owned-driver-fresh-0975ad8.log`, `evidence/windows/owned-driver-uninstall-0975ad8.log`, `evidence/windows/owned-driver-restore-0975ad8.log` |

The physical USB stability check followed a logging fix. Synchronous redirected
output could stop streaming when its destination stalled. Separate bounded
output workers keep capture and transport moving. The original failure,
disk-reset correlation and before/after unread-pipe reproduction remain in
`evidence/windows/usb-stalls/` and `evidence/async-logging-7f135a3/`.
The evidence does not establish a hardware cause for the disk resets.

At 19:42:10 UTC, Windows crashed with `SYSTEM_SERVICE_EXCEPTION (0x3B)` and an
access violation in NVIDIA `nvlddmkm.sys` (driver 591.86), in `WUDFHost.exe`.
WER identifies `CHwContext::uninitialize`. The installed host's USB connection
arrived at 19:42:08.514 and triggered virtual-display activation; no completed
enable operation appears before the crash. This is strong timing evidence that
the display activation triggered the crash. The exact driver defect and a
permanent fix remain unverified. Earlier restarts at 18:02–18:04 UTC were
planned Windows Update operations. The new `4f0c706` installer had not been
applied. Evidence: `evidence/windows/restarts/summary.json` and
`evidence/windows/restarts/windbg-analysis.log`. Keep raw dumps and logs private.

A fresh driver installation initially returned success while Windows had left
the device unbound. Installer `0975ad8` now verifies the present device's bound
INF, version and problem code before recording ownership or allowing launch.
The actual Inno failure fixture exits 1; its bound-device fixture exits 0.
See `evidence/windows/vdd-installer-exit-check.log`. The signed vendor driver
required an interactive Windows publisher approval during this machine's fresh
installation. Unattended publisher approval on a clean PC remains unverified.

The pinned upstream asset is tagged `25.5.2`, its package reports `25.05.03`,
and its bound driver version is `23.40.36.27`. These are different version
fields. The final binding is `oem33.inf`; the uninstall/restore tests verified
actual device binding, task permissions, product files and firewall rules.

The core Windows matrix has these recorded results. FPS values describe the
measured interval after startup. A row's retained `result.json`, logs and pixel
checks are authoritative.

| Row | Result and limits | Evidence directory under `evidence/` |
| --- | --- | --- |
| R-baseline | Passed on merged P0 `0965566`; 1,140 decoded frames at 3440×1440, final 59 FPS, quadrant pixels passed | `windows-p0-0965566/real/R-baseline/` |
| R-nvenc-h264 | Passed: 58.54 FPS over 20.50 s, one drop | `windows-campaign-47909ed/real/R-nvenc-h264/` |
| R-nvenc-hevc | Passed: 58.59 FPS over 20.48 s, zero drops | `windows-campaign-47909ed/real/R-nvenc-hevc/` |
| R-amf-h264 | Passed: 58.66 FPS over 20.46 s, zero drops; SDK bitstream validation passed | `windows-campaign-47909ed/real/R-amf-h264/` |
| R-amf-hevc | Passed: 58.38 FPS over 20.56 s, zero drops; SDK bitstream validation passed | `windows-campaign-3022d87/real/R-amf-hevc/` |
| R-nvenc-h264-loss3 | Failing: recovery exceeds the limit of one keyframe request per ten seconds; some runs also exceed 2% drops. The 25 ms repair budget is unchanged | Latest retained `windows-campaign-*/real/R-nvenc-h264-loss3/` |
| R-nvenc-burst | Failing reliability gate; a complete passing run is required | Latest retained `windows-campaign-*/real/R-nvenc-burst/` |
| R-audio | Passed: 58.20 FPS, 2,155 audio packets, zero audio loss; WASAPI tone, decoded audio and silence assertions passed | `windows-campaign-01474bb/real/R-audio/` |
| R-pairing | Passed: wrong code rejected, correct code accepted and Keychain token reconnect verified | `windows-campaign-b9f4514/real/R-pairing/` |
| R-input | Passed: probe receives mapped clicks within one pixel and the required keyboard/pointer events | `windows-campaign-01474bb/real/R-input/` |
| R-vdd | Latest installed `0975ad8` run failed overall: three keyframe requests in 20.89 s. Mode selection, attach, disconnect, reattach, active-host exit and connected pixels passed. 57.46 FPS, three dropped frames | `windows-vdd-0975ad8-measurement/real/R-vdd/` |
| R-reconnect | Passed: video resumed in 10.30 s after the tracked host restarted | `windows-campaign-01474bb/real/R-reconnect/` |
| R-gui | Passed: connected Stream, client/audio/USB/pairing cards, Settings and QR views inspected | `windows-campaign-01474bb/real/R-gui/` |

Additional gates:

| Row | Result and limits | Evidence under `evidence/` |
| --- | --- | --- |
| R-usb-service | Native fake-server tests and physical cable streaming passed. The existing Apple device service works; first-install trust flow and timed cable takeover/fallback remain open | `windows/usbc-restored-0975ad8-host.log` |
| R-bgra-nvenc | Passed: 601.35 s, 58.27 FPS; quadrant mean-channel errors 0, 0, 0.33 and 1, below 12/255 | `windows-colors-a0823cc/real/R-bgra-nvenc/` |
| R-bgra-amf | Unsupported by this Radeon encoder. Forced BGRA falls back to software; the hardware BGRA gate does not pass. Shipping default stays YUV420P | `windows-colors-a0823cc/real/R-bgra-amf/` |
| R-vdd-limited | Passed on the final installation: the normal-user host invokes the SYSTEM tasks and attaches the physical iPad's extended display | `windows/owned-driver-restore-0975ad8.log`, `windows/usbc-restored-0975ad8-host.log` |
| R-fps120 | Passed requested/negotiated 120 FPS and error checks. Actual simulator decode was 58.06 FPS; this does not prove ProMotion | `windows-campaign-755741b/real/R-fps120/` |
| R-headless-ctrlc | Passed tracked host exit. The runner now records command completion before transcript flushing | `windows/headless-stop-trace.log` |
| R-autostart | Passed quoted HKCU value write/remove; prior absent value restored | `windows-campaign-755741b/real/R-autostart/` |
| R-update-banner | Passed the 0.0.1 native test build, available-release banner and dismissal check | `windows/update-banner-native-inspection.log`, `windows/R-update-banner.png` |
| R-installer | Upgrade and fresh/owned lifecycle checks passed. Latest installed R-vdd still fails its UDP keyframe limit, as recorded above | `windows/installer-0975ad8/`, `windows/owned-driver-*-0975ad8.log` |
| Simulator soak | `79d0660` (P7) passes 30 minutes with unattended cleanup: host RSS -9.04%, app -22.47%, 60/60 intervals at least 55 FPS. `4f0c706` passes 30 minutes after the mdns-sd timer-heap correction: host RSS +1.61%, app -0.007%, 60/60 intervals at least 55 FPS, 59.99 average FPS and zero drops. Post-measurement simulator termination required a recorded manual cleanup. Earlier `116d6f5` failed host RSS at +55.57%; keep that failure | `soak-simulator-p7-79d0660/report.md`, `soak-discovery-4f0c706/report.json`, `soak-discovery-4f0c706/cleanup-intervention.json`, `soak-simulator-116d6f5/` |
| Real NVENC soak and final main soaks | Pending: 30 minutes each, host/app RSS growth below 20% from minute five and at least 95% of intervals at 55 FPS | Required before an rc tag |

The independent Tailscale timing probe recorded 13 of 240 round trips above
25 ms, with a maximum of 116.38 ms and no ICMP loss. That observation does not
establish the cause of the UDP row failures. Keep the original repair and
keyframe assertions while investigating them. Hosted CI records simulator FPS
because its three-vCPU VM cannot encode and decode in software at 55 FPS; the
local matrix and soak on the development Mac enforce that gate.

Run `scripts/e2e_matrix.sh` and `scripts/soak.sh 1800` for simulator checks.
Use `scripts/e2e_matrix.sh --real` and `scripts/soak.sh --real 1800` for Windows.
Set `EM_PC_AVAILABLE=1` only with the user's availability authorization and
`EM_RUN_LEVEL=Limited` for normal-user behavior. The user authorized the current
campaign, including testing beyond the initial three hours. The kernel crash
currently suspends Windows GPU/display tests despite that availability.
Verify current monitor geometry before each campaign; input tests reject mismatched bounds.
See `scripts/win/README.md` for the interactive runner and evidence collection.

Each merged host phase's `release.yml` dry-run artifact is recorded in
`PROGRESS.md`. The phases are merged to `main` with the Windows rows above still
open, by Ali's decision; no release candidate is tagged until Windows is
re-verified. No installer here is a published release candidate.

Campaign cleanup and installed state:

1. Use the unlocked console session for capture and input. Check input idle
   time before probes or topology changes. Keep injection inside the focused
   probe, and stop if its foreground identity changes.
2. Build and stage on D:. Preserve the installed profile while using the
   separate test profile. Do not install during capture or dismiss unrelated
   programs' dialogs.
3. Stop only tracked test processes and close all pattern/probe windows.
   Completed Limited test tasks now remove their registrations. One hundred
   earlier completed registrations were removed after verifying their action,
   job identity and exit record; backups remain under the campaign directory.
4. At 20:00 UTC the original AC timeouts, 240 minutes standby and 10 minutes
   display, were restored. Four temporary development firewall rules and the
   stopped host launcher were removed after identity checks. Installed product
   rules remain. No host is running, autostart is off and VDD problem 22 proves
   it is disabled. Evidence: `evidence/windows/restarts/pause-cleanup.json`.
5. Installer `0975ad8` is at
   `D:\AgentWork\em-v030\installer-0975ad8\EternalMonitor-Setup.exe`.
   Its SHA-256 is
   `39E7C78E29F32B30E6204710170A82BBAEEAEC76FE8D244EF1C2A92375B9C32D`.
   The installed host SHA-256 is
   `BE67FA00AE7B5C42EA4F8F2A2DD69CDA371D7A94EFE4BCD683F49E641A0D39EF`.
   Rollback is `C:\Users\aliyo\Downloads\EternalMonitor-Setup.exe`.
   The final desktop cleanup screenshot and published rc installation remain
   pending.

## Ali with the physical iPad

These checks require an iPad, its accessories and the real LAN. Simulator
software decode and loopback USB tests cannot prove them. Record the iPad model,
iPadOS version, app version/build, host version, WiFi band/router, cable and
Windows display scaling. Save the host session log and the app's diagnostics
with each failure; include a short screen recording when timing or feel matters.

| Check | Expected result | Collect if it fails |
| --- | --- | --- |
| TestFlight installation | Install from the external invite/public link. App and installed host show the same marketing version. Connect on the first attempt after pairing. | Invite/build number, install/review error, both versions; setup is in `FRIENDS_TESTING.md` |
| H.264 hardware decoding | With HEVC off, diagnostics say hardware decoder; video is stable near the selected rate | App diagnostics, host encoder line, iPad model |
| HEVC hardware decoding | Explicitly enable HEVC; diagnostics and host both report HEVC and hardware decoding | Both codec lines and the first session/decoder error |
| LAN discovery | On the same 10.0.0.x LAN, Scan finds the PC and stays stable for four minutes; quitting the host removes it promptly | Both LAN addresses, firewall profile, scan recording and host mDNS log |
| QR and pairing | Fresh app prompts for the code; two wrong codes stay rejected; correct code connects. QR carries the token and skips the sheet. Regenerating the token requires pairing again. Six wrong attempts within a minute show a 60-second wait. | Host pairing/session log and sheet screenshots; redact tokens and QR codes before sharing |
| USB service and trust | Apple Devices or desktop iTunes exposes the local usbmuxd service with the iPad attached; accept Trust on the iPad. Host sees the device and the app shows USB while open. | USB card, Device Manager, whether TCP 27015 listens, cable/trust state |
| USB power | Rear USB-C with a C-to-C data cable has already charged this iPad during extended-desktop streaming. Recheck battery percentage and charging state with the intended cable, brightness and workload. The former rear USB-A connection supplied too little power. | Cable/port, brightness, battery before/after and charging diagnostics |
| USB takeover and fallback | Connect over WiFi, then plug in: USB takes over within three seconds. Unplug: WiFi returns within five seconds. Repeat without duplicate sessions. Manual Disconnect stays disconnected. | Host link/session log, app link badge recording and timestamps |
| Audio | PC music reaches the iPad with <150 ms perceived offset; changing Windows output recovers; iPad mute works without stopping video | Endpoint name, packet loss/buffer diagnostics, recording of the clap test |
| Touch, Pencil and view-only | Center/corners hit correctly at 100% and 150% scaling; dragging and two-finger scrolling feel direct; hold gives right-click. Pencil contact/hover behave as advertised. View-only sends no input. | Capture/display geometry, scaling, probe log or recording; note that this release does not promise pressure-sensitive Windows pen injection |
| Hardware keyboard and pointer | Magic Keyboard text, Shift/Ctrl, arrows and copy/paste work with ⌘ as Ctrl; on-screen accessory keys work; trackpad secondary click/scroll and supported Pencil hover work | Exact key/gesture, mapping setting, host input log collected in a harmless test window |
| Reserved iPadOS keys | Globe, ⌘H, ⌘Tab and ⌘Space keep their system behavior | Describe any unexpected interception |
| ProMotion | On a supported iPad, host/virtual display/app request 120 Hz. A strong link sustains 100+ decoded fps; record the actual rate. Compare 60/90/120 host settings. | App/host requested and effective rates, VDD mode, power mode and network |
| Background and reconnect | Home sends BYE; returning resumes. Host restart shows SIGNAL LOST and recovers without tapping. Extended display is removed when disconnected. | Host/app timestamps, recording, VDD device state |
| WiFi loss and latency | Walking toward the edge of coverage reduces bitrate without multi-second freezes, then recovers. Use 240 fps video of both displays to compare latency with the HUD. | Network conditions, bitrate/loss/repair graph, camera recording |

If Apple Devices does not expose TCP 27015 after cable attachment and trust,
record that result before trying desktop iTunes. Physical USB streaming now
works on the reference PC with its existing Apple device support. Cable
takeover, unplug/replug timing and the trust-prompt installation path still
need their own checks.

Logs on the PC are at `%APPDATA%\EternalMonitor\logs\eternal-host-session.log`
(with `.1` and `.2` for prior sessions), or use Copy logs. Preserve failures;
do not mark them passed after only changing a setting. Complete the iPad pass
and a fix/retest round before publishing the final `v0.3.0` tag.
