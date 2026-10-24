#!/bin/zsh
# Signs a bundle or binary with the dev identity, non-interactively, from any
# shell (including T3's Background session) by way of gui-run.sh.
#   sign.sh PATH [extra codesign args...]
set -euo pipefail
here=${0:A:h}
identity=035589F28BE7EE106A491FE7E8A544A667FE92BF # Apple Development: Created via API (4Z7KFJ8DWZ)
keychain=$HOME/Library/Keychains/em-mac-dev.keychain-db
password_file=$HOME/private_keys/eternalmonitor-mac-dev/keychain-password.txt
target=${1:A}; shift
# The password goes through a file the agent reads, never through argv.
"$here/gui-run.sh" -- /bin/zsh -c '
  security unlock-keychain -p "$(cat "$1")" "$2" &&
  codesign --force --timestamp=none --keychain "$2" --sign "$3" "${@:5}" "$4" &&
  codesign --verify --strict --verbose=2 "$4"' sign "$password_file" "$keychain" "$identity" "$target" "$@"
