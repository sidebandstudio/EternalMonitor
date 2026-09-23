# Design

The Windows app, the iPad app and the website share one look: near-black
surfaces, one lime accent taken from the logo, and the Geist typefaces.

## Logo

The mark is three lime bars on a black rounded square. Use the artwork as it
is; never recolor, redraw or stretch it. The PNG sources in `host/assets` and
`docs/assets` have transparent corners. The iPad `LogoImage` still sits on a
white matte, so `LogoMark` clips it to the rounded square.

## Color

| Token | Hex | Use |
| --- | --- | --- |
| Canvas | `#0A0A0B` | Window and page background |
| Surface | `#121214` | Cards |
| Surface raised | `#1A1A1D` | Inputs, secondary buttons, menus |
| Border | white 8% | Card and row hairlines |
| Text | `#F4F4F5` | Primary text |
| Muted | `#A1A1AA` | Secondary text and descriptions |
| Faint | `#71717A` | Captions, section labels, disabled text |
| Accent | `#E8FF47` | Primary actions, live status, selected state |
| On accent | `#0C0D04` | Text and icons on the accent |
| Warning | `#FBBF24` | Fallbacks and recoverable problems |
| Danger | `#F87171` | Stop, disconnect, destructive actions, errors |

Use the accent sparingly: one primary action per view, plus live indicators.

## Type

Geist for text, Geist Mono for addresses, codes and measurements. Headings
use SemiBold, labels Medium, body Regular. Section labels are small uppercase
Medium with extra tracking. The fonts ship with the apps
(`host/assets/fonts`, `ios/EternalMonitor/Resources/Fonts`) and are licensed
under the SIL Open Font License 1.1.

## Components

- Windows: `host/src/gui/theme.rs` (tokens and fonts) and
  `host/src/gui/widgets.rs` (buttons, toggle, segmented control, slider,
  setting rows, banners, charts).
- iPad: `ios/EternalMonitor/App/DesignSystem.swift`.
- Website: `docs/style.css`.

## Writing

Sentence case everywhere. Say what happens and what to do next, in plain
words: "Restart the stream to apply your changes", not "Pending restart".
Name the device the user is looking at ("PC", "iPad"), not the protocol.

## Automation names

UI tests and the reference-PC runner find controls by their visible names.
The Windows names are listed at the top of `host/src/gui/mod.rs`; the iPad
accessibility identifiers are asserted in `ios/EternalMonitorUITests`. Keep
them when changing copy. `scripts/px.swift --assert-ui` also expects every UI
test screenshot to show some accent color, so keep the lime visible on each
screen (the logo, a primary button or a live indicator).

## Screenshots

- Windows: `EM_UI_REVIEW_DIR=/tmp/review cargo test -p eternal-host --test
  gui_snapshots -- --ignored` renders every page at the real window size.
  The regular test compares against `host/tests/snapshots`.
- iPad (Debug builds): launch with `EM_UI_PREVIEW=display`, `quality`,
  `connecting`, `error`, `pairing`, `settings` or `recent`, plus
  `-allowUSB NO`, to show that state without a PC.
