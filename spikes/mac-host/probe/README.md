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

| mode | permissions | what it does |
|---|---|---|
| `grant` | asks for both | Requests Screen Recording, then Accessibility. This lists the app in Privacy & Security. It waits, showing the state, until both are on. |
| `status` | none | Logs the three preflight states. |
| `selftest` | none | 420v IOSurface buffers go through `VtSession::encode_pixel_buffer` (via `ffi/`) and are decoded with VideoToolbox. Runs at 2670×1878@60, 2034×1398@120 and 3440×1440@60. |
| `d` | Screen Recording | ScreenCaptureKit on the main display at 60 and 120 fps, with the window animating, then an idle run with nothing changing. Every frame is encoded zero-copy and decoded. System audio is captured, with a quiet 1.5 s 440 Hz tone so there's something to hear. |
| `e` | Accessibility | The event sequence below, posted with `CGEventPostToPid` (this process only), then with the HID tap. HID posting is guarded: the app must be active, its window key, and the location inside the window. Keys are plain characters, never ⌘ shortcuts. The cursor is put back afterwards. |
| `c` | Screen Recording | The virtual display sequence below. **It adds a display to the Mac**, so it's for when Ali is connected and agrees. |

### Spike e's events

Mouse move, left/right click, drag, pixel and line scroll, keys with and without Shift, and Unicode (é, and 😀 as one event). Then a pen: proximity in, down/drag/up with pressure and tilt, a pure tablet point, and proximity out.

### Spike c's sequence

The display is a `CGVirtualDisplay` at 2670×1878 pixels: HiDPI, modes at 1335×939 points @60/@120, `maxPixels` at full size, vendor `0x454d`, serial `0x26701878`.
1. Checks logical size, backing pixels and refresh at each rate.
2. Captures it at each rate (spike d's capture, run on the virtual display).
3. Arranges it right of the main display, for the session only.
4. Removes it and checks it's gone within 2 s.
5. Recreates it at 2034×1398 @120 and logs whether the arrangement was remembered.
6. Removes it again.

## Why the runner

T3's shells run in a launchd Background session. There, the keychain search list falls back to the System domain, and `launchctl asuser` needs root. See `~/Services/fleet/tasks/mac-host/signing.md`.
