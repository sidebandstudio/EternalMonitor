# EternalMonitor Mac Probe (phase 0 spikes c, d, e)

A signed development app that needs the one-time grants: Screen Recording and Accessibility.
- **Bundle ID:** `com.sidebandstudio.eternalmonitor.macprobe`.
- **Signing identity:** "Apple Development: Created via API (4Z7KFJ8DWZ)", team `9X79V37Q89`.

Both stay fixed so that TCC grants survive rebuilds. The Designated Requirement pins the identifier and that leaf certificate.

```sh
spikes/mac-host/probe/build.sh      # builds the Rust FFI, swiftc, assembles and signs build/EternalMonitor Mac Probe.app
spikes/mac-host/probe/probe.sh grant | status | selftest | d | e | c | all [--seconds N] [--experimental] [--h264] [--no-tone]
```

## Running it

`probe.sh` launches the app in the logged-in GUI session through `scripts/gui-run.sh`, a one-shot LaunchAgent in `gui/<uid>`. It then waits and prints the log. Logs go to `~/Library/Logs/EternalMonitorMacProbe/`, with `latest-<mode>.log` pointing at the newest run.

- `RESULT` lines are measurements.
- `PASS`/`FAIL` lines are checks.
- `SENT`, `GOT` and `RECV` lines trace spike e.

## Modes

Second screen comes first; mirror is the fallback (Ali, 2026-10-24). **Spike c is the gate.** Capture and input are measured on the virtual display there. `d` and `e` on their own are main-display baselines.

| mode | permissions | what it does |
|---|---|---|
| `grant` | asks for both | Requests Screen Recording, then Accessibility. This lists the app in Privacy & Security. It waits, showing the state, until both are on. |
| `status` | none | Logs the three preflight states. |
| `selftest` | none | 420v IOSurface buffers go through `VtSession::encode_pixel_buffer` (via `ffi/`) and are decoded with VideoToolbox. Runs at 2670×1878@60, 2034×1398@120 and 3440×1440@60. |
| `c` | both | **The second-screen gate.** It adds displays to the Mac, so it's only for when Ali is connected and says go. |
| `d` | Screen Recording | Baseline: the main display at 60 and 120 fps, then idle. Frames are encoded and decoded, and audio is captured. |
| `e` | Accessibility | Baseline: the event sequence posted into the probe window on the main display. |
| `all` | both | `c`, then `d`, then `e`. |

### The gate (`c`) checks

1. **HiDPI mode.** A `CGVirtualDisplay` at 2670×1878 pixels: modes at 1335×939 pt @60/@120, `maxPixels` at full size, vendor `0x454d`, serial `0x26701878`. Logical size, backing pixels, `backingScaleFactor` 2 and refresh are checked when the display is created and at each rate.
2. **Placement.**
   - Logs where macOS puts the display, then places it right of the main display for the session. `--persist-arrangement` places it permanently instead, which writes devbox1's saved arrangement for this display, so it needs Ali's OK.
   - Every recreation logs where the display came back.
3. **Capture.** The probe window fills the virtual display, and its animation runs at that display's refresh. ScreenCaptureKit captures at 60 and at 120 Hz at 2670×1878, then idle. Frames are encoded zero-copy through the host's direct encoder and decoded, with fps, latency, level and audio.
4. **Input.**
   - Wire coordinates (0…65535) map onto the display's bounds as in `display_map::norm_to_display_point`.
   - The corners, the centre and the right edge are checked to land on the virtual display, never its neighbour. The cursor is warped there, not sent an event.
   - The full sequence goes through the pid and HID paths, guarded. Each pointer event's landing point is compared with where it was sent: within 1 pt, and on the display.
   - The sequence: mouse move, left/right click, drag, pixel and line scroll, keys with and without Shift, Unicode (é, and 😀 as one event), then a pen with proximity, pressure and tilt.
5. **Removal and recreation.**
   - A mode change 60→120→60, then removal within 2 s. This is the macOS 26 failure.
   - Recreation at 2034×1398, a mode change 120→60, then removal.
   - A third creation at the inner size, a mode change, then removal.
6. **Process death.** A child process (`--mode vd-child`) owns a display and is killed with SIGKILL, then a second child exits without releasing it. Each time, the display must go away within 2 s.

## Why the runner

T3's shells run in a launchd Background session. There, the keychain search list falls back to the System domain, and `launchctl asuser` needs root. See `~/Services/fleet/tasks/mac-host/signing.md`.
