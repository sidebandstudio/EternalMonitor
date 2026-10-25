# EternalMonitor for Mac: the app bundle

`build-app.sh` builds `target/macos/EternalMonitor.app` (bundle ID `com.sidebandstudio.eternalmonitor.machost`) from the release host, then checks it with `verify-app.sh`. `run-app.sh start|stop|status` runs it in the logged-in GUI session. This is a development build for the `feat/mac-host` branch: signed with an Apple Development certificate, not notarized, and not for distribution.

## What the build does

1. `cargo build --release -p eternal-host` for macOS 14, with Homebrew's FFmpeg 7 (`PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig`).
2. Lays out the bundle: the host as `Contents/MacOS/EternalMonitor` and an icon made from the window icon.
3. `scripts/bundle-dylibs.py` copies every Homebrew library the host needs, directly or through another library, into `Contents/Frameworks`. Each gets the install name `@rpath/<name>`, every reference to it is rewritten to match, and the host gets the run path `@executable_path/../Frameworks`, so nothing is loaded from `/opt/homebrew`. On macOS the host builds ffmpeg-next without avdevice and avfilter (`host/Cargo.toml`), which it never uses; that leaves out SDL, tesseract and about 25 other libraries. If a build ever links sdl2-compat again, the script bundles the SDL3 its initializer loads by name, because without it macOS shows an error dialog before the host starts. The list of bundled libraries, with the licence Homebrew records for each, is written to `Contents/Resources/ThirdPartyLibraries.txt` and `target/macos/ThirdPartyLibraries.txt`.
4. Writes `Info.plist` from `Info.plist.in`: the version from `host/Cargo.toml`, and as `LSMinimumSystemVersion` the newest macOS any Mach-O file in the app was built for (`scripts/min-macos.sh`, never below 14.0). Homebrew builds its libraries for the macOS it runs on, so on devbox1 that is 15.0.
5. Signs every library, then the app, with the hardened runtime and `EternalMonitor.entitlements`, which is empty: the file says why each entitlement the host might seem to need isn't needed.
6. `verify-app.sh` checks the signature (`--deep --strict`), the designated requirement, the team and hardened runtime on every Mach-O file, that no load command or run path names `/opt/homebrew`, the Info.plist keys, and that `LSMinimumSystemVersion` is no older than any Mach-O file's minimum.

Homebrew's FFmpeg is built with `--enable-gpl --enable-version3` and links x264, x265 and other GPL libraries, so this bundle is GPL-3.0-or-later as a whole. A release build uses an LGPL FFmpeg with VideoToolbox only, which also drops most of these libraries.

## Signing

The certificate is "Apple Development: Created via API (4Z7KFJ8DWZ)", team 9X79V37Q89, in its own keychain (`~/Library/Keychains/em-mac-dev.keychain-db`). Its password is read from a file by `scripts/keychain-unlock.c`, so it never appears in any process's arguments or environment.

Shells started by launchd's Background session (T3's agents, for example) can't sign: there, the Security framework looks only at the System keychain domain, and codesign's trusted-identity search fails. `scripts/gui-run.sh` runs a command in the GUI (Aqua) session as a one-shot LaunchAgent and waits for it. `scripts/sign-in-session.sh` runs there: it unlocks the keychain, signs, verifies and locks the keychain again when it ends. `build-app.sh` also locks it again from its own exit trap, after success, failure or interruption. Both scripts are copied from the Mac probe (`spikes/mac-host/probe/scripts/`).

The designated requirement pins the bundle ID and this certificate, so Screen Recording and Local Network permissions survive rebuilds. They are separate from the probe's, which has another bundle ID.

## Running it

`run-app.sh start` opens the app with `open` in the GUI session, so macOS attributes its permissions to the app itself. `run-app.sh stop` sends SIGTERM: the window closes and the host's usual teardown runs (the second screen's display is removed, mDNS says goodbye). Settings and logs are in `~/Library/Application Support/EternalMonitor/`.

The Mac host has no updates yet: it never checks for, downloads or installs one, and Settings shows only its version. Rebuild to update it.

Second screen is off by default. While it's off, the host streams a test picture and creates no display. The switch at the top of Stream writes `mac_second_screen` in `settings.json` and restarts the stream.
