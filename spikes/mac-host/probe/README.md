# EternalMonitor Mac Probe (phase 0 spikes c, d, e)

A signed development app that needs the one-time grants: Screen Recording and Accessibility.
- **Bundle ID:** `com.sidebandstudio.eternalmonitor.macprobe`.
- **Signing identity:** "Apple Development: Created via API (4Z7KFJ8DWZ)", team `9X79V37Q89`.

Both stay fixed so that TCC grants survive rebuilds. The Designated Requirement pins the identifier and that leaf certificate.

```sh
spikes/mac-host/probe/build.sh      # builds the Rust FFI, swiftc, assembles and signs build/EternalMonitor Mac Probe.app
spikes/mac-host/probe/probe.sh grant | status | selftest | d | e | c | all [--seconds N] [--experimental] [--h264] [--no-tone] [--deadline S]
spikes/mac-host/probe/tests/run.sh  # safe tests: no display, no events, no TCC changes
```

`ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER=1 probe.sh c` is the same as `--experimental`: full-size HEVC at 120 fps on High tier, logged as informational.

## Running it

`probe.sh` launches the app in the logged-in GUI session through `scripts/gui-run.sh`, a one-shot LaunchAgent in `gui/<uid>`. It then waits and prints the log. Logs go to `~/Library/Logs/EternalMonitorMacProbe/`, with `latest-<mode>.log` pointing at the newest run.

- `RESULT` lines are measurements.
- `PASS`/`FAIL` lines are checks. `INFO` lines are informational checks that never count as failures.
- `SENT`, `TAP`, `GOT`, `MISSING` and `RECV` lines trace spike e.
- `ABORT` means the probe's hard deadline passed or it was sent a signal.

Every run has a hard deadline (`grant` 30 min, `c` 10 min, `all` 15 min, `d` 4 min, `e` and `selftest` 2 min, `status` 30 s; `--deadline` overrides it). When it passes, the probe restores the pointer, kills its children by PID and exits, and any virtual display it owns goes with it. `probe.sh` waits 60 s longer. On a timeout or Ctrl-C it stops the probe and its children by the PIDs they recorded, after checking each is still this probe's binary. Nothing is ever killed by name.

After every run, `probe.sh` lists the online displays (`--mode displays`, which creates nothing) and says whether a probe virtual display (vendor `0x454d`) is still there. If one is, it stops the recorded owners and checks again.

## Modes

Second screen comes first; mirror is the fallback (Ali, 2026-10-24). **Spike c is the gate.** Capture and input are measured on the virtual display there. `d` and `e` on their own are main-display baselines.

| mode | permissions | what it does |
|---|---|---|
| `grant` | asks for both | Requests Screen Recording, then Accessibility. This lists the app in Privacy & Security. It waits, showing the state, until both are on. |
| `status` | none | Logs the three preflight states. |
| `selftest` | none | 420v IOSurface buffers go through `VtSession::encode_pixel_buffer` (via `ffi/`) and are decoded with VideoToolbox. Runs at 2670×1878@60, 2034×1398@120 and 3440×1440@60. |
| `c` | both | **The second-screen gate.** It adds displays to the Mac, so it's only for when Ali is connected and says go. |
| `d` | Screen Recording | Baseline: the main display at 60 and 120 fps, then idle. Frames are encoded and decoded, and audio is captured. |
| `e` | Accessibility | Baseline: the event sequence posted to the probe on the main display. On the system path it sends only keys, which the probe's tap swallows. |
| `all` | both | `c`, then `d`, then `e`. |

### The gate (`c`) checks

1. **HiDPI mode.** A `CGVirtualDisplay` at 2670×1878 pixels: modes at 1335×939 pt @60/@120, `maxPixels` at full size, vendor `0x454d`, serial `0x26701878`. Logical size, backing pixels, `backingScaleFactor` 2 and refresh are checked when the display is created and at each rate.
2. **Placement.**
   - Logs where macOS puts the display, then places it right of the main display for the session. `--persist-arrangement` places it permanently instead, which writes devbox1's saved arrangement for this display, so it needs Ali's OK.
   - Every recreation logs where the display came back.
3. **Capture.** The probe window covers the virtual display at shielding level, and its animation runs at that display's refresh. ScreenCaptureKit captures at 60 and at 120 Hz at 2670×1878, then idle. Frames are encoded zero-copy through the host's direct encoder and decoded. A run passes only if:
   - over a fixed 5 s window (from 1 s in), capture delivered at least 0.9× the rate (108 fps at 120) and the content changed every frame;
   - a marker the probe draws (a sync pattern, the low 16 bits of its display's ID, and a frame counter) reads back from every captured **and decoded** frame with the captured display's ID, so the frames are the virtual display's;
   - the encoder kept up with at least 0.9× its planned rate, and every frame encoded and decoded;
   - the probe's tone was captured (missing audio fails).

   Full-size HEVC at 120 Hz is capped at 60 fps on Main tier by design, so that run's encode is checked against 60. **Encode at 120 is tested at the Duo outer size**, 2034×1398 on Main tier, in step 5. With `--experimental` the full-size run encodes 120 on High tier; its encode checks are `INFO` lines.
4. **Input.**
   - Wire coordinates (0…65535) map onto the display's bounds as in `display_map::norm_to_display_point`.
   - The corners, the centre and the right edge are checked to land on the virtual display, never its neighbour. The cursor is warped there, not sent an event.
   - The sequence: mouse move, left/right click, drag, pixel and line scroll, keys with and without Shift, Unicode (é, and 😀 as one event), then a pen with proximity, pressure and tilt.
   - **pid path:** every event is posted with `CGEventPostToPid` to the probe itself.
   - **System (HID) path:** posted only while the probe's own event tap is installed (annotated session, appended last, on its own thread). It is the last check before delivery:
     - keys are recorded there and swallowed, so no app receives them;
     - mouse and pen events pass only inside the virtual display, which the probe window covers at shielding level, with the probe frontmost and macOS not annotating them for another process; anything else is swallowed and fails;
     - no ⌘, ⌃, ⌥ or fn, and no modifier key events;
     - no tap, no system path; if macOS turns the tap off, posting stops;
     - untagged input (someone using the Mac) stops the run; buttons and keys still down are released through the tap; the pointer is always put back.
   - Every event carries a tag (`eventSourceUserData`: a run tag and a sequence number). What arrives (in the window, or at the tap for system-path keys) is matched against the full sequence: type, location within 1 pt, and order. Anything missing fails.
5. **Removal and recreation.**
   - A mode change 60→120→60, then removal within 2 s. This is the macOS 26 failure.
   - Recreation at 2034×1398 at 120 Hz, captured and encoded at 120 fps (the encode-at-120 gate), then a mode change to 60 and removal.
   - A third creation at the inner size, a mode change, then removal.
6. **Process death.** A child process (`--mode vd-child`, in a process group of its own) owns a display and is killed with SIGKILL, then a second child exits without releasing it. Each time, the display must go away within 2 s. Reading the child's output has a 6 s deadline, after which it is killed by PID. A child never outlives its parent or 20 s.
7. **Nothing left.** No probe virtual display may be online at the end.

## Tests

`tests/run.sh` needs no display, posts no events and changes no permissions:
- `tests/main.swift`: the tap's decisions (inside and outside the display, frontmost or not, tagged or not, modifiers), the sequence matcher (missing, reordered, offset events), deadlines (a child that never prints is killed by PID in time; the watchdog), the fps window and the marker;
- `bash -n` and `zsh -n` on every script;
- `gui-run.sh` stopping a timed-out command and its group by PID;
- `sign.sh` against a throwaway keychain: `ps` never shows the password, and the keychain is locked again after a failure and a timeout.

## Why the runner

T3's shells run in a launchd Background session. There, the keychain search list falls back to the System domain, and `launchctl asuser` needs root. See `~/Services/fleet/tasks/mac-host/signing.md`.

`sign.sh` unlocks the dev keychain with `build/tools/em-keychain-unlock` (built from `scripts/keychain-unlock.c`), which reads the password from its file and calls `SecKeychainUnlock`, so it never appears in a process's arguments or environment. The keychain is locked again on every path.
