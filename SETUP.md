# Set up EternalMonitor

EternalMonitor turns your iPad into a screen for your Windows PC. You can
mirror your PC's screen, or add the iPad as a second screen next to your
monitor. Touch, Apple Pencil and a keyboard on the iPad control the PC.

Setup takes about 10 minutes. You install one app on the PC and one on the
iPad. Then you connect them over Wi-Fi or with a USB cable.

## What you need

- A 64-bit PC with Windows 10 version 1809 or later, or Windows 11.
- An NVIDIA, AMD or Intel graphics card. Without one that can encode video,
  the picture uses the processor and may lag.
- An iPad with iPadOS 17 or later.
- **For Wi-Fi:** the PC and iPad on the same home or office network. Guest
  networks usually block devices from seeing each other.
- **For USB:** a USB cable that carries data. The cable that came with your
  iPad works. Some cheap cables only charge.
- **For USB, also Apple Devices**, Apple's free app from the Microsoft Store.
  Windows can only reach an iPad over a cable while Apple Devices is open.
  [Get Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k).
  Apple Devices needs Windows 10 version 22H2 (build 19045) or Windows 11,
  so on older Windows 10 connect over Wi-Fi. To see your version, open
  **Settings**, **System**, **About**.

Both apps must come from the same version of EternalMonitor. If you update
one, update the other.

## Step 1: Install EternalMonitor on your PC

1. Go to [eternalmonitor.dev/download.html](https://eternalmonitor.dev/download.html).
   Click **Download for Windows** for v0.3.1, the current stable release.
   Your browser saves **EternalMonitor-Setup.exe**.
2. Open **EternalMonitor-Setup.exe**.
3. If a blue **Windows protected your PC** window appears, click
   **More info**, then **Run anyway**. It appears because the installer is not
   code-signed yet.
4. Windows asks **Do you want to allow this app to make changes to your
   device?** Click **Yes**.
5. Click through the installer. Leave **Create a desktop shortcut** ticked if
   you want an icon on your desktop.
6. If Windows asks **Would you like to install this device software?**, click
   **Install**. This is the driver that lets the iPad be a second screen.
7. On the last page, leave **Launch EternalMonitor now** ticked and click
   **Finish**.

The EternalMonitor window opens and says **Ready for your iPad**. Keep this
window open while you use the iPad. You can minimize it. Closing it stops
streaming.

To open it later, use the desktop shortcut or search for **EternalMonitor** in
the Start menu. To have it open whenever you sign in to Windows, go to
**Settings** in the EternalMonitor window and turn on **Start with Windows**.

## Step 2: Install EternalMonitor on your iPad

The iPad app installs through Apple's TestFlight app. Use v0.3.1, build 166,
with the current stable Windows release. The beta is open to anyone with the
invite link.

1. On the iPad, install **TestFlight** from the App Store.
2. Open the [EternalMonitor TestFlight invite](https://testflight.apple.com/join/ppc9QfXc),
   then tap **Accept** and **Install**.
3. Open **EternalMonitor** on the iPad.

## Step 3: Connect over Wi-Fi

1. Make sure the PC and iPad are on the same network, and EternalMonitor is
   open on the PC.
2. Open EternalMonitor on the iPad. The first time, the iPad asks whether
   EternalMonitor can **find and connect to devices on your local network**.
   Tap **Allow**. Without this, the iPad cannot see your PC.
3. Choose one of these ways to connect:
   - **Tap your PC.** The iPad looks for PCs by itself. Tap your PC under
     **Found on this network** and it connects. While it says **Finding…**,
     wait a moment. If it says **Couldn't get its address**, type the
     **Address** instead. Tap **Find PCs** to look again. PCs you used before
     stay under **Recent**.
   - **Scan the QR code.** On the PC, click **QR code**. On the iPad, tap
     **Scan QR code**, allow the camera, and point it at the code on the PC.
   - **Type the address.** Type the **Address** shown in the PC window into
     the iPad, then tap **Connect**.
4. The first time you connect over Wi-Fi, the iPad shows **Pair with your PC**.
   Type the six-digit **Pairing code** from the PC window, then tap
   **Pair iPad**. Scanning the QR code skips this step. You only pair once;
   after that the iPad remembers your PC.

Your PC's screen now appears on the iPad. On the PC, the top of the window
says **Streaming to** followed by your iPad's name.

If Windows shows a **Windows Security Alert** about EternalMonitor, tick
**Private networks** and click **Allow access**. The installer normally sets
this up for you.

## Step 4: Connect with a USB cable (optional)

A cable gives a steadier picture than Wi-Fi and charges the iPad. It needs
Apple Devices open on the PC, and Apple Devices needs Windows 10 version 22H2
(build 19045) or Windows 11. On older Windows 10, use Wi-Fi.

1. On the PC, install **Apple Devices** from the Microsoft Store:
   [apps.microsoft.com/detail/9np83lwlpz9k](https://apps.microsoft.com/detail/9np83lwlpz9k).
   It is free.
2. Open **Apple Devices** and leave it open. You can minimize it, but do not
   close it. EternalMonitor cannot use the cable while Apple Devices is
   closed. Wi-Fi works either way.
3. Plug the iPad into the PC and unlock the iPad.
4. The first time, the iPad asks **Trust This Computer?** Tap **Trust** and
   enter your iPad passcode. If Apple Devices on the PC asks to access the
   iPad, allow it.
5. Open EternalMonitor on the iPad. It connects over the cable on its own.
   USB needs no pairing code.

When it works, the iPad's **USB cable** card says **Connected to** your PC's
name **over the cable**, and the PC's **USB connection** row says **An iPad is
streaming over the cable.**

**If the PC says Apple Devices is missing or closed.** EternalMonitor checks
for this whenever an iPad is plugged in:

- **Your iPad is plugged in, but Apple Devices is not running.** Click
  **Open Apple Devices** and leave it open.
- **Your iPad is plugged in, but Apple Devices is not installed.** Click
  **Get Apple Devices**, install it from the Microsoft Store, open it and
  leave it open.

The iPad then connects on its own. If it doesn't after a few seconds, unplug
the cable and plug it in again.

If the iPad's **USB cable** card has a **Turn on** button, tap it. That
switches **Allow USB connections** back on in the app's settings on the iPad.

## Step 5: Use the iPad as a second screen (optional)

At first the iPad mirrors your main screen. To use it as an extra screen
instead:

1. At the top of the **Stream** page in the PC window, next to
   **Use the iPad as**, click **Second screen**. This works over Wi-Fi and USB.
2. If the iPad is connected, the stream restarts and the iPad reconnects by
   itself after a moment. If it isn't, the second screen appears as soon as it
   connects.
3. Drag a window past the edge of your main screen and it moves onto the iPad.

To choose which side of your main screen the iPad is on, click
**Arrange screens** next to the choice. Windows opens **Display** settings;
drag the screens into place. The extra screen exists only while the iPad is
connected. It goes away when you disconnect.

To mirror again, click **Mirror my screen**. To mirror one particular monitor,
open **Settings** and choose it under **How the iPad shows your PC**.

The iPad app's settings explain this under **Second screen or mirror**. The
choice is always made on the PC.

## Draw with Apple Pencil (optional)

Apple Pencil works as a pen in Windows drawing apps, with pressure and tilt.

1. In the app's settings on the iPad, turn on **Drawing mode**. This also turns
   on **Allow USB connections**.
2. Connect again. Drawing mode applies from the next connection. If you are
   already connected, tap **Reconnect now to apply**. A USB cable gives the
   smoothest strokes.
3. In your drawing app, pick the Windows pen setting. In Clip Studio Paint,
   choose **Preferences**, then **Tablet**, then **Tablet PC**, and set up pen
   pressure there.

In Drawing mode, fingers on the canvas are ignored and PC audio is muted. Tap
the round sliders button in the top right corner to show the controls. Apple
Pencil (USB-C) has no pressure sensor, so its strokes don't change width with
pressure. If the iPad says **Apple Pencil can't draw on this PC**, the PC has
an older EternalMonitor or a Windows 10 version before 1809. Update both.

## Using EternalMonitor

- **Tap** to click. **Drag** to move something. **Two fingers** scroll.
  **Touch and hold** to right-click. Apple Pencil works as a pen in Windows.
- **Show the controls:** tap with **three fingers**. With **Control PC** off,
  a single tap works. The controls have **Keyboard**, a gear button for settings
  and **Disconnect**.
- **Sound:** PC audio plays on the iPad when **Stream PC audio** is on in the
  PC window and **Play PC audio** is on in the app's settings on the iPad.
- **Only watch, don't control:** turn off **Control PC** in the app's settings
  on the iPad.
- **Keyboard:** a keyboard attached to the iPad types on the PC. The ⌘ key
  works as Ctrl. Change it to the Windows key with **⌘ key acts as**.
- **Stream stats:** turn on **Show stream stats** in the app's settings on the
  iPad to see frame rate and connection quality.

The app's settings open from the gear button, at the top of the connect screen
or in the controls while streaming.

## Keep both apps up to date

Update both devices when a new version comes out. Each app tells you.

**On the PC:** a banner says **EternalMonitor 0.3.2 is available** (with the
new version number), and Windows shows one notification. Click **Update now**.
When Windows asks for permission, click **Yes**. EternalMonitor closes,
installs the update and opens again by itself. If you are streaming, the update
waits until you stop. **Later** hides the banner until the next day.

In the PC window's **Settings**, **Check for updates automatically** turns the
daily check on or off. **Check now** checks right away.

**On the iPad:** the connect screen says **A new version is ready in
TestFlight**. Tap **Open TestFlight**, then **Update**. The app's settings show
the same message under **Updates**. To get new versions without asking, open
TestFlight, tap **EternalMonitor** and turn on **Automatic Updates**.

The update check reads a small file from eternalmonitor.dev. It sends nothing
about you or your devices.

## If something goes wrong

Find the message you see in the first column. Each one is quoted exactly as
the app shows it, so you can search this page for its first few words. The
apps use curly apostrophes (’) in a few messages; if a search finds nothing,
try fewer words. If your problem isn't here, see [Known issues](https://eternalmonitor.dev/known-issues.html).

### Messages on the iPad

| What the iPad says | What it means | What to do |
| --- | --- | --- |
| **No PCs found. Check that EternalMonitor is open on the PC and both are on the same Wi-Fi, not a guest network. Or type the Address shown on the PC.** | The iPad looked on its network and no PC with EternalMonitor open answered. | Check that EternalMonitor is open on the PC and both are on the same Wi-Fi, not a guest network. Tap **Search again**, or type the **Address** shown in the PC window. |
| **EternalMonitor can't look for PCs. Turn on Local Network for EternalMonitor in Settings.** | The iPad isn't allowed to search your local network. | Tap **Open Settings** and turn on **Local Network**. |
| **Couldn't get its address. Type the Address shown on the PC.** under a found PC | The iPad saw the PC's name but not its address. | Type the **Address** shown in the PC window, then tap **Connect to PC**. |
| **Your PC didn't answer** | Nothing came back from the PC. It may be closed, asleep, on another network, or blocked by a firewall. | Check that EternalMonitor is open on the PC and says **Ready for your iPad**, that the PC is awake, and that both are on the same Wi-Fi, not a guest network. If all of that is right, Windows Firewall or security software is probably blocking EternalMonitor: run EternalMonitor-Setup.exe again, or use a USB cable. Then tap **Try again**. |
| **Your PC didn't answer over the cable** | The cable is connected, but EternalMonitor on the PC didn't reply through it. | Check that EternalMonitor and Apple Devices are both open on the PC, then unplug the cable and plug it in again. |
| **Your PC answered, but no picture arrived** | The connection worked, but the PC couldn't send its screen. | Look at EternalMonitor on the PC and follow any message it shows. Click **Restart** there, then tap **Try again** on the iPad. |
| **That looks like the pairing code, not the PC's address** | A six-digit number was typed where the address goes. | Type the **Address** from the PC window, for example 192.168.1.20. The iPad asks for the code after it connects. |
| **That looks like your router's address, not your PC's** | Nothing answered at an address ending in .1, which is usually the Wi-Fi router. | Type the **Address** shown in the PC window. |
| **That address doesn't look right** | The iPad couldn't find anything by that name. | Type the **Address** exactly as the PC window shows it. |
| **This iPad isn't connected to a network** | Wi-Fi is off on the iPad, or it hasn't joined a network. | Turn on Wi-Fi and join the same network as the PC, or use a USB cable. |
| **EternalMonitor isn't allowed on your local network** | Local Network access is off for EternalMonitor on the iPad. | Tap **Open Settings** and turn on **Local Network**. |
| **Couldn't connect to your PC** | The connection failed for another reason. | Check that both are on the same Wi-Fi and that EternalMonitor is open on the PC, then tap **Try again**. |
| **This iPad couldn't start the connection** | Something on the iPad side didn't start. | Close EternalMonitor on the iPad and open it again. |
| **Pair with your PC** | This is the first Wi-Fi connection to this PC. | Type the six-digit **Pairing code** from the PC window and tap **Pair iPad**. You only do this once. |
| **That code didn’t match. Check the PC and try again.** | The code typed isn't the one the PC shows now. | Type the six digits currently shown on the PC. The code changes when someone clicks **New code** on the PC. |
| **Too many attempts. Wait before trying again.** | The PC stops accepting codes for a minute after several wrong ones. | Wait until the button counts down, then enter the code again. |
| **Lost the connection to your PC** | The PC stopped responding during the stream. | The PC may have gone to sleep, closed EternalMonitor, or lost its network. Wake it, check EternalMonitor is open, then tap **Try again**. |
| **Reconnecting to your PC…** | The iPad is trying to get the stream back by itself. | Keep EternalMonitor open on the PC and wait. |
| **Couldn't reconnect to your PC** | Every automatic attempt failed. | Check that the PC is awake and EternalMonitor is open, then tap **Try again**. |
| **The connection over the cable stopped** | The USB connection broke after it had started. | Keep EternalMonitor and Apple Devices open on the PC, unlock the iPad, then unplug the cable and plug it in again. |
| **The USB cable was disconnected** | The cable came out, or the PC lost it. | Plug the cable back in. The iPad reconnects by itself. |
| **USB connections were turned off** | **Allow USB connections** is off in the iPad app's settings. | Turn on **Allow USB connections** to use the cable again. |
| **Your PC ended the session** | Someone clicked **Stop** on the PC, changed a setting that restarts the stream, or closed EternalMonitor. | Tap **Try again** when the PC says **Ready for your iPad**. |
| **Your PC is already streaming to another device** | Only one iPad can connect at a time. | Disconnect the other iPad first, then tap **Try again**. |
| **Your PC is updating EternalMonitor** | The PC is installing a new version. | Wait. With **Reconnect automatically** on, the iPad connects again by itself. Otherwise tap **Try again** once EternalMonitor has opened on the PC. |
| **Your PC hasn't come back from its update yet** | The update is taking longer than expected. | Check that EternalMonitor opened again on the PC. Windows may be waiting for you to click **Yes** there. Then tap **Try again**. |
| **This iPad app is older than EternalMonitor on your PC, so they can't connect. Update the iPad app: open TestFlight and tap Update.** | The two apps are different versions and can't connect. | Tap **Open TestFlight**, then **Update**. Connect again. |
| **EternalMonitor on your PC is older than this iPad app, so they can't connect. Update EternalMonitor on your PC: click Update now on the PC, or download it from eternalmonitor.dev.** | The two apps are different versions and can't connect. | On the PC, the window says **Update this PC to connect your iPad**. Click **Update now**. If the button says **Check for updates** instead, click it, then click **Update now** when it appears. If no update is found, click **Download from the website** and run the installer. |
| **This app and EternalMonitor on your PC are different versions. Update both, then try again.** | The two apps can't connect. | Install matching versions on both, then connect again. |
| **A new version is ready in TestFlight** | A newer iPad app is available. | Tap **Open TestFlight**, then **Update**. |
| **Apple Pencil can't draw on this PC. Update EternalMonitor on the PC; Windows 10 needs version 1809 or later.** | The PC has an older EternalMonitor, or Windows 10 older than version 1809. | Update EternalMonitor on the PC, and update Windows. |
| **That QR code isn't from EternalMonitor. Scan the code shown on the PC’s Stream page.** or **That QR code couldn't be read. Try again, or type the address instead.** | The camera read a different code, or couldn't read it. | Scan the code from **QR code** on the PC's Stream page, or type the address instead. |

### Messages on the PC

| What the PC window says | What it means | What to do |
| --- | --- | --- |
| **Streaming is off** | EternalMonitor isn't accepting iPads right now. | Click **Start streaming**. |
| **No network found. Connect this PC to Wi-Fi or Ethernet, or use a USB cable.** under **Address** | The PC isn't connected to Wi-Fi or Ethernet. | Connect the PC to your network, or use a USB cable. |
| **Your iPad is plugged in, but Apple Devices is not running** | Windows can only reach the iPad over the cable while Apple Devices is open. | Click **Open Apple Devices** and leave it open. It can stay minimized. |
| **Your iPad is plugged in, but Apple Devices is not installed** | Apple's free Apple Devices app is missing. | Click **Get Apple Devices**, install it from the Microsoft Store, open it and leave it open. |
| **Video is using the processor instead of the graphics card** | The graphics card couldn't encode the video, so the picture may lag and the PC may run hot. | If the message has **Use recommended settings**, click it. Otherwise update the graphics driver from NVIDIA, AMD or Intel, then click **Restart**. Some graphics cards can't encode video at all. |
| **The second screen couldn't start, so your main screen is mirrored** | The iPad couldn't become a second screen, so it shows your main screen instead. The line below the title says why. | **Windows' display driver stopped responding. Restart the PC to use the second screen.** Restart Windows; this message has no button. **The second screen isn't installed…** or **Part of the EternalMonitor install is missing or blocked…** Click **Download installer**, run EternalMonitor-Setup.exe, then click **Try again**. **Windows didn't add the second screen in time…** Click **Try again**. If it keeps happening, run EternalMonitor-Setup.exe again. |
| **Streaming to … is paused** | Your iPad is connected, but Windows stopped sharing the screen. The message above it on the Stream page says why. The iPad keeps the last picture. | Follow that message. The picture comes back by itself once Windows allows it. |
| **Your PC is showing the lock screen or a permission prompt** | Windows hides the screen from apps while it's locked or asking for permission. The iPad keeps the last picture. | Unlock the PC or answer the prompt. The picture comes back by itself. |
| **Windows is changing the display** | The resolution changed, or a full-screen game or app started. | Wait. The picture comes back by itself. |
| **Windows won't share this screen from the graphics card EternalMonitor uses** | On PCs with two graphics chips, often laptops, Windows only shares the screen from the chip it's connected to. | Open Windows **Settings**, **System**, **Display**, **Graphics**, choose EternalMonitor and pick the graphics card your screen is connected to (on a laptop, usually **Power saving**). Quit EternalMonitor and open it again. |
| **Another app is recording or sharing your screen** | Windows lets only a few apps capture the screen at once. | Close the screen recorder or meeting app. The picture comes back by itself. |
| **This Windows session is disconnected** | A Remote Desktop session disconnected, so there's no screen to share. | Sign in at the PC itself. |
| **Restart the stream to apply your changes** | Some display and video settings only apply when the stream restarts. | Click **Restart now**. The iPad reconnects by itself. |
| **Update the iPad app to connect** | The iPad app is older than EternalMonitor on the PC. | On the iPad, open TestFlight and tap **Update**. |
| **Update this PC to connect your iPad** | EternalMonitor on the PC is older than the iPad app. | Click **Update now**. If the button says **Check for updates** instead, the PC doesn't know about the new version yet: click it, then click **Update now** when it appears. If it still finds nothing, click **Download from the website** and run the installer. |
| **EternalMonitor … is available** | A new version is ready. | Click **Update now**, then **Yes** when Windows asks. If you are streaming, the update waits until you stop, or click **Stop and update**. |
| **The update did not finish** | The update couldn't download or install. You still have the old version. | Click **Try again**. If it fails again, click **Download from the website** and run the installer. |
| **EternalMonitor could not start** or **EternalMonitor stopped unexpectedly** | The app hit an error. The message names it and the log file. | Open EternalMonitor again; after a failed start it tries another way to draw its window. Update the graphics driver. If it happens again, send the log file it names, from `%APPDATA%\EternalMonitor\logs`, through the feedback form. |
| **Audio unavailable: …. Video continues. Turn Stream PC audio off and on to retry.** (… is the reason) | The PC's sound couldn't be captured. Video keeps working. | Turn **Stream PC audio** off and on again on the **Stream** page. |

### Messages from Windows, and problems without a message

| What you see | What it means | What to do |
| --- | --- | --- |
| **Windows protected your PC** when installing | The installer isn't code-signed yet. | Click **More info**, then **Run anyway**, for a file you downloaded from eternalmonitor.dev. |
| **Windows Security Alert** about EternalMonitor | Windows Firewall is asking whether EternalMonitor may use the network. | Tick **Private networks** and click **Allow access**. |
| The iPad never connects over USB | Usually Apple Devices is closed, the iPad isn't trusted, or the cable only charges. Apple Devices also needs Windows 10 version 22H2 (build 19045) or Windows 11. | Open Apple Devices on the PC and leave it open. Unlock the iPad, tap **Trust**, and try another cable. Then unplug and plug in again. |
| The picture stutters or freezes on Wi-Fi | The Wi-Fi is losing data or is too slow. | Move closer to the router or use a 5 GHz network. Connect the PC to the router with a network cable, or use USB. |
| The second screen is small and blurry | Windows started it at a low resolution. | See [Known issues](https://eternalmonitor.dev/known-issues.html#second-screen-small). |

**Using a VPN such as Tailscale?** In the PC window, open **Settings** and,
under **Advanced**, set **Packet size** to **1200**.

**Start over with pairing.** On the iPad, tap **Forget paired PCs** in the
app's settings. On the PC, **Forget all iPads** in **Settings** makes every
iPad pair again.

## Getting help

In the PC window, open **Performance** and click **Copy logs**. Paste the
result into a [bug report on GitHub](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose)
or an email to [hello@sideband.studio](mailto:hello@sideband.studio).
Say what you tried, what you saw, your iPad model, and whether you used Wi-Fi or
USB. On iPad, open **Settings → Support → Diagnostics**, then **Export diagnostics**
for a text file or **Copy diagnostics**. Include both reports when possible.

Reports include Windows build, CPU, GPU and driver details; iPad hardware,
iPadOS and app build; encoder/decoder setup, connection events and periodic
stream statistics. Each side records its own device information. Matching
session IDs and timestamps help compare the reports without changing the
wire protocol. Logs survive app restarts and rotate automatically. Windows
keeps three files up to 2 MiB each; iPad keeps three up to 1 MiB each.

Pairing codes and tokens are redacted from saved support logs and exports.
Reports still contain PC/device names and local network addresses. Review
them before posting publicly. Keep the QR code private because anyone who
scans it can connect.

Pairing keeps other people on your network from connecting, but the stream
itself is not encrypted. Use EternalMonitor on networks you trust, such as
your home Wi-Fi.

### Automatic diagnostics

Configured releases send device information, errors and stream statistics to EternalMonitor support automatically. Windows Settings and iPad Settings → Support → Diagnostics show the current upload state and an automatic sharing control. Offline events retry after reconnection or relaunch. PC names and network addresses may be included; pairing credentials, screen frames and audio are excluded. Manual exports remain available.

Uploads use encrypted HTTPS. Support history is retained for up to 30 days and may be removed sooner when storage limits are reached. Disabling sharing clears pending uploads; diagnostics already sent expire under the retention policy. Existing releases require an update to enable uploads.
