# EternalMonitor 0.3.3: What to Test

Released October 27, 2026. Windows and iPad versions listed on the download page
work together. Each 0.3.3 app still works with the other on 0.3.1 or 0.3.2.

This release fixes **Second screen** on PCs where it didn't start, most often
laptops with Intel and NVIDIA graphics. The iPad then said **Your PC answered,
but no picture arrived**, and over USB the PC's screen blinked and Windows
played its connect and disconnect sounds every few seconds until the cable was
unplugged. Choosing **Mirror my screen** on the PC was the way around it.

### What changed

- The iPad shows the PC's main screen as soon as it connects, while the second
  screen starts. Once Windows adds it, the PC switches to it; if it shows no
  picture within 5 seconds, the iPad goes back to the mirror. Windows now gets
  up to 30 seconds to add it, not 10; if it never does, the mirror stays. Either way the PC says the second screen
  didn't start, and later connections mirror until you click **Try again**,
  restart the stream or change the mode.
- The PC turns the second screen on or off at most once every 10 seconds and
  keeps it on for 10 seconds after the iPad leaves, so a quick reconnect reuses
  it. The screen no longer blinks in a loop.
- The PC tries to start the second screen on the graphics card that drives the
  main screen. Before, the driver chose the card with the most memory, which on
  Intel + NVIDIA laptops drives no screen. This should help those laptops; if
  the second screen still doesn't start, **Mirror my screen** works. The driver
  still chooses when two cards share a name, when the PC can't write the
  driver's settings, or when another app installed the driver and chose a card.
- With **Match the iPad's resolution** on, as it is by default, the second
  screen starts at the iPad's resolution instead of a small, blurry 800×600.
  When the driver doesn't offer that size, it uses the largest one that fits.
- When the second screen doesn't appear, the PC's Stream page says why and what
  to do: run Setup again, restart the PC, or choose Extend with Windows key + P.
  Running Setup again repairs a display driver Setup installed whose device
  was removed or stopped working.
- The first picture arrives sooner on NVIDIA and Intel graphics, especially on
  a still second screen. **Mirror my screen** no longer waits
  for the second screen to turn off at startup.
- After two attempts in a row where the PC answered but sent no picture, the
  iPad stops reconnecting and says to choose **Mirror my screen** on the PC
  (**Main display (mirror)** in 0.3.1). **Try again** starts over. This also
  ends the blinking with PCs still on 0.3.1 or 0.3.2.
- The iPad shows a **Support ID** in Settings > Diagnostics, with **Copy
  Support ID**, and feedback emails include it. PC Settings says **Match the
  iPad's resolution** applies the next time the second screen starts.
- After a failed attempt, the support diagnostics and **Copy logs** hold a
  display snapshot: why the second screen didn't start, and the graphics cards
  and displays Windows reports. The snapshot leaves out the PC's name and the
  user's name; the rest of the log still contains them. Diagnostics name
  Windows 11 correctly.

### What to test

On a laptop with Intel and NVIDIA graphics, choose **Second screen** and connect
the iPad over Wi-Fi, then over USB. The PC's screen should appear at once, then
the second screen a few seconds later, at the iPad's resolution (or the largest
size that fits) while **Match the iPad's resolution** is on. If the second
screen doesn't come up, the iPad should keep the PC's screen with no blinking or
connect sounds, and the PC should say why. Send the PC's **Copy logs** text with
the time; it includes the PC's name.

With the iPad on 0.3.3 and a PC on 0.3.2 whose second screen fails, the iPad
should stop after two tries and say to choose **Mirror my screen**.
