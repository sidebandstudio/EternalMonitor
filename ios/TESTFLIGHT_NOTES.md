# EternalMonitor 0.3.4: What to Test

Windows and iPad versions listed on the download page work together.
Each 0.3.4 app works with the other app on 0.3.3.

This release fixes the problems reported with 0.3.3 and keeps the stream
going when links, disks or drivers are slow.

### Problems you reported

- EternalMonitor no longer loads AMD's OpenGL driver, which closed it at
  start on PCs with AMD graphics. On Intel graphics it draws with DirectX 12,
  not Intel's Vulkan driver, which crashed some PCs.
- On laptops with two graphics cards and no picture, the PC spots that
  Windows runs it on the wrong card and switches it in one click. If the
  screen moves cards, later connections retry the second screen.
- Over USB on slower PCs, the picture no longer freezes after a big screen
  change. Quality steps down instead.
- After a lost frame on Wi-Fi, the iPad keeps the last clear picture instead
  of smeared blocks or error -12909, and says **Restoring the picture…** if
  it takes over a second.
- If EternalMonitor can't start and a newer version is out, it offers the
  download page. When an update doesn't finish, the PC says why.
- If another program holds the stream port (9876), the Stream page names it.
  Streaming starts once the port is free.
- With the laptop lid closed, the PC says **No display is on** and how to
  keep streaming, and resumes when a screen is back.
- When the PC's address belongs to a VPN or virtual adapter, the PC also
  shows its home network address.

### Settings

- Both apps come in 11 languages, picked in setup or Settings. Buttons,
  settings and setup are translated; messages and diagnostics stay in
  English.
- A new **Troubleshooting** section replaces settings support had you make by
  hand: graphics card, how the window draws, video encoder and smaller
  packets for VPNs.
- Pairing and input handling are stricter. To limit iPads to one network,
  pick it in **Listen on** under Troubleshooting.

### Reliability

- A PC whose second screen can't start is remembered after a restart: it
  mirrors and retries later, or at once after a driver, Windows or app
  update.
- A stuck graphics driver no longer freezes a session. A slow encoder start
  keeps hardware encoding; a stuck one uses the CPU until the driver answers.
- On a slow disk, falling back from the second screen no longer pauses the
  stream.
- After a stall, Apple Pencil strokes no longer lose points.
- On Wi-Fi the iPad waits longer for missing picture data, so a still
  desktop flashes less and short PC stalls end without an extra freeze.
- When iPadOS blocks Local Network access, the iPad offers **Open Settings**
  and says USB works without it.
- Pulling the USB cable no longer leaves a key or mouse button held down.
- PC audio comes back by itself after iPadOS resets its media services.
- The pairing code goes once you type the sixth digit.
- With the on-screen keyboard up, a hardware keyboard no longer drops keys.
- A rare iPad crash when the picture closes is fixed.

### Diagnostics and privacy

- Diagnostics now remove your PC's and iPad's names, Windows user name and
  user folder. Audio devices are sent only by kind, such as headphones.
  **Copy logs** and the iPad's Copy and Export keep the names.
- If you share diagnostics, the iPad reports crashes, hangs and unexpected
  exits: app and system details only.
- Settings says whether sharing is on and when diagnostics were last sent.

### What to test

Over Wi-Fi far from the router, move windows on the PC: the picture should
never smear and recover after **Restoring the picture…**. A busy PC may
pause it briefly.

Turn off EternalMonitor in iPad Settings > Privacy & Security > Local
Network: the iPad should offer **Open Settings**, and USB should still
connect.

Keep **Automatically share diagnostics** on. If the app crashes or freezes,
send us the **Support ID** from Settings > Support > Diagnostics with the
time. Copy and Export should stay put while the status loads.
