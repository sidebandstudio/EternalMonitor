# EternalMonitor 0.3.5: What to Test

Windows and iPad versions listed on the download page work together.
Each 0.3.5 app works with the other app on 0.3.4 and 0.3.3.

This release fixes the problems reported with 0.3.4.

### Problems you reported

- Second screen can now start when Windows already shows the virtual
  display beside your screen, instead of showing the PC's own screen.
- On laptops, Second screen's background tasks can now run on battery.
  Updating repairs them on existing installs.
- With **Match the iPad's resolution** on, the Windows app's Settings >
  Display has a Resolution choice for each iPad's second screen. Connect the
  iPad to choose. A smaller size can make text larger and motion smoother.
- When Windows starts EternalMonitor at sign-in, the window minimises
  shortly after it opens. It stays open when it needs your attention.
- If a Windows Firewall rule blocks EternalMonitor, the PC says so and can
  remove it in one click. Setup removes a block left by a dismissed
  firewall prompt. If Windows blocks all incoming connections, the PC says
  how to let iPads in.
- On Windows 10, when Second screen can't start, the PC says so and can
  mirror your screen instead.
- Screens with an odd width or height now stream.

### On the iPad

These need the 0.3.5 iPad app, and most also need the 0.3.5 PC app.

- When a paired PC that didn't answer shows up on your network again, the
  iPad connects by itself. It can also find a saved PC again when its
  address on your network changes.
- The iPad says when the PC is locked, restarting its stream, stopped for
  good, out of date, or unable to capture from its graphics card.
- The iPad says when Wi-Fi is off. If the PC couldn't reach Apple's USB
  service during a recent session, it suggests checking Apple Devices on
  the PC.
- When both apps support it, the iPad draws the pointer apart from the
  video, so it can move while the desktop is still.
- On lossy Wi-Fi, the iPad keeps repairing a keyframe for longer, and asks
  for a new one when a first picture never completes.
- If no video arrives over Wi-Fi, the iPad can ask the PC for smaller
  packets or try the PC's other address on your network.
- When the PC sleeps, shuts down or signs out, the iPad is told at once
  instead of waiting.
- When a PC doesn't answer over Wi-Fi, the iPad's diagnostics record
  whether it was seen at the address tried.

### Reliability

- If a hardware video encoder is slow to open, video starts on the
  processor and switches over if the encoder opens in time.
- Supported NVIDIA and Intel Quick Sync encoders change quality without
  restarting.
- PC audio comes back when another app lets go of the output.
- Capture recovers after sleep, a lost display and repeated access loss.
  Losing the window's graphics device no longer stops the window or the
  stream.
- If a graphics driver stalls the window at start, the app says so.
- A still mirror can show its first picture before the desktop changes.
- Second screen retries after unlock when locking the PC interrupted it,
  and reads settings saved in another code page. On a laptop with two
  graphics cards, if turning on the virtual display takes the laptop's own
  panel away, the app keeps the virtual display on and asks you to restart.
- The iPad's session stays up while the PC restarts its stream, for up to
  20 seconds after the last frame.
- Input from the iPad is handed to Windows off the PC's network loop, so it
  can't hold the connection up.
- On a laptop, the graphics card that encodes video can be woken when an
  iPad looks for PCs, which can shorten the wait for the first picture.

### Known issues

- If Windows has set the virtual display to **PC screen only**, Second
  screen shows the PC's own screen. Press Win+P and choose **Extend** once.
