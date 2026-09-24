# Set up EternalMonitor

EternalMonitor turns your iPad into a screen for your Windows PC. You can
mirror your PC's screen, or add the iPad as a second screen next to your
monitor. Touch, Apple Pencil and a keyboard on the iPad control the PC.

Setup takes about 10 minutes. You install one app on the PC and one on the
iPad. Then you connect them over Wi-Fi or with a USB cable.

## What you need

- A Windows 10 or Windows 11 PC.
- An iPad with iPadOS 17 or later.
- **For Wi-Fi:** the PC and iPad on the same home or office network. Guest
  networks usually block devices from seeing each other.
- **For USB:** a USB cable that carries data. The cable that came with your
  iPad works. Some cheap cables only charge.
- **For USB, also Apple Devices**, Apple's free app from the Microsoft Store.
  Windows can only reach an iPad over a cable while Apple Devices is open.
  [Get Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k).

Both apps must come from the same version of EternalMonitor. If you update
one, update the other.

## Step 1: Install EternalMonitor on your PC

1. Go to [eternalmonitor.dev/download.html](https://eternalmonitor.dev/download.html).
   Click **Download for Windows**. If you were asked to test a preview, use
   **Download the preview for Windows** instead. Your browser saves
   **EternalMonitor-Setup.exe**.
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

The iPad app is in testing, so it installs through Apple's TestFlight app.

1. On the iPad, install **TestFlight** from the App Store.
2. Open the EternalMonitor invite link you were given, then tap **Accept** and
   **Install**. If you have no link, ask Ali for one.
3. Open **EternalMonitor** on the iPad.

## Step 3: Connect over Wi-Fi

1. Make sure the PC and iPad are on the same network, and EternalMonitor is
   open on the PC.
2. Open EternalMonitor on the iPad. The first time, the iPad asks whether
   EternalMonitor can **find and connect to devices on your local network**.
   Tap **Allow**. Without this, the iPad cannot see your PC.
3. Choose one of these ways to connect:
   - **Find your PC.** Tap **Find PCs**, then tap your PC under
     **Found on this network**.
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
Apple Devices open on the PC.

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

When it works, the iPad's **USB cable** card says **USB: connected to** your
PC's name, and the PC's **USB connection** row says **An iPad is streaming
over the cable.**

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

1. Connect the iPad first.
2. In the PC window, open **Stream** and find **Display**. Choose
   **Extended display (iPad)**.
3. Click **Restart now** in the message that appears. The iPad reconnects by
   itself after a moment.
4. Drag a window past the edge of your main screen and it moves onto the iPad.

To choose which side of your main screen the iPad is on, open Windows
**Settings**, then **System** and **Display**, and drag the screens into place.
The extra screen exists only while the iPad is connected. It goes away when
you disconnect.

To mirror again, choose **Main display (mirror)** and click **Restart now**.

## Draw with Apple Pencil (optional)

Apple Pencil works as a pen in Windows drawing apps, with pressure and tilt.

1. In the app's settings on the iPad, turn on **Drawing mode**. This also turns
   on **Allow USB connections**.
2. Connect again. Drawing mode applies from the next connection, and a USB
   cable gives the smoothest strokes.
3. In your drawing app, pick the Windows pen setting. In Clip Studio Paint,
   choose **Preferences**, then **Tablet**, then **Tablet PC**, and set up pen
   pressure there.

In Drawing mode, fingers on the canvas are ignored and PC audio is muted. Tap
the round sliders button in the top right corner to show the controls. Apple
Pencil (USB-C) has no pressure sensor, so its strokes don't change width with
pressure. If the iPad says **Native Pencil input is unavailable**, the PC has
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

## If something goes wrong

| What you see | What to do |
| --- | --- |
| **No PCs found** on the iPad | Check that both are on the same Wi-Fi and not on a guest network. Type the **Address** from the PC window instead. |
| **Couldn't reach your PC after 6 tries** | Make sure EternalMonitor is open on the PC and the PC is awake. Check that both are on the same network. |
| **That code didn't match** | Type the six digits currently shown on the PC. The code changes when you click **New code**. |
| **Too many attempts** | Wait a minute, then enter the code again. |
| **This app and EternalMonitor on your PC are different versions** | Install matching versions on both, then connect again. |
| **Your PC is already streaming to another device** | Disconnect the other iPad first. |
| The iPad never connects over USB | Open Apple Devices on the PC and leave it open. Unlock the iPad, accept **Trust**, and try another cable. Then unplug and plug in again. |
| The iPad can't find the PC at all | Open the iPad's **Settings** app, then **Privacy & Security**, then **Local Network**, and turn on **EternalMonitor**. |
| The picture stutters or freezes on Wi-Fi | Move closer to the router, or use a 5 GHz network. Connect the PC to the router with a network cable, or use USB. |
| **Hardware encoder unavailable** on the PC | Update your graphics driver from NVIDIA, AMD or Intel, then click **Restart**. |
| **Extended display unavailable** on the PC | Run EternalMonitor-Setup.exe again to repair the install, then restart the stream. |

**Using a VPN such as Tailscale?** In the PC window, open **Settings** and set
**Packet size** to **1200**.

**Start over with pairing.** On the iPad, tap **Forget paired PCs** in the
app's settings. On the PC, **Forget all iPads** in **Settings** makes every
iPad pair again.

## Getting help

In the PC window, open **Performance** and click **Copy logs**. Paste the
result into an email to
[aliyounes@eternalreverse.com](mailto:aliyounes@eternalreverse.com). Say
what you tried, what you saw, your iPad model, and whether you used Wi-Fi or
USB. The logs include the current pairing code. Click **New code** in the PC
window after sharing them. Don't post the QR code publicly either, because
anyone who scans it can connect.

Pairing keeps other people on your network from connecting, but the stream
itself is not encrypted. Use EternalMonitor on networks you trust, such as
your home Wi-Fi.
