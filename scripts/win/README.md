# Reference PC tests

These scripts use `ssh windows` from devbox1. Builds, toolchains, temporary
files, and logs stay under `D:\AgentWork`. `remote.sh sync <branch>` copies
the scripts and updates the clean PC checkout. It refuses to overwrite
uncommitted work.

```sh
scripts/win/remote.sh sync v030/p7-verification
scripts/win/remote.sh build --release
scripts/win/remote.sh firewall 'D:\AgentWork\Eternal-Monitor\target\release\eternal-host.exe'
scripts/e2e_matrix.sh --real
```

The session runner refuses an RDP or missing console session. Pattern,
probe, and host runs require two minutes idle because host startup can
change the virtual display. Screenshots can run while the console is active.
Detached tasks expire after their timeout and unregister on exit. Job output
and exit status are in `D:\AgentWork\em-v030\jobs`. Host settings are isolated
in `D:\AgentWork\em-v030\state`, preserving the installed app's preferences.

`remote.sh stop-host` checks the recorded PID, creation time, and executable
path, then closes the GUI or delivers Ctrl+C from the same console session.
It reports a failed graceful exit even if forced cleanup succeeds.
`stop-host --kill` is reserved for the reconnect test and failed cleanup.
`pattern stop` and `probe stop`
close the test windows using their flag files. Check task output if a window
does not appear; an SSH process cannot capture the interactive desktop.

`shot <name>` saves private desktop screenshots in the handoff evidence
folder, outside the repository. `EM_EVIDENCE_DIR` can select another private
folder. Never commit desktop screenshots or input-probe logs.

The real matrix runs thirteen rows: baseline, NVENC and AMF in both codecs,
loss/repair, a fixed 40 Mbps burst, WASAPI audio, code pairing, input, VDD,
host restart and the host GUI. Run a selected row while investigating with
`python3 scripts/e2e_real.py --rows R-input`. Results, simulator screenshots,
UI result bundles, desktop screenshots and logs all go under the private
evidence folder. Remaining rows say `NOT RUN` after a failed prerequisite.

The input row launches a fullscreen animated probe, records its PID, then
sets `ETERNAL_INPUT_WINDOW_PID` on the host. This optional guard checks the
foreground process, window title and pointer bounds before every injection;
it permits only the test's pointer events, `Hi!` and Enter. The row fails if
the probe loses focus. It checks five clicks within three desktop pixels, a
held-button drag, upward two-finger scrolling, a right-click and the exact
received text. XCTest's two-touch adapter is compiled into the UI test runner
only. `probe-info` reports the process and bounds; `probe-log` retrieves the
observed Windows events. Do not run this row while using the PC.
It runs last because injected events reset the Windows input-idle clock;
wait two minutes before manually starting another display/input row.

`gui Stream|Settings|QR <name>` uses UI Automation on the tracked host window
and saves a cropped screenshot plus its accessible labels. `vdd-state`
reports the driver state and mode XML. The VDD row verifies both client
disconnect and host exit while streaming, then verifies the display is
disabled. `vdd-disable` runs the installed cleanup task when needed.

AMF rows use `ETERNAL_AMF_DIAG=1`. `diagnostic h264|hevc <row>` validates the
first 120 captured packets using the pinned Windows FFmpeg SDK and retrieves
the bitstream. Hardware selection is asserted from the last opened encoder;
an override request or software fallback cannot pass the row.

`EM_RUN_LEVEL=Limited` runs the host without elevation. After the installer
campaign installs under `D:\AgentWork\em-v030\installed`, use
`EM_INSTALLED_HOST=1` to test that exact installed executable. These variables
do not install anything or modify the user's normal settings profile.

Record current AC power timeouts before changing them and restore those same
values after the campaign. Remove temporary firewall rules with
`remote.sh firewall <path> --remove`, close test windows, stop the host, and
verify that the virtual display is disabled when finished.
