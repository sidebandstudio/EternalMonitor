# Drawing with Apple Pencil

Apple Pencil input uses the native Windows Ink pen API on Windows 10 version
1809 or later and Windows 11. Pressure, tilt, contact and hover travel as pen
events. The host does not send a second mouse click for the same Pencil event.
Windows may promote pen input to mouse messages for applications that request it.

## Clip Studio Paint setup

1. Install matching host and iPad builds with Pencil support.
2. Enable **Drawing mode** in the iPad settings, then reconnect. Connect the
   iPad to the PC with a data-capable USB cable and confirm the stream says USB.
3. Set the PC's frame-rate limit to 60 or 120 fps as supported by the display,
   encoder and iPad. Drawing mode requests the iPad's highest refresh rate over
   USB but respects the host's limit. Wi-Fi keeps the selected iPad preference.
4. In CSP, open **Preferences > Tablet > Using tablet service** and choose
   **Tablet PC**. This integration uses Windows Ink; it does not install a
   WinTab driver. Keep CSP and the host at the same Windows privilege level.
5. Open **File > Pen Pressure Settings**, draw several light and heavy strokes,
   and select a brush that responds to pressure. Tilt also needs a brush with
   tilt enabled for one of its properties.

Drawing mode enables remote input, ignores fingers on the canvas, and mutes
stream audio. Keyboard and trackpad controls remain available. The small
drawing controls button reveals the toolbar without a three-finger gesture.
The mode applies at the next connection. Outside drawing mode, a Pencil stroke
also suppresses concurrent finger gestures until those fingers lift.

Apple Pencil 1st generation, 2nd generation and Pro support pressure on their
compatible iPads. **Apple Pencil USB-C does not have a pressure sensor.** A USB
connection from iPad to PC cannot add pressure sensing to that Pencil. Hover
availability depends on the Pencil and iPad hardware.

Older hosts continue to receive the existing mouse-style Pencil events outside
drawing mode. Drawing mode shows an unavailable message if the connected host
does not advertise native pen support. The host logs failures to create or
inject the Windows pen device.

## Stroke delivery and latency

The iPad uses precise Pencil positions and all measured coalesced samples. It
does not throttle them to the video's frame rate or send predicted points,
which CSP could not undo once painted. Coordinates use the same aspect-fit
rectangle as the renderer. A stroke that leaves the displayed image clamps to
its edge; a new stroke in a letterbox bar does not start drawing.

USB uses the existing reliable tunnel, TCP_NODELAY in both directions, and
single copies of input edges. A coalesced sample batch crosses the control
queue together, and already-pending framed messages can share a TCP write.
Video already uses low-delay encoding and immediate USB delivery. Drawing mode
also removes audio traffic and requests a higher video rate when available.
These reduce avoidable delays; they do not establish a measured Pencil-to-pixel
latency. That depends on the devices, encoder, canvas and brush settings.

Wi-Fi retains redundant edges and monotonic pen sample IDs. Reordered samples
cannot move a stroke backward or revive a released contact. Missing contact
edges can recover at a later sample, hover, next stroke or session reset.
USB remains the preferred drawing connection because Wi-Fi can lose samples.

## Protocol and Windows behavior

- `HOSTCAP_PEN = 1 << 4` means the host created its native pen device.
- Existing `kind = 1` identifies Pencil. Input version 1 remains 30 bytes.
- Input version 2 has the same 30-byte prefix and appends `tilt_x: i16` and
  `tilt_y: i16`, in little-endian signed degrees from -90 through 90.
- Pressure uses the existing 0 through 1000 field, scaled to Windows' 0 through
  1024 range. Positive X tilt points right; positive Y tilt points down the view.
- Button bit 0 distinguishes contact from hover in version 2. Contact phases
  are down, move, up and cancel. Hover uses move to enter/update and end to leave.
- Native pen events use physical desktop coordinates, including capture-display
  offsets. Disconnect, backgrounding and view cancellation release contact.
- The injector retries `ERROR_NOT_READY` for up to 2 ms because Windows can
  reject calls spaced less than its 0.1 ms timestamp resolution.
- A held contact or hover refreshes after 50 ms without a new sample, preventing
  Windows from expiring a stationary pen. Up and cancellation stop the refresh.

Rust and Swift share golden vectors for pen contact, hover and cancellation.
Unit tests cover pressure and tilt conversion, precise positioning, palm
suppression, USB sequencing, cancellation and old protocol compatibility.
The synthetic host test exercises the authenticated input transport too.

For a Windows receiver check, build `cargo build -p eternal-host --example
pen_probe` using the project's FFmpeg/Visual Studio environment, then run
`scripts/win/Test-PenInput.ps1` in an interactive desktop with `-Executable`
pointing to that executable and `-EvidenceDirectory` pointing to an empty output
directory. It confines injection to its own focused window and checks the
pressure, tilt, coordinates, stationary hold and pen release on reset reported
by GetPointerPenInfo. Windows can report a requested cancellation as an ordinary
zero-pressure pen-up, so the check asserts that contact actually ends.
`-ScreenIndex` can select another display. This does not substitute for an
actual Pencil stroke in CSP.

Before release, verify light-to-heavy strokes, pressure-sensitive dots, tilted
brushes, diagonals and all canvas corners on a physical iPad. Rest a palm before
and during a stroke. Test both iPad orientations, Windows display scaling,
multiple monitors, app switching and cable removal during a stroke. Record the
hardware, CSP version and measured latency rather than promising a fixed number.

Sources: [CSP Tablet PC setup](https://support.clip-studio.com/en-us/faq/articles/20210011),
[CSP pressure adjustment](https://support.clip-studio.com/en-us/faq/articles/20190228),
[Apple Pencil features](https://www.apple.com/apple-pencil/),
[UIKit drawing input](https://developer.apple.com/documentation/uikit/leveraging-touch-input-for-drawing-apps),
[Windows native pen injection](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injectsyntheticpointerinput),
[Windows pen fields](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-pointer_pen_info).
