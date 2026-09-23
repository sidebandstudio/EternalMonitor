# v0.3.0 hardware verification

Status: candidate work is in draft phase branches. No v0.3.0 release candidate
has been published. The installed host and VDD scripts were updated to `cac987c`
on 2026-09-23 to repair physical USB extended-display startup. The full desktop
campaign and installer upgrade/uninstall/reinstall checks remain pending.
The user is keeping the iPad connected for USB testing; preserve that live
session until the next test requires an interruption.

Use the host and iPad app from the same candidate. Keep the evidence with the
candidate's commit, version, build number, encoder, capture resolution, transport,
and date. A failed or unavailable row remains pending until it is rerun.

## Verified by the automated campaign on the reference PC

Reference hardware: Windows 11, Ryzen 7 7800X3D with Radeon integrated graphics,
GeForce RTX 5080. The physical displays reported 3440×1440 and 1920×1080 during
the USB check; record the primary display geometry again before the campaign.
The following preliminary checks
passed on 2026-09-22 and 2026-09-23; the full desktop campaign has **not** passed.

| Check | Result | Evidence in the handoff folder |
| --- | --- | --- |
| Native Rust 1.98 release build, strict clippy, Windows unit and synthetic integration tests | Passed at `5092767`; 184 tests with synthetic tests isolated from the attached iPad | `evidence/windows/em-normal-priority-native-build.log` |
| WASAPI endpoint opens and accounts for silent elapsed time | 96,015 stereo frames in 2.000 seconds at 48 kHz after the clock fix | `evidence/windows/p3-audio-read/` |
| Installer compilation | `EternalMonitor-USB-cac987c-Setup.exe` compiled; installer execution remains pending | `evidence/windows/em-usb-installer-cac987c.log` |
| Limited-user VDD tasks | Enable and disable completed through the host's task runner; missing-task and failed-action paths report failure | `evidence/windows/em-vdd-task-validation-3.log`, `em-vdd-toggle-unit.log`, `em-vdd-missing-task.log` |
| Physical USB extended display | Startup and sustained delivery confirmed at 2732×2048 with NVENC H.264; native build/test load caused two interruptions, followed by automatic recovery. Inherited background priorities were corrected; load and visual confirmation remain pending | `evidence/windows/usb-extend-cac987c-complete.log`, `usb-extend-installed-cac987c.log` |
| Interactive task scheduling | New Limited and Highest tasks use Normal CPU, memory priority 5 and I/O priority 2. The old live host and its launcher were corrected in place at 06:07 UTC | `evidence/windows/new-task-priorities.log`, `installed-priority-repair.jsonl` |
| Desktop availability | The earlier firewall prompt is gone. The user is using the PC; fullscreen pattern and input rows must wait for an idle console | `PROGRESS.md` |

The endpoint read is an API/clock check. It does not prove PC audio was encoded,
transported and heard on the iPad. The native tests use synthetic capture; they
do not prove DXGI, AMF/NVENC, SendInput, or VDD behavior. The separate physical
USB run exercised DXGI, NVENC and VDD startup. Receiver reports prove completed
frames; visual confirmation on the physical iPad is still required. The two
build-time interruptions remain part of the result. Changing CPU priority alone
did not solve them, because the original scheduled task also lowered memory and
I/O priorities. Both effective priorities are now normal; do not label that
change a load-test pass until it has been measured.

The 30-minute simulator soak evidence is in
`evidence/soak-b28687e/simulator/`. It used immutable source and binary copies.
The earlier failing soak reports remain retained alongside it.

Every row below is required before the release candidate. Save host stdout and
stderr, app milestones, simulator and PC screenshots, pixel assertions, duration,
and a machine-readable result under `evidence/real/<row>/`. Keep desktop images
in the private handoff evidence directory, outside git and public PRs.

| Row | Pass condition | Current result |
| --- | --- | --- |
| R-baseline | Installed/current host captures the primary screen through DXGI; H.264 reaches the simulator at ≥55 fps; quadrant pixels match | Pending |
| R-nvenc-h264 | NVENC is actually selected; ≥55 fps for ≥20 measured seconds; no encoder error | Pending |
| R-nvenc-hevc | NVENC HEVC opens; software VideoToolbox decodes the correct pattern at ≥55 fps | Pending |
| R-amf-h264 | Radeon AMF opens; normalized packets decode and SDK FFmpeg validates the saved Annex B stream | Pending |
| R-amf-hevc | Radeon HEVC opens and saved packets validate; record any explicit hardware/resource limitation | Pending |
| R-nvenc-h264-loss3 | 3% first-transmission loss and 1% reorder; ≥55 fps, <2% unrecovered drops, nonzero repairs, no keyframe storm | Pending |
| R-nvenc-burst | Fixed 40 Mbps, one IDR per second; ≥55 fps for ≥20 seconds; no overflow/freeze | Pending |
| R-audio | Session-1 tone passes through WASAPI → Opus → simulator playback; ≥100 packets decoded, ≤2 lost, 1 kHz >−20 dBFS; silence uses small packets | Pending |
| R-pairing | Wrong code rejected; real logged code accepted through the sheet; persisted token reconnects; pairing card captured | Pending |
| R-input | Only the focused input probe receives center/corner clicks within ±3 px, drag, wheel, right-click and `Hi!` plus Enter | Pending |
| R-vdd | Extended display attaches; advertised mode is first in VDD XML; heartbeat reports that resolution; disconnect and host exit remove the display | Physical iPad startup passed; full simulator row and teardown assertions pending |
| R-reconnect | Kill only the tracked host; SIGNAL LOST appears; restart a new process; video resumes in <15 seconds | Pending |
| R-gui | Stream/client/audio/USB/pairing cards, Settings and QR are driven and captured; labels reflect the actual session | Pending |

Additional Windows gates:

| Row | Pass condition | Current result |
| --- | --- | --- |
| R-usb-service | USB card truthfully reports the service/device state; native fake-server tests exercise TCP usbmuxd | Native tests and physical cable streaming passed; GUI assertion pending |
| R-bgra-nvenc / R-bgra-amf | Each hardware encoder runs BGRA for ten minutes without errors; quadrant mean-channel difference versus its YUV run <12/255 | Pending; default stays YUV420P |
| R-vdd-limited | A normal-user host can run the SYSTEM VDD tasks with the new read/execute ACLs | Passed on 2026-09-23; repeat after the full installer upgrade |
| R-fps120 | Requested/negotiated 120 fps is visible; report achieved decode rate and any stutter/errors | Pending |
| R-headless-ctrlc | Ctrl+C reaches the tracked host and exits cleanly with VDD removed | Pending |
| R-autostart | Toggle writes/removes the correct HKCU Run entry and preserves the prior value after the test | Pending |
| R-update-banner | A test build at version 0.0.1 shows the available-release banner; dismissal works | Pending |
| R-installer | Upgrade, limited-user extended-display stream, uninstall cleanup, reinstall; files, task ACLs, TCP/UDP rules and pinned VDD version verified | Pending |
| Simulator and real NVENC soaks | 30 minutes each; both host/app RSS grow <20% from minute five; ≥55 fps in ≥95% of samples | Simulator passed at `b28687e`: host +1.90%, app +0.16%, 60/60 FPS samples. Real NVENC and final main runs pending |

Run simulator checks with `scripts/e2e_matrix.sh` and the long test with
`scripts/soak.sh 1800`. The Windows entry points are
`scripts/e2e_matrix.sh --real` and `scripts/soak.sh --real 1800`. Set
`EM_SIZE=3440x1440` for the current primary monitor; verify it again before
starting. Input tests reject mismatched probe bounds. Consult
`scripts/win/README.md` for the session runner and evidence collection. A script
entry point is not a claim that every hardware row has passed. The thirteen-row
Windows runner is implemented, including cleanup and failure checks. Use the
tables above to identify unfinished hardware execution.

For each merged host phase, retain the `release.yml` workflow-dispatch installer
artifact URL in `PROGRESS.md`. Before tagging, repeat both matrices and both
soaks on main. Then install the exact prerelease installer, verify its published
SHA-256, and repeat R-baseline. Those release artifacts remain pending.

Campaign prerequisites and cleanup:

1. The Windows console must be logged in, unlocked and free of secure-desktop
   prompts. Run capture and input through the interactive session runner, not
   SSH session 0. Check actual input idle time is at least two minutes before
   opening probes or changing topology; do not use an RDP session.
2. Build, stage and retain artifacts on D:. Preserve the installed profile while
   using the separate harness profile. Do not click or dismiss other programs'
   dialogs. Do not run installers during capture.
3. Keep input inside the focused probe. Do not send Windows shortcuts, Alt+F4,
   or text to another application. Stop if the foreground window changes.
4. Stop only tracked processes, close probes/patterns, verify VDD disabled,
   restore the power settings observed before testing, and remove temporary
   debug/release firewall rules. Keep the installed product's rules.
5. End with the verified newest installer installed and a clean desktop
   screenshot. The reference PC currently has the `cac987c` host and task
   scripts copied into the existing installation. The previous files are
   backed up under `D:\AgentWork\em-v030\installed-before-cac987c`.
   The driver is still 23.40.36.27; the pinned 25.5.2 upgrade remains pending.
   Rollback installer: `C:\Users\aliyo\Downloads\EternalMonitor-Setup.exe`.

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
