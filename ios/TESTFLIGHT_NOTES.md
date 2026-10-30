# EternalMonitor 0.3.4: What to Test

Released [date set at release]. Windows and iPad versions listed on the
download page work together. We streamed the 0.3.4 apps with the other app on
0.3.3, both ways, and the 0.3.4 PC app with the iPad app on 0.3.2.

This release fixes the problems people reported with 0.3.3 and keeps the
stream going through slow links, slow disks and stuck graphics drivers.

### Problems you reported

- PCs with AMD graphics no longer close as EternalMonitor starts, and on
  Intel graphics the window now draws with DirectX 12, so Intel's Vulkan
  driver can't crash it either.
- On laptops with two graphics cards that showed no picture, the PC spots
  that Windows runs it on the wrong card and switches it in one click.
- Over USB on slower PCs, the picture no longer freezes after a big screen
  change. Quality steps down instead.
- After a lost frame on Wi-Fi, the iPad keeps the last clear picture instead
  of smeared blocks or error -12909, and says **Restoring the picture…** if
  that takes more than a second.
- If EternalMonitor can't start and a newer version is out, the error offers
  the download page. When an update doesn't finish, the PC says why.
- If another program holds the stream port (9876), the Stream page names it,
  and streaming starts by itself once the port is free.
- With the laptop lid closed, the PC says **No display is on** and how to
  keep streaming, and carries on by itself when a screen is back.
- When the PC's address belongs to a VPN or a virtual adapter, the PC also
  shows its home network address for the iPad.

### Settings

- A new **Troubleshooting** section replaces the settings support used to
  ask you to make by hand: graphics card, how the window is drawn, video
  encoder, and smaller packets for VPNs.

### Reliability

- A PC whose second screen can't start is remembered after a restart: it
  mirrors and tries again later, or at once after a driver, Windows or
  EternalMonitor update.
- A stuck graphics driver can't freeze a session: its calls are time-limited.
  A slow encoder start keeps hardware encoding, and a stuck one hands the
  picture to the CPU until the driver answers.
- On a slow disk, falling back from the second screen no longer pauses the
  stream.
- After a stall, Apple Pencil strokes no longer lose points.
- On Wi-Fi the iPad waits longer for a missing piece of the picture before
  asking for a new one, so a still desktop flashes less and short PC stalls
  end without an extra freeze.
- When iPadOS blocks Local Network access, the iPad says where to turn it on,
  offers **Open Settings**, and notes that USB works without it.
- PC audio comes back by itself after iPadOS resets its media services.

### Diagnostics and privacy

- The Windows app no longer sends your PC's name, Windows user name or user
  folder with its diagnostics, and sends stream statistics once a minute
  instead of several times a second. **Copy logs** still includes the names.
- The iPad app reports crashes, hangs and unexpected exits with its
  diagnostics, if you share them: app and system details only.
- Settings says whether sharing is on and when diagnostics were last sent.
  On the iPad, Copy and Export no longer move while that status loads.

### What to test

On the iPad, over Wi-Fi far from the router, move windows on the PC: the
picture should never smear, and should recover after **Restoring the
picture…**. While the PC is busy, the picture may pause but should resume
without a long freeze.

Turn off EternalMonitor in iPad Settings > Privacy & Security > Local
Network: the iPad should say so, **Open Settings** should open its settings,
and USB should still connect.

Keep **Automatically share diagnostics** on. If the app crashes or freezes,
send us the **Support ID** from Settings > Support > Diagnostics with the
time. That screen's buttons should stay put while its status loads.
