# v0.3.0 hardware verification

Status as of 2026-09-24: the release-candidate gates below ran on the combined
code, including Apple Pencil drawing (#47), on the reference PC, the physical
iPad and the development Mac. The NVIDIA crash did not recur in a bounded
retest. The 2026-09-23 campaign further down is kept as history; its failing
UDP rows are superseded by the fixes and runs in the next section.

Results belong to the stated revisions. Keep failures with their original
evidence. All paths are relative to the private `EternalMonitor-Handoff`
directory; desktop images, pairing data and captures stay out of git.

## Release candidate verification (2026-09-24)

Reference PC: Windows 11, Ryzen 7 7800X3D with Radeon graphics, GeForce RTX 5080
(driver 591.86), 3440×1440 primary display. Physical iPad: iPad Pro 12.9-inch
(M2), iPadOS 26.2, on the 10.0.0.x WiFi LAN and the PC's rear USB-C port.
Simulator rows use the iPad Pro 11-inch (M4) simulator on iOS 18.6 and reach the
PC over Tailscale. Evidence is under `evidence/rc-hardware-20260924/`.

| Gate | Result | Evidence |
| --- | --- | --- |
| Local simulator matrix, `ebb8a29` | 14/14 PASS with the 55 FPS gate: 59.91–60.08 FPS, 0 drops; loss row 886 repaired, 0 dropped | `matrix-sim-ebb8a29/report.md` |
| Simulator soak, `ebb8a29` | PASS: 1801.5 s, 60/60 intervals ≥55 FPS; RSS from minute 5: host +4.56%, app +0.33% | `soak-sim-ebb8a29/` |
| Windows native suite, `ebb8a29` | Release build clean; 7 test binaries, 199 tests, 0 failures, run from an NVMe copy | `pc-build-test-ebb8a29.log` |
| Windows real matrix, `ed55b55` (before #47) | 13/13 PASS in one run: 57.15–58.74 FPS, 0 drops; loss 0 and burst 0 keyframe requests over Tailscale | `win-matrix-full-2/report.md` |
| Windows real matrix, `ebb8a29` | 12 rows PASS: baseline 58.53, NVENC H.264 58.72, NVENC HEVC 57.93, AMF H.264 58.34, AMF HEVC 58.36, burst 58.73 (0 keyframe requests), audio 57.55, pairing, VDD 58.39, reconnect, GUI 57.28, input. **Loss row: FAIL once** (4 keyframe requests, limit 3; 20 dropped in two WiFi stalls on the Tailscale path, jitter to 7.7 ms), then PASS in 3 separate repeats (1, 2, 2 requests). #47 does not touch the repair path; the same row passed on the physical LAN and in the simulator matrix on this code | `win-matrix-ebb8a29/`, `win-matrix-ebb8a29-rest/`, `win-loss3-ebb8a29-repeat*/` |
| Windows pen injection, `ebb8a29` | PASS: 60 samples, low/high pressure, left/right tilt, positions, stationary hold, 2 downs/2 ups, release on reset | `pen-probe-ebb8a29/result.json` |
| Physical iPad rows, `ebb8a29` | 9/9 PASS, hardware decode, 0 drops: WiFi H.264 58.73, NVENC HEVC 58.77, AMF HEVC 58.90, 3% loss 58.16 (779 repaired, 1 keyframe request), 40 Mbps burst 58.91; USB H.264 58.79, HEVC 58.97, 120 FPS negotiated 59.34 (60 Hz source), extended 2732×2048 58.61 | `device-ebb8a29/device/` |
| Physical USB extended soak, branch before #47 | PASS: 1800.4 s at 2732×2048, 58.42 FPS, 105,240 decoded, 0 drops, 60/60 intervals ≥55 FPS, host RSS +0.11% | `soak-extended-usb/` |
| NVENC WiFi soak on the physical iPad, `ebb8a29` | PASS: 1800.7 s at 3440×1440, 58.61 FPS, 105,600 decoded, 19 dropped, 256 repaired, 10 keyframe requests; 57/60 intervals ≥55 FPS (the 95% minimum); host RSS +1.85% | `soak-nvenc-wifi-ebb8a29/` |
| Installer upgrade and installed host, `11315de` | Release dry-run installer (SHA-256 `1d046e10…ab0a`) upgraded `0975ad8` silently in the console session: exit 0, settings unchanged, 2 VDD tasks and 2 firewall rules, driver `oem33.inf` 23.40.36.27 kept, VDD disabled. Installed host: R-baseline 58.43 FPS, R-vdd 58.63 FPS | `install-state-*.json`, `install-11315de.log`, `installed-11315de/` |

**NVIDIA crash retest.** The installed `0975ad8` host, which crashed on
2026-09-23, ran five cycles of NVENC mirroring (1–8 minutes) followed by the
physical iPad arriving over USB and switching to the extended display. Every
cycle attached the virtual display, streamed 2732×2048 at 60 FPS with 0 drops,
then removed it. Including the later extended-display runs above, the logs
record at least 22 activations on driver 591.86 without a crash, GPU event or
new dump. The fault is not understood; NVIDIA 617.14 is staged on the PC but was
not installed. Evidence: `crash-retest-0975ad8/`.

**WiFi repair.** The physical iPad showed reordering, stalls and a late repair
path on real WiFi; the fixes are recorded in DECISIONS.md ("Let the repair
window follow real WiFi timing"). Over Tailscale the 40 Mbps burst row also
exposed a keyframe request for any frame with more than 64 missing fragments;
13 of 15 such frames later completed (`diag-burst-abandon/`). The loss rows
pass on the physical LAN; over Tailscale the loss row passed 3 of 4 runs on the
final code (see the table).

**Repair overhead on real WiFi.** During the NVENC WiFi soak the iPad sent
58,350 NACKs and the host resent 226,480 fragments, of which 256 filled a gap:
on WiFi most missing fragments are only a few milliseconds late, and the iPad
requests each one as soon as it sees the gap. That is about 1.2–1.4 Mbps, 8–10%
of the 15 Mbps ceiling. A short reorder wait before the first request is a
follow-up; it was not tuned for this candidate.

**Apple Devices notice.** With an iPad on the cable, closing Apple Devices
shows "Your iPad is plugged in, but Apple Devices is not running" and an Open
Apple Devices button, which restarted the service and cleared the notice.
Screenshots: `apple-devices/`.

**Test runner on this PC.** The PC's D: drive (3 TB HDD, 62,237 power-on hours)
stalls for 10–45 s every few minutes and logs controller resets. It failed
test binaries, one host shutdown, a runner job, a host settings write and a log
mirror, while the product checks in those rows passed; each failure keeps its
evidence (`win-matrix-69c4cee*`, `win-matrix-4b0729b-part5`,
`win-matrix-31b363b-part6`). One R-audio run failed at 54.68 FPS after a 1.5 s
capture gap whose cause was not established (`win-matrix-69c4cee-part2`). The
runner now waits for the host to listen,
launches it with an installed host's TEMP and folder, can move its root with
`EM_WIN_ROOT` (these runs used a C: root), and captures every monitor at full
resolution.

## 2026-09-23 campaign

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

The automated physical-iPad rows above already prove hardware H.264 and HEVC
decoding, LAN streaming with loss repair, USB through Apple Devices, 120 FPS
negotiation and the extended display. These checks still need a person with the
iPad, its accessories and the real LAN. Record the iPad model,
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
| USB service and trust | On a PC that has never trusted the iPad, open Apple Devices, plug in and accept Trust. The host shows the Apple Devices notice while the app is closed or missing, and the iPad connects once it runs. | USB card and notice text, whether TCP 27015 listens, cable/trust state |
| USB power | Rear USB-C with a C-to-C data cable has already charged this iPad during extended-desktop streaming. Recheck battery percentage and charging state with the intended cable, brightness and workload. The former rear USB-A connection supplied too little power. | Cable/port, brightness, battery before/after and charging diagnostics |
| USB takeover and fallback | Connect over WiFi, then plug in: USB takes over within three seconds. Unplug: WiFi returns within five seconds. Repeat without duplicate sessions. Manual Disconnect stays disconnected. | Host link/session log, app link badge recording and timestamps |
| Audio | PC music reaches the iPad with <150 ms perceived offset; changing Windows output recovers; iPad mute works without stopping video | Endpoint name, packet loss/buffer diagnostics, recording of the clap test |
| Touch, Pencil and view-only | Center/corners hit correctly at 100% and 150% scaling; dragging and two-finger scrolling feel direct; hold gives right-click. Pencil contact/hover behave as advertised. View-only sends no input. | Capture/display geometry, scaling, probe log or recording; Pencil pressure and tilt are covered by the drawing row |
| Hardware keyboard and pointer | Magic Keyboard text, Shift/Ctrl, arrows and copy/paste work with ⌘ as Ctrl; on-screen accessory keys work; trackpad secondary click/scroll and supported Pencil hover work | Exact key/gesture, mapping setting, host input log collected in a harmless test window |
| Apple Pencil drawing | With Drawing mode on over USB, Clip Studio Paint (Tablet PC) shows light-to-heavy strokes, pressure-sensitive dots, tilted brushes, diagonals and all canvas corners. A resting palm draws nothing; unplugging mid-stroke ends the stroke. Test both orientations and 100%/150% scaling. The automated pen probe covers Windows injection only. | Pencil model, CSP version, scaling, a recording and the measured latency; see `docs/pencil-drawing.md` |
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
