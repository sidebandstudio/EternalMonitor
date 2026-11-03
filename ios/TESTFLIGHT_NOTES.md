# EternalMonitor 0.3.4: What to Test

Windows and iPad versions listed on the download page work together.
Each 0.3.4 app works with the other app on 0.3.3.

This release fixes the problems reported with 0.3.3.

### Problems you reported

- EternalMonitor no longer closes at start on PCs with AMD graphics, or
  crashes on some with Intel graphics.
- On laptops with two graphics cards and no picture, the PC spots the wrong
  card and fixes it in one click. If the screen moves cards, the second
  screen is retried.
- Over USB on slower PCs, a big screen change lowers quality instead of
  freezing the picture.
- After a lost frame on Wi-Fi, the iPad keeps the last clear picture, not
  smeared blocks or error -12909, and says **Restoring the picture…**.
- If EternalMonitor can't start and a newer version is out, it offers the
  download page. When an update doesn't finish, the PC says why.
- If another program holds port 9876, the Stream page names it. Streaming
  starts once it's free.
- With the lid closed, the PC says **No display is on** and how to keep
  streaming, and resumes when a screen is back.
- If the PC's address belongs to a VPN or virtual adapter, it also shows its
  home network address.

### Settings

- Both apps come in 11 languages, picked in setup or Settings. Messages and
  diagnostics stay in English.
- A new **Troubleshooting** section has the graphics card, window drawing,
  video encoder and VPN packet size.
- Pairing and input handling are stricter. **Listen on** in Troubleshooting
  limits iPads to one network.

### Reliability

- On NVIDIA graphics, an iPad Pro's full-size second screen gets nearly
  every frame with **Encoder input** on Automatic, up from about 91%.
- If the second screen can't start, the PC mirrors and retries later, even
  after a restart, or at once after a driver, Windows or app update.
- A stuck graphics driver no longer freezes a session: video uses the
  processor until it answers.
- On a slow disk, falling back from the second screen no longer pauses the
  stream.
- If Wi-Fi briefly cuts the iPad off, the PC streams on for up to 10
  seconds. When the PC ends a session, the iPad reconnects at once.
- On Wi-Fi a still desktop flashes less, and short PC stalls end without an
  extra freeze.
- After a stall, Apple Pencil strokes no longer lose points.
- If iPadOS blocks Local Network access, the iPad offers **Open Settings**
  and says USB still works.
- Pulling the USB cable no longer leaves a key or mouse button held down.
- PC audio comes back after iPadOS resets its media services.
- The pairing code goes once you type the sixth digit.
- With the on-screen keyboard up, a hardware keyboard no longer drops keys.
- After a stream restart, the iPad no longer shows an old frame.
- A rare iPad crash when the picture closes is fixed.

### Diagnostics and privacy

- Diagnostics now remove your PC's, iPad's and audio devices' names, and
  your Windows user name and folder. **Copy logs** and the iPad's Copy and
  Export keep them.
- If you share diagnostics, the iPad reports crashes, hangs and unexpected
  exits: app and system details only.
- Settings says whether sharing is on and when diagnostics were last sent.

### Known issues

- If your PC's screen stays black after installing or after Second screen,
  press Win+P and choose **PC screen only**, or restart the PC.

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
