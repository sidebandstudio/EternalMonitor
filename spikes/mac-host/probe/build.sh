#!/bin/zsh
# Builds and signs "EternalMonitor Mac Probe.app" into spikes/mac-host/probe/build/.
# The bundle ID and signing identity stay fixed so TCC grants survive rebuilds.
set -euo pipefail
here=${0:A:h}
root=${here:h:h:h}
export PKG_CONFIG_PATH=${PKG_CONFIG_PATH:-/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig}
app="$here/build/EternalMonitor Mac Probe.app"

MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --release --manifest-path "$root/Cargo.toml" -p eternal-mac-probe-ffi
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$here/Info.plist" "$app/Contents/Info.plist"
swiftc -O -swift-version 5 -target arm64-apple-macos14.0 \
  -import-objc-header "$here/Sources/ProbeBridge.h" \
  "$here"/Sources/*.swift \
  "$root/target/release/libeternal_mac_probe_ffi.a" \
  -L /opt/homebrew/opt/ffmpeg@7/lib -lavutil -lavformat -lavfilter -lavdevice -lswscale -lswresample -lavcodec -liconv \
  -framework VideoToolbox -framework CoreMedia -framework CoreVideo -framework Accelerate -framework ScreenCaptureKit \
  -framework AVFoundation -framework Carbon -framework Metal -framework QuartzCore \
  -Xlinker -dead_strip -Xlinker -dead_strip_dylibs \
  -o "$app/Contents/MacOS/EternalMonitorMacProbe"
"$here/scripts/sign.sh" "$app" --identifier com.sidebandstudio.eternalmonitor.macprobe
codesign -d -r- "$app" 2>&1 | sed -n 's/^designated => /designated requirement: /p'
echo "built $app"
