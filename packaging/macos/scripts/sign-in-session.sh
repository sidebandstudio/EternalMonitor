#!/bin/zsh
# Runs inside the GUI session (through gui-run.sh, from build-app.sh): unlocks
# the dev keychain without its password in any argv or environment, signs the
# app's dylibs and then the app, verifies, and locks the keychain again however
# this script ends.
#   sign-in-session.sh UNLOCK_HELPER KEYCHAIN PASSWORD_FILE IDENTITY APP BUNDLE_ID ENTITLEMENTS
set -euo pipefail
unlock=$1 keychain=$2 password_file=$3 identity=$4 app=$5 bundle_id=$6 entitlements=$7
trap '/usr/bin/security lock-keychain "$keychain" 2>/dev/null || true' EXIT
trap 'exit 143' TERM INT HUP
"$unlock" "$keychain" "$password_file"
sign=(codesign --force --timestamp=none --options runtime --keychain "$keychain" --sign "$identity")
# Inside out: every bundled library first, then the app, which seals them.
dylibs=("$app"/Contents/Frameworks/*.dylib(N))
(( ${#dylibs} == 0 )) || "${sign[@]}" "${dylibs[@]}"
"${sign[@]}" --identifier "$bundle_id" --entitlements "$entitlements" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
