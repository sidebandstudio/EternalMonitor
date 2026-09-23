# Host settings in v0.3.0

The host offers 30, 60, 90 and 120 fps. An iPad requesting 30, 60 or 120 fps caps the host at the lower of the two settings. Disconnecting clears the client cap without changing the host preference. The iPad's Frame Rate setting applies on the next connection.

Encoder input defaults to YUV420. BGRA and Auto are opt-in until both NVENC and AMF pass the reference PC's color comparisons and ten-minute stability runs. Auto chooses BGRA, then BGR0, only when the encoder advertises support. It otherwise uses YUV420P. libx264 always uses YUV420P. BGRA avoids the host's color conversion step. An unsupported forced BGRA setting reports an encoder error. Changing this setting requires a stream restart; the Settings page offers Restart now. `ETERNAL_INPUT=auto`, `bgra` or `yuv420` overrides the saved setting at launch.

The Connected iPad card shows the latest receiver report. Unrecovered loss counts fragments still missing after repair. Repaired fragments have their own percentage. Both percentages use received, repaired and still-missing fragments as the total. A zero latency estimate displays Measuring until a measurement is available.

The update check runs in a background thread with a three-second request timeout. It checks GitHub's latest stable release at most once per day, including after a failed check or an application restart. Turn off Check for updates in Settings to opt out. Dismiss hides the current banner for the rest of that host session. The host and iPad versions must match.

Each host launch rotates `eternal-host-session.log` into `.1`, retaining `.2` as well. The Windows headless host handles Ctrl+C and Ctrl+Break through the normal shutdown path.

The installer registers UDP and TCP firewall rules for its installed host executable on private and public networks. Uninstall removes those rules. It also grants local users read and execute access to its display tasks. A failed task produces the reinstall guidance and records Windows' exact error in the host log. A task that runs but times out waiting for a display instead asks the user to check the display and driver.

Local format selection, frame-rate negotiation, release version comparison and log rotation tests pass. Hardware BGRA, task permissions, installer lifecycle and native GUI evidence remain required before the P6 gate can pass. YUV420 remains the default while those checks are pending.
