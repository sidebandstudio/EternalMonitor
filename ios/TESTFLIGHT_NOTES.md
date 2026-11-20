# EternalMonitor 0.3.5: What to Test

Windows and iPad versions listed on the download page work together.
Each 0.3.5 app works with the other app on 0.3.4 and 0.3.3.

This release fixes the problems reported with 0.3.4.

### Problems you reported

- Second screen now starts when Windows already shows the virtual display
  beside your screen, instead of showing the PC's own screen.
- On laptops, Second screen's background tasks can now run on battery.
  Updating repairs them on existing installs.
- The Windows app's Settings > Display has a Resolution choice for each
  iPad's second screen. Connect the iPad to choose. A smaller size can make
  text larger and motion smoother.
- When Windows starts EternalMonitor at sign-in, the window minimises
  shortly after it opens. It stays open when it shows a warning.
- If Windows Firewall blocks EternalMonitor, the PC says so and can remove
  the block in one click. Setup removes a block left by a dismissed
  firewall prompt. If Windows blocks all incoming connections, the PC says
  how to let iPads in.
- On Windows 10, when Second screen can't start, the PC mirrors your screen
  and says so.
- Screens with an odd width or height now stream.

### On the iPad

These need the 0.3.5 iPad app, and most also need the 0.3.5 PC app.

- The iPad connects by itself when a paired PC on your network that didn't
  answer comes back, and finds it again when its address changes.
- The iPad says when the PC is locked, restarting its stream, stopped for
  good, out of date, or unable to capture from its graphics card.
- The iPad says when Wi-Fi is off, and suggests checking Apple Devices on
  the PC when a cable connection can't reach it.
- The pointer is drawn by the iPad, apart from the video, so it moves even
  when the desktop is still.
- On lossy Wi-Fi, the iPad keeps repairing a keyframe until it completes,
  and asks for a new one when a first picture never completes.
- If no video arrives, the iPad asks the PC for smaller packets or tries
  the PC's other address.
- When the PC sleeps, shuts down or signs out, the iPad is told at once
  instead of waiting.
- When a PC doesn't answer, the iPad's diagnostics record whether it was
  seen on the network and which network path the iPad used.

### Reliability

- If a hardware video encoder is slow to open, video starts on the
  processor until it is ready. An encoder that fails isn't tried again at
  that size until the app restarts.
- Supported NVIDIA and Intel Quick Sync encoders change quality without
  restarting.
- PC audio comes back when another app lets go of the output.
- Capture recovers after sleep, a lost display and repeated access loss. A
  lost graphics device no longer stops the window or the stream.
- If a graphics driver stalls the window at start, the app says so.
- A still mirror shows its first picture without waiting for the desktop to
  change.
- Second screen retries after the PC is unlocked, and reads settings saved
  in another code page. If turning on the virtual display takes a laptop's
  own panel away, the app keeps the virtual display on and asks you to
  restart.
- The iPad's session stays up for up to 20 seconds while the PC restarts
  its stream.
- Input from the iPad can no longer hold up the PC's network connection.
- On a laptop, the graphics card that encodes video is woken when an iPad
  looks for PCs, so the first picture comes sooner.
- USB connections survive a failed device list.

### Known issues

- If Windows has set the virtual display to **PC screen only**, Second
  screen shows the PC's own screen. Press Win+P and choose **Extend** once.
