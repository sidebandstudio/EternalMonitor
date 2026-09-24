# Keyboard and pointer controls

Turn on **Control PC** in the iPad settings. A connected host that supports keyboard input shows a **Keyboard** button beside the stream settings. The button opens the iPad keyboard and a row of PC keys. Done, Hide keyboard, and the iPad keyboard's own dismiss button close it.

The accessory row has Esc, Tab, Ctrl, Alt, Win, arrows, Home, End and Delete. Ctrl, Alt and Win stay selected for one key, then release. Tap a selected modifier again to cancel it. On narrow screens the keys scroll horizontally; Done stays visible. Text goes directly to the focused application on the PC. EternalMonitor does not keep a local text field.

A hardware keyboard sends physical HID keys, including both sides of each modifier, function keys and the numeric keypad. **⌘ key acts as** defaults to Ctrl for familiar copy and paste shortcuts. Choose Win to send the Windows key instead. iPadOS reserves Globe and system shortcuts such as ⌘H, ⌘Tab and ⌘Space, so those may not reach the PC. Keyboard layout affects physical keys and modifier shortcuts. Ordinary software-keyboard text is sent as UTF-16, including two units for a surrogate pair.

A connected mouse or trackpad relays left, right and middle buttons, wheel scrolling and hover. Pointer hover is limited to 60 updates per second. Apple Pencil has a separate native Windows Ink path with pressure, tilt and supported hover. See [drawing with Apple Pencil](pencil-drawing.md).

Opening stream settings, backgrounding the app, hiding the keyboard or losing the session releases held keys as appropriate. The host also releases tracked buttons and keys when the session ends or changes. Key edges use the same duplicate suppression as touch edges. All input sources share one event sequence, so switching between touch, keyboard and mouse does not discard unrelated events.

The host advertises `HOSTCAP_KEYBOARD`. The iPad sends KEY, TEXT and HOVER only when that capability is present. Host scan codes follow [Microsoft's keyboard input table](https://learn.microsoft.com/windows/win32/inputdev/about-keyboard-input#scan-codes), and injection uses [KEYBDINPUT scan-code and Unicode flags](https://learn.microsoft.com/windows/win32/api/winuser/ns-winuser-keybdinput).

`scripts/e2e_keyboard.sh` types `Hi!` and Enter through the actual iPad UI, presses Ctrl+Left, and checks the complete deduplicated host injection sequence. It also checks both dismissal controls and retains screenshots. Rust tests cover the HID table, injection resolution, held-key cleanup and the transport path. Swift tests cover modifier state, text splitting, event IDs and pointer behavior. `R-input` remains the Windows check for actual application focus, coordinates and SendInput delivery.
