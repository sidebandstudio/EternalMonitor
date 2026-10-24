# Mac host phase 0: spikes a and b

Run on devbox1 (MacBook Pro M1 Max, macOS 15.7.9 24G830) over SSH, headless, on
2026-10-24, while other builders shared the machine. Load averages are recorded
with each run; they ran from about 130 to 330.

Nothing here posts an event, creates a display, or touches a permission.

## How to run

```sh
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo build --release -p eternal-mac-spikes --examples
spikes/mac-host/run_bench.sh throughput /tmp/em-spikes   # also: paced, reopen, soak60, soak120, gop, gopvt, bgra, direct
cargo run -p eternal-mac-spikes --example cgevent_fields
cargo test -p eternal-mac-spikes
```

- `examples/vt_encode_bench.rs`: FFmpeg VideoToolbox, opened the way `EncoderState::new` does.
- `examples/vt_direct_session.rs`: a direct `VTCompressionSession` with hand-written FFI.
- `examples/cgevent_fields.rs`: builds CGEvents and reads them back.
- `src/keymap.rs` and `src/display_map.rs`: product-quality, with unit tests. Phase 1 moves them to `host/src/input/`.
- `results/`: the RESULT, window and readback lines from every run quoted here.

## Spike a: VideoToolbox encode

### Build and hardware

- FFmpeg: Homebrew `ffmpeg@7` 7.1.5_3, libavcodec 61.19.101, through `ffmpeg-next` 7.1.0 / `ffmpeg-sys-next` 7.1.3. Configure line:

  ```
  libavcodec 61.19.101
  configuration: --prefix='/opt/homebrew/Cellar/ffmpeg@7/7.1.5_3' --enable-shared --enable-pthreads --enable-version3 --cc=clang --host-cflags= --host-ldflags= --enable-ffplay --enable-gnutls --enable-gpl --enable-libaom --enable-libaribb24 --enable-libbluray --enable-libdav1d --enable-libharfbuzz --enable-libjxl --enable-libmp3lame --enable-libopus --enable-librav1e --enable-librist --enable-librubberband --enable-libsnappy --enable-libsrt --enable-libssh --enable-libsvtav1 --enable-libtesseract --enable-libtheora --enable-libvidstab --enable-libvmaf --enable-libvorbis --enable-libvpx --enable-libwebp --enable-libx264 --enable-libx265 --enable-libxml2 --enable-libxvid --enable-lzma --enable-libfontconfig --enable-libfreetype --enable-frei0r --enable-libass --enable-libopencore-amrnb --enable-libopencore-amrwb --enable-libopenjpeg --enable-libspeex --enable-libsoxr --enable-libzmq --enable-libzimg --disable-libjack --disable-indev=jack --enable-videotoolbox --enable-audiotoolbox --enable-neon
  ```
- **Hardware VideoToolbox works over SSH on devbox1.**
  - Every session read back `UsingHardwareAcceleratedVideoEncoder = true`. The read goes through FFmpeg's own session, via `VTEncContext.session`, with a type-ID check.
  - FFmpeg's default `allow_sw=0` makes VT *require* hardware, so the open would fail rather than fall back.
  - Forcing software (`require_sw=1`) gives 4.7 fps at 2670×1878.
  - LLRC sessions don't support that property (-12900), but they were created with `RequireHardwareAcceleratedVideoEncoder`, which would fail without hardware.
- **Load:** these are not clean-room numbers. Other builders kept the 1-minute load between 80 and 770, and every run logs it. Identical configs varied by up to ±30% in unpaced throughput.
- **Thermal:** `pmset -g therm` recorded no thermal or performance warning in any sample, including through both 10-minute soaks.

### What FFmpeg 7.1 actually sets (read back from its session)

| option | HEVC (host: `realtime=1`) | H.264 (host: `profile=baseline`) |
|---|---|---|
| RealTime | true (false with `realtime=0`) | false (the host sets no `realtime`) |
| PrioritizeEncodingSpeedOverQuality | follows `prio_speed` | follows `prio_speed` |
| ProfileLevel | empty (VT default) | `H264_Baseline_AutoLevel`, or `_5_2` with `level=5.2` |
| ExpectedFrameRate | **0**: never set | **30**: VT's default, whatever the real rate |
| MaxKeyFrameInterval | 30 (the host's `set_gop(30)`) | 30 |
| AllowFrameReordering | false | false |
| AverageBitRate | as asked | as asked |
| Low-latency rate control | not available (H.264 only) | `flags=low_delay` turns it on; the session then rejects most property reads |

Setting `ExpectedFrameRate` on FFmpeg's session after open is accepted (status 0), but the level stays the same: VT fixes it when it creates the session.

### Profile and level actually emitted (parsed from the SPS, and the VPS for HEVC)

| stream | needs | FFmpeg emits | direct VT (AutoLevel + ExpectedFrameRate) |
|---|---|---|---|
| HEVC 2670×1878 @60 | 5.1 | Main, Main tier, **5.1** ✓ | Main, Main tier, 5.1 ✓ |
| HEVC 2670×1878 @120 | 5.2 | Main, Main tier, **5.1 ✗** | **Main, High tier, 5.2** ✓ |
| HEVC 2034×1398 @120 | 5.1 | **5.0 ✗** | 5.1 ✓ |
| H.264 2670×1878 @60 | 5.2 (1,182,360 MB/s) | Baseline **5.1 ✗**; 5.2 ✓ with `level=5.2` | Baseline 5.2 ✓ |
| H.264 2670×1878 @120 | 6.0 (2,364,720 MB/s > 5.2's 2,073,600) | 5.1 ✗; 5.2 ✗ with `level=5.2`, the most FFmpeg allows | Baseline **6.0** |
| H.264 2034×1398 @120 | 5.2 | 5.0 ✗ | 5.2 ✓ |

- FFmpeg's H.264 auto level is **not** correct at full size even at 60 fps, because VT thinks the stream is 30 fps. The host must pass `level=5.2` there.
- At 120 fps, H.264 at full size has no legal level FFmpeg can request.
- HEVC at 120 fps needs a direct session, and that session switches to **High tier**. Whether the iPad and Duo decoders accept HEVC Main High tier 5.2, and H.264 level 6.0, is unverified and needs a device check.

### Throughput: unpaced, FFmpeg, NV12, 15 Mbps, steady-state fps

| options | HEVC 2670×1878 | H.264 2670×1878 | HEVC 2034×1398 | H.264 2034×1398 |
|---|---|---|---|---|
| host's options | 133.4 | 121.1 | 110.2 | 206.7 |
| `realtime=0` | 169.5 (one 32.7 s stall) | 164.2 | 329.4 | 152.0 |
| `realtime=1` | 101.5 | 75.6 | 219.1 | 138.4 |
| `realtime=0 prio_speed=1` | 141.5 | 104.5 | 217.4 | 257.9 |
| `realtime=1 prio_speed=1` | 125.6 | 79.9 | 219.5 | 143.3 |
| `low_delay` (LLRC) | n/a | 37.1 | n/a | 37.0 |
| `low_delay realtime=1` | n/a | 33.4 | n/a | 43.5 |
| direct VT, zero-copy IOSurface | 135.6 | 85.0 | | |
| direct VT + LLRC | 66.6 out, 75% dropped | 0 out, all dropped | | |

- **BGRA input:**
  - `hevc_videotoolbox` advertises `bgra` but fails to open (`EINVAL`) unless the colour range is **full** (`AVCOL_RANGE_JPEG`). The host never sets a range, so its Auto input on macOS picks BGRA for HEVC and fails over to libx264.
  - With full range it runs: 60.0 fps at p50 100 ms, and 118.5 fps at 120.
  - `h264_videotoolbox` doesn't take BGRA at all, so the host feeds it YUV420P.
- **send_frame cost:** with NV12, the CPU copy into VT's pixel buffer makes `send_frame` take 0.5–8 ms (p50) at full size. The direct session's zero-copy `VTCompressionSessionEncodeFrame` takes 0.16–0.3 ms.

### Paced runs: frames offered at the target rate, packets polled every 0.2 ms

Latency is from the encode call to the packet, so it excludes capture and network.

| run (30 s unless noted) | out fps | p50 ms | p95 ms | p99 ms | frames in flight |
|---|---|---|---|---|---|
| FFmpeg HEVC full @60, host options | 60.0 | 133.8 | 142.9 | 242.1 | 8 |
| FFmpeg HEVC full @60, host-style drain | 60.3 | 129.8 | 225.2 | 388.5 | 8 |
| FFmpeg HEVC full @60, gop 600 / 100000 | 60.2 / 60.1 | 83.5 / 67.1 | 88.0 / 71.0 | 97.2 / 77.2 | 5 / 4 |
| FFmpeg HEVC full @60, BGRA full range | 60.0 | 100.5 | 104.6 | 116.1 | |
| FFmpeg HEVC full @120, host options | 119.7 | 66.8 | 81.3 | 233.8 | 8 |
| FFmpeg HEVC full @120, `prio_speed=1` | 120.2 | 66.9 | 74.6 | 133.6 | 8 |
| FFmpeg HEVC full @120, `realtime=0 prio_speed=1` | 122.6 | 125.3 | 130.8 | 274.8 | 15 |
| FFmpeg HEVC outer @120 | 117.4 | 41.9 | 97.8 | 513.8 | |
| FFmpeg H.264 full @60, `level=5.2` | 60.0 | 200.8 | 582.7 | 1447 | 15 |
| FFmpeg H.264 full @120 (37 Mbps for a 15 Mbps target) | 118.7 | 126.1 | 252.8 | 605.6 | 15 |
| FFmpeg H.264 full @60, `low_delay` | 29.7 | 33.0 | 44.6 | 67.4 | 0 |
| direct HEVC full @60 (10 s) | 59.6 | 34.8 | 77.9 | 87.2 | |
| direct HEVC full @60, gop 30 / 100000 (20 s) | 59.8 / 59.7 | 37.6 / 36.2 | 80.4 / 79.5 | 92.6 / 93.7 | |
| direct HEVC full @60, `RealTime=false` | 59.8 | 16.2 | 45.4 | 73.6 | |
| direct HEVC full @120 (20 s) | 119.6 | 42.7 | 65.1 | 72.2 | |
| direct HEVC outer @120 | 119.6 | 20.6 | 43.1 | 49.1 | |
| direct HEVC full @60 + LLRC | 29.7 (259/600 dropped) | 14.1 | 38.8 | 44.3 | |
| direct HEVC full @120 + LLRC | 31.9 (880/1200 dropped) | 13.4 | 15.4 | 27.9 | |
| direct H.264 full @60 | 59.2 | 55.3 | 106.2 | 132.9 | |
| direct H.264 full @120 | 84.9 | 104.3 | 111.9 | 137.1 | |
| direct H.264 outer @120 (two runs) | 38.5, 52.5 | 60.7 | 419.8 | 805.9 | |

- **The FFmpeg path keeps 4–15 frames inside the encoder** at full size. The count is steady once reached, so the latency is that many frame intervals: 134 ms at 60 fps with the host's GOP of 30.
  - A longer GOP halves it at 60 fps (4 frames, 67 ms) but not at 120.
  - A direct session holds about 2–5 frames, whatever the GOP.
  - I didn't isolate the cause inside FFmpeg. The CPU copy and the missing `ExpectedFrameRate` are the obvious differences.
- **LLRC is unusable at this size.** It encodes one frame at a time (12–14 ms each at full size) and silently **drops** every frame that arrives while busy: the callback reports status 0 with no sample buffer, which is how VT reports a dropped frame. A keyframe requested on a dropped frame comes out on the next encoded frame instead. H.264 + LLRC at 120 fps dropped everything.
- **`realtime=0` produced two long stalls inside `send_frame`**, 32.7 s and 11.0 s, both under load. Don't use it.
- **The host's GOP of 30 forces an IDR every 0.5 s at 60 fps and every 0.25 s at 120.** That's about 12–13% of the bitrate (soak: 1,260 IDRs of about 110 KB in 630 s at 15 Mbps).

### Go/no-go

| gate | result |
|---|---|
| **Baseline: HEVC 2670×1878 @60 sustained, 10 min** | **Pass on throughput.** 630 s, 37,800/37,800 frames, every 30-s window 56.1–64.7 fps, averaging exactly 60.0 with no drift, no thermal warning. **Latency over the run, FFmpeg with host options: p50 133.9, p95 166.6, p99 503.1 ms (max 2.4 s during load spikes to 770).** That's far too slow for touch input, and it's the encoder queue, not throughput. |
| **Best effort: HEVC 2670×1878 @120** | **Average passes, steadiness and level fail through FFmpeg.** 10-min soak (`realtime=0 prio_speed=1`): 75,600/75,600 frames, 119.9 fps average, but one 11-s stall, windows from 53 to 242 fps while it caught up, p50 125 / p95 174 / p99 695 ms, and level 5.1 where 5.2 is required. **Through a direct session it holds 119.2–119.6 fps with p50 41–43 / p99 72–75 ms (10–20 s runs) at a legal 5.2 High tier.** Unpaced headroom is small: 133–170 fps through FFmpeg and 136 fps direct, on a loaded machine. |
| HEVC 2034×1398 @120 | Passes direct (119.6 fps, p50 20.6 ms, level 5.1). Through FFmpeg it reaches 117 fps, but at level 5.0, one too low. |
| H.264 2670×1878 | 60 fps works only with `level=5.2`, at high latency. 120 fps fails: direct VT tops out at about 85 fps, and no legal level exists below 6.0. |

### Keyframe on request

- `pict_type=I` was honoured every time: 0 misses in 125 requests over the soak, and 0 misses in every paced run.
- A requested IDR takes as long as any other frame. FFmpeg host options: p50 134 ms at 60 and 66 ms at 120, because it waits behind the same queue. Direct: 27–53 ms.
- With LLRC, a request on a frame the encoder drops moves to the next frame.

### Bitrate change cost

| path | cost |
|---|---|
| FFmpeg reopen, HEVC full @60 (14 reopens) | open 9.5–13.1 ms (warm; the cold first open is 236–241 ms), dropping the old session 0.9–51.7 ms, about 4.6 in-flight frames lost per reopen, and a **61 ms (p50) / 111 ms (max) gap in packets**. Every reopen restarts on an IDR. |
| FFmpeg reopen, HEVC full @120 | open 9.4–40.5 ms, drop 42–78 ms, 8 frames lost, gap 110 / 121 ms |
| FFmpeg reopen, H.264 full @60 | open 11.7–19.6 ms, drop 2.2–290 ms, about 14 frames lost, gap 155 / 347 ms |
| **direct `VTSessionSetProperty(AverageBitRate)`** | **0.4–0.55 ms per call; output moved 15.0 → 40.2 Mbps (HEVC) and 17.3 → 39.0 Mbps (H.264) within the same one-second bucket, with no IDR, no dropped frame and no gap.** |

FFmpeg's `videotoolboxenc.c` reads the bitrate only when it creates the session, so reopening is the only option on the FFmpeg path.

### Recommendation for phase 1

1. Use **HEVC** at full size. Phase 1 can stay on FFmpeg at 60 fps if it sets `color_range=JPEG` for BGRA, `level=5.2` for H.264, and a long GOP (the host already forces IDRs on request). Even so, expect 67–134 ms of encoder latency.
2. Plan a **direct `VTCompressionSession`** (via `objc2-video-toolbox`) as the real encoder, sooner than the report suggested. It is the only path that:
   - holds 120 fps at a legal level;
   - keeps encode latency near 40 ms;
   - changes bitrate live, without the 60–150 ms gap;
   - takes ScreenCaptureKit's IOSurfaces with no copy.
3. Do not enable low-latency rate control at these sizes.
4. Check on a real iPad and the Duo that HEVC Main **High** tier 5.2 decodes.


## Spike b: CGEvent construction

723 field checks pass, and none fail. Every event was created from a
`kCGEventSourceStatePrivate` source and read back; nothing was posted.

### Mouse and scroll

| event | type | button | click state | auto pressure |
|---|---|---|---|---|
| move | 5 | 0 | 0 | 0 |
| left down / drag / up | 1 / 6 / 2 | 0 | 1 | 1 / 1 / 0 |
| right down / drag / up | 3 / 7 / 4 | 1 | 1 | 1 / 1 / 0 |
| middle down / up | 25 / 26 | 2 | 1 | 1 / 0 |

Locations keep fractional, negative coordinates exactly, for example (-1016.25, 12.5).

| scroll | point delta | line delta | fixed-point delta | continuous |
|---|---|---|---|---|
| pixels (dy -40, dx 10) | -40, 10 | -4, 1 | -262144, 65536 (16.16) | 1 |
| lines (dy 3, dx -1) | 30, -10 | 3, -1 | 196608, -65536 | 0 |

Pixel scroll events are continuous and phaseless, which is what the report recommends.
macOS derives the line delta as pixels ÷ 10.

### Keys

- All 106 supported usages (0x04–0x65 and 0xE0–0xE7) give the keycode, flags and event type that were set.
- `CGEventCreateKeyboardEvent` already makes the eight modifiers **and Caps Lock** `kCGEventFlagsChanged` (type 12), not key down/up. `ModifierState::update` returns true for all nine.
- Flags must be set by the host. Events from a private source carry none. `ModifierState` produces them:
  - device-independent masks plus side bits (for example, right ⌘ is `0x100010`);
  - `NumericPad` on the keypad and arrows;
  - `SecondaryFn` on F-keys and the navigation keys;
  - a Caps Lock toggle (`AlphaShift`).
- `CGEventCreateKeyboardEvent` also fills the event's Unicode string from the **current layout**, at creation time: kVK 0x00 → "a" under the US layout.

### Non-US layouts

kVK codes name ANSI *positions*, not characters. The layout below, read with `UCKeyTranslate` without enabling it, decides what each position types:

| layout | A pos. | Q pos. | W pos. | Y pos. | Z pos. | 1 pos. | ; pos. | - pos. | ISO § |
|---|---|---|---|---|---|---|---|---|---|
| US | a | q | w | y | z | 1 | ; | - | § |
| French | q | a | z | y | w | & | m | ) | @ |
| German | a | q | w | z | y | 1 | ö | ß | ^ |
| Dvorak | a | ' | , | f | ; | 1 | s | [ | § |

What it means:
- **Hardware keyboards:** HID usages are positional too. If the Mac's input source matches the keyboard attached to the iPad, each key types its own label. If they don't match, the result is what that keyboard would type plugged into the Mac.
- **Software keyboard:** plain text is `KIND_TEXT` (Unicode), so it is layout-independent.
- **The exception:** with a sticky modifier held, `KeyboardRelay.asciiKey` turns a character into its **US** position. On a French Mac, sticky ⌘ plus "z" arrives as the Z position and types ⌘W, which closes the window. Phase 1 should look up the key for that character in the current layout (inverse `UCKeyTranslate`). Windows has the same problem with scan codes today.

### Unicode text

| input | UTF-16 | read back |
|---|---|---|
| é, € | `e9`, `20ac` | exact |
| 😀 in one event | `d83d de00` | exact |
| 😀 as two events (high, then low) | `d83d` / `de00` | each stored as given |
| 👍🏽, 4 units | `d83d dc4d d83c dffd` | exact |
| 25 ASCII units | | exact (no 20-unit truncation at construction) |

The wire sends one UTF-16 unit per `KIND_TEXT` event (`KeyboardRelay.text`), so an emoji arrives as two events. CGEvent stores a lone surrogate without complaint. Phase 1 must hold a high surrogate until its low half arrives, and post the pair as one event. Whether apps accept more than 20 units is a posting question for spike e.

### Tablet

| event | how it is built | fields read back |
|---|---|---|
| proximity enter, pure | `CGEventCreate` + type 24 | enter 1, pointer type 1 (pen), caps 0x25c7, vendor, device |
| proximity enter, as mouse | mouse moved + subtype 2 | the same |
| pen down / drag / up | mouse events + subtype 1 | pressure 0.733989, tilt (0.416639, -0.666646), rotation 47.5, buttons 1/1/0, device |
| tablet point, pure | `CGEventCreate` + type 23 | the same, but no mouse pressure (0) |
| proximity exit | type 24 | enter 0 |

Surprises:
- **Set the subtype first.** Setting it after the tablet fields wipes them: pressure, tilt, rotation and device all read 0. Without the subtype, the fields are not stored at all.
- **Device ID fields are 16-bit.** 0x454d01 read back as 0x4d01. Use one 16-bit ID for every event in a pen session.
- **Mouse pressure is 8-bit, tablet pressure is about 16-bit.** `MouseEventPressure` 0.734 read back as 187/255 = 0.7333, while `TabletEventPointPressure` reads 0.733989. Over 0..1 in 1/1000 steps, the worst tablet-pressure error was 1.5e-5, and the worst tilt error 3e-5. Set both: apps that read `NSEvent.pressure` from a mouse event get the 8-bit value.
- **Tilt scale:** the wire carries degrees and CGEvent wants -1..1. Qt reads ±1 as ±60°, so `deg / 60`, clamped, is what Krita expects.

### Displays and coordinate mapping

Over SSH, `CGGetActiveDisplayList` works and returns devbox1's one display: id 5, bounds (0, 0, 3440×1440), mode 3440×1440 points and 3440×1440 pixels, rotation 0°. The wire's corners and centre all map inside it.

`display_map` keeps the two spaces apart:
- `norm_to_display_point`: wire value → global point. This is for CGEvent locations.
- `norm_to_capture_pixel`: wire value → capture pixel.
- `point_to_capture_pixel`: global point → capture pixel, using each display's own scale.

Each wire value maps to the centre of its 1/65536 slice. So 65535 lands half a slice short of `origin + size` (≤0.026 pt on a 3440-pt display), which is inside the last pixel. It never reaches the neighbouring display's first point, and no clamp is needed.

The tests cover:
- 1× and 2× displays side by side;
- negative origins (a display left of and above the main one);
- a portrait (rotated) display;
- fractional origins;
- 1.5× and per-axis scales.

For every one of the 65,536 values, they assert that the injected point falls in the pixel the user touched.

## Where the tests live for phase 1

`spikes/mac-host/src/keymap.rs` and `src/display_map.rs` are plain Rust with no macOS calls. In phase 1, `git mv` them to `host/src/input/mac_keys.rs` and `host/src/input/mac_display.rs`, beside `hid.rs`, and their tests come along. `DisplayBounds` gains a `From<CGRect>` there.

## Phase 1 sketch: splitting input

Today `input::resolve` (`host/src/input/mod.rs:246`) is portable in name, but Windows-specific in its outputs:
- At `:254` it turns every event into SendInput's 0..65535 virtual-screen space, through `desktop_pixel_to_abs`.
- At `:330` it turns keys into Windows scan codes, through `hid_to_scancode`.

The split, in words:

1. **A platform-neutral `resolve` returns intent, not Windows units.** Pointer events carry the wire's `x_norm`/`y_norm` (the pen variant already does). Keys carry the HID usage and the edge, text carries the UTF-16 unit, wheels carry the client's pixel delta, and buttons and pen phases stay as they are. `EventDeduper`, the stroke state and the pen contact machine stay shared, because they work on these neutral events.
2. **Each platform has a converter, owned by its backend:**
   - **Windows:** `norm_to_output_pixel` → `desktop_pixel_to_abs`, `hid_to_scancode`, `scroll_to_wheel`. That is today's code moved behind the trait, with no behaviour change, and its existing tests still pin it.
   - **macOS:** `norm_to_display_point` on `CGDisplayBounds`, `hid_to_kvk` plus `ModifierState`, surrogate-pair joining, pixel scroll, and the tablet proximity/point sequence.
3. **The geometry each converter needs comes from capture:** `CaptureGeometry` on Windows, and on macOS the display's ID, bounds and pixel size. Today's single `CaptureGeometry` becomes a per-platform type behind the same `record_capture` call.
4. **The off-platform recorder** used by the end-to-end tests records the neutral events, so the tests keep checking the wire → intent path on every OS.
