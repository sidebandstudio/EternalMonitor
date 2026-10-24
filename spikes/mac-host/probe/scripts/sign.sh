#!/bin/zsh
# Signs a bundle or binary with the dev identity, non-interactively, from any
# shell (including T3's Background session) by way of gui-run.sh.
#   sign.sh PATH [extra codesign args...]
#
# The keychain password never appears in a process's arguments or
# environment: em-keychain-unlock (keychain-unlock.c) reads it from its file
# and calls SecKeychainUnlock. The keychain is locked again on every path:
# by sign-in-session.sh when it ends, and by this script's EXIT trap (in the
# GUI session, where the unlock applies) after success, failure, timeout or
# interruption. EM_SIGN_KEYCHAIN, EM_SIGN_PASSWORD_FILE, EM_SIGN_IDENTITY and
# EM_SIGN_TIMEOUT override the defaults, for tests with a throwaway keychain.
set -euo pipefail
here=${0:A:h}
identity=${EM_SIGN_IDENTITY:-035589F28BE7EE106A491FE7E8A544A667FE92BF} # Apple Development: Created via API (4Z7KFJ8DWZ)
keychain=${EM_SIGN_KEYCHAIN:-$HOME/Library/Keychains/em-mac-dev.keychain-db}
password_file=${EM_SIGN_PASSWORD_FILE:-$HOME/private_keys/eternalmonitor-mac-dev/keychain-password.txt}
unlock=${here:h}/build/tools/em-keychain-unlock
target=${1:A}; shift

if [[ ! -x $unlock || $here/keychain-unlock.c -nt $unlock ]]; then
  mkdir -p ${unlock:h}
  clang -O2 -Wall -mmacosx-version-min=14.0 -framework Security -framework CoreFoundation -o "$unlock" "$here/keychain-unlock.c"
fi

relock() {
  "$here/gui-run.sh" --timeout 30 -- /usr/bin/security lock-keychain "$keychain" >/dev/null 2>&1 ||
    echo "sign.sh: could not confirm $keychain is locked again" >&2
}
trap relock EXIT
trap 'exit 130' INT TERM HUP
"$here/gui-run.sh" --timeout "${EM_SIGN_TIMEOUT:-180}" -- "$here/sign-in-session.sh" "$unlock" "$keychain" "$password_file" "$identity" "$target" "$@"
