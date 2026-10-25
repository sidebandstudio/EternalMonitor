#!/bin/zsh
# Builds target/macos/EternalMonitor.app: the release host, its FFmpeg dylibs
# from Homebrew in Contents/Frameworks, dev-signed with the hardened runtime,
# then checked by verify-app.sh.
#
#   packaging/macos/build-app.sh
#
# Signing runs in the logged-in GUI session (scripts/gui-run.sh), the only
# session where codesign finds the dev identity; see README.md. The bundle ID
# and identity stay fixed so Screen Recording and Local Network grants survive
# rebuilds. EM_SIGN_KEYCHAIN, EM_SIGN_PASSWORD_FILE, EM_SIGN_IDENTITY and
# EM_SIGN_TIMEOUT override the defaults.
set -euo pipefail
here=${0:A:h}
root=${here:h:h}
out=$root/target/macos
app=$out/EternalMonitor.app
exe=$app/Contents/MacOS/EternalMonitor
bundle_id=com.sidebandstudio.eternalmonitor.machost
prefix=/opt/homebrew
identity=${EM_SIGN_IDENTITY:-035589F28BE7EE106A491FE7E8A544A667FE92BF} # Apple Development: Created via API (4Z7KFJ8DWZ)
keychain=${EM_SIGN_KEYCHAIN:-$HOME/Library/Keychains/em-mac-dev.keychain-db}
password_file=${EM_SIGN_PASSWORD_FILE:-$HOME/private_keys/eternalmonitor-mac-dev/keychain-password.txt}
unlock=$out/tools/em-keychain-unlock

export PKG_CONFIG_PATH=${PKG_CONFIG_PATH:-$prefix/opt/ffmpeg@7/lib/pkgconfig}
# A local build never uploads diagnostics.
unset ETERNAL_DIAGNOSTICS_ENDPOINT ETERNAL_DIAGNOSTICS_INGEST_KEY
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/host/Cargo.toml" | head -1)
[[ -n $version ]] || { echo "build-app.sh: no version in host/Cargo.toml" >&2; exit 1; }

# Only the host: a workspace build unifies ffmpeg-next's features with the
# spike crates', which brings avdevice and avfilter back (verify-app.sh
# fails if they are bundled).
MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --release --locked -p eternal-host --manifest-path "$root/Cargo.toml"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Frameworks"
cp "$root/target/release/eternal-host" "$exe"

# The window icon, as the app icon. The source is 256 px, so the set stops there.
iconset=$(mktemp -d /tmp/em-iconset.XXXXXX)/AppIcon.iconset
mkdir -p "$iconset"
for size in 16 32 128 256; do
  sips -z $size $size "$root/host/assets/icon_256.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 128; do
  sips -z $(( size * 2 )) $(( size * 2 )) "$root/host/assets/icon_256.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns -o "$app/Contents/Resources/AppIcon.icns" "$iconset"
rm -rf "${iconset:h}"

python3 -I "$here/scripts/bundle-dylibs.py" "$app" "$exe" "$prefix" "$app/Contents/Resources/ThirdPartyLibraries.txt"
cp "$app/Contents/Resources/ThirdPartyLibraries.txt" "$out/"

# The app can't run on a macOS older than its newest-built library needs
# (Homebrew builds for the macOS it runs on), and never below 14.
min_macos=$(print -l 14.0 "$("$here/scripts/min-macos.sh" "$app")" | sort -V | tail -1)
sed -e "s/@VERSION@/$version/g" -e "s/@MIN_MACOS@/$min_macos/g" "$here/Info.plist.in" > "$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"

if [[ ! -x $unlock || $here/scripts/keychain-unlock.c -nt $unlock ]]; then
  mkdir -p "${unlock:h}"
  clang -O2 -Wall -mmacosx-version-min=14.0 -framework Security -framework CoreFoundation \
    -o "$unlock" "$here/scripts/keychain-unlock.c"
fi
relock() {
  "$here/scripts/gui-run.sh" --timeout 30 -- /usr/bin/security lock-keychain "$keychain" >/dev/null 2>&1 ||
    echo "build-app.sh: could not confirm $keychain is locked again" >&2
}
trap relock EXIT
trap 'exit 130' INT TERM HUP
"$here/scripts/gui-run.sh" --timeout "${EM_SIGN_TIMEOUT:-300}" -- "$here/scripts/sign-in-session.sh" \
  "$unlock" "$keychain" "$password_file" "$identity" "$app" "$bundle_id" "$here/EternalMonitor.entitlements"

"$here/verify-app.sh" "$app"
echo "built $app ($version)"
