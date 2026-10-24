#!/bin/zsh
# Runs inside the GUI session (through gui-run.sh, from sign.sh): unlocks the
# dev keychain without its password in any argv or environment, signs,
# verifies, and locks the keychain again however this script ends.
#   sign-in-session.sh UNLOCK_HELPER KEYCHAIN PASSWORD_FILE IDENTITY TARGET [codesign args...]
set -euo pipefail
unlock=$1 keychain=$2 password_file=$3 identity=$4 target=$5
shift 5
trap '/usr/bin/security lock-keychain "$keychain" 2>/dev/null || true' EXIT
trap 'exit 143' TERM INT HUP
"$unlock" "$keychain" "$password_file"
codesign --force --timestamp=none --keychain "$keychain" --sign "$identity" "$@" "$target"
codesign --verify --strict --verbose=2 "$target"
