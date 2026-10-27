# EternalMonitor 0.3.3: What to Test

Released October 27, 2026. Install the Windows and iPad 0.3.3 updates together;
each still works with the other app on 0.3.1 or 0.3.2.

This release fixes **Second screen** on PCs where it didn't start, most often
laptops with Intel and NVIDIA graphics. The iPad then said **Your PC answered,
but no picture arrived**, and over USB the PC's screen blinked and Windows
played its connect and disconnect sounds every few seconds until the cable was
unplugged. Choosing **Mirror my screen** on the PC was the way around it.

### What changed

- The iPad shows the PC's main screen as soon as it connects, then switches to
  the second screen once that screen shows a picture. If none arrives within 5
  seconds, the iPad keeps the mirror and the PC says the second screen didn't
  start. Later connections mirror until you click **Try again**, restart the
  stream or change the mode.
- The PC turns the second screen on or off at most once every 10 seconds and
  keeps it on for 10 seconds after the iPad leaves, so a quick reconnect reuses
  it. The screen no longer blinks in a loop.
- On laptops with two graphics chips, the second screen is drawn by the chip
  that drives the main screen. Before, the driver chose the chip with the most
  memory, which on Intel + NVIDIA laptops drives no screen. When another app
  installed the driver and a chip was chosen there, EternalMonitor leaves it.
- The second screen starts at the iPad's resolution instead of a small, blurry
  800×600.
- When the second screen doesn't appear, the PC's Stream page says why and what
  to do: run Setup again, restart the PC, or choose Extend with Windows key + P.
  Running EternalMonitor-Setup.exe again repairs a display driver Setup
  installed whose device was removed or stopped working.
- The first picture arrives sooner on NVIDIA and Intel graphics, most of all
  when the second screen isn't changing. **Mirror my screen** no longer waits
  for the second screen to turn off at startup.
- After two attempts in a row where the PC answered but sent no picture, the
  iPad stops reconnecting and says to choose **Mirror my screen** on the PC
  (**Main display (mirror)** in 0.3.1). **Try again** starts over. This also
  ends the blinking with PCs still on 0.3.1 or 0.3.2.
- The iPad shows a **Support ID** in Settings > Diagnostics, with **Copy
  Support ID**, and feedback emails include it. PC Settings says **Match the
  iPad's resolution** applies the next time the second screen starts.
- Diagnostics and **Copy logs** say why the second screen didn't start and list
  the graphics chips and displays Windows reports, without the PC's or user's
  name. They name Windows 11 correctly.

### What to test

On a laptop with Intel and NVIDIA graphics, choose **Second screen** and connect
the iPad over Wi-Fi, then over USB. The PC's screen should appear at once, then
the second screen a few seconds later at the iPad's resolution. If the second
screen doesn't come up, the iPad should keep the PC's screen with no blinking or
connect sounds, and the PC should say why. Send the PC's **Copy logs** text with
the time.

With the iPad on 0.3.3 and a PC on 0.3.2 whose second screen fails, the iPad
should stop after two tries and say to choose **Mirror my screen**.
