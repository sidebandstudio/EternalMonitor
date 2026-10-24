#!/bin/zsh
# Safe tests for the Mac probe: no virtual display is created, no event is
# posted, and no TCC or System Settings state changes.
#   1. Sources/Safety.swift and Measure.swift: the event tap's decisions, the
#      sequence matcher, deadlines (a silent child killed by PID, the
#      watchdog), fps windows and the content marker (tests/main.swift).
#   2. bash -n and zsh -n on every script.
#   3. gui-run.sh: on timeout the command and its group are stopped by PID.
#   4. sign.sh with a throwaway keychain: no process's arguments or
#      environment ever show the password, and the keychain is locked again
#      after a failure and after a timeout. The real keychain is not used.
set -uo pipefail
here=${0:A:h}
probe=${here:h}
tmp=$(mktemp -d /tmp/em-probe-tests.XXXXXX)
failures=0
check() { if [[ $1 == 0 ]]; then echo "PASS $2"; else echo "FAIL $2"; failures=$(( failures + 1 )); fi; }
cleanup() { [[ -n ${dummy:-} ]] && security delete-keychain "$dummy" 2>/dev/null; rm -rf "$tmp"; }
trap cleanup EXIT

echo "== 1. logic"
swiftc -O -swift-version 5 -target arm64-apple-macos14.0 "$probe/Sources/Safety.swift" "$probe/Sources/Measure.swift" \
  "$here/main.swift" -o "$tmp/logic-tests"
"$tmp/logic-tests"; check $? "logic tests"

echo "== 2. script syntax"
for f in "$probe"/*.sh "$probe"/scripts/*.sh "$here"/*.sh; do
  bash -n "$f" && zsh -n "$f"; check $? "bash -n and zsh -n ${f#$probe/}"
done

echo "== 3. gui-run.sh timeout"
start=$SECONDS
"$probe/scripts/gui-run.sh" --timeout 2 -- /bin/sh -c "sleep 61 & echo \$! > $tmp/grandchild; echo \$\$ > $tmp/child; sleep 60" >/dev/null 2>&1
code=$?
check $(( code == 124 ? 0 : 1 )) "gui-run returns 124 on timeout (got $code after $(( SECONDS - start )) s)"
sleep 0.5
child=$(cat "$tmp/child" 2>/dev/null); grandchild=$(cat "$tmp/grandchild" 2>/dev/null)
[[ -n $child ]] && ! kill -0 "$child" 2>/dev/null; check $? "the timed-out command (pid ${child:-?}) is gone"
[[ -n $grandchild ]] && ! kill -0 "$grandchild" 2>/dev/null; check $? "its background child (pid ${grandchild:-?}, same group) is gone"

echo "== 4. sign.sh with a throwaway keychain"
before=$(security list-keychains -d user)
password="em-dummy-${RANDOM}${RANDOM}-not-a-secret"
print -r -- "$password" > "$tmp/password"
chmod 600 "$tmp/password"
dummy=$tmp/dummy.keychain-db
security create-keychain -p "$password" "$dummy" >/dev/null 2>&1   # throwaway, before sampling starts
security lock-keychain "$dummy"
unlock=$probe/build/tools/em-keychain-unlock
mkdir -p ${unlock:h}
clang -O2 -Wall -mmacosx-version-min=14.0 -framework Security -framework CoreFoundation -o "$unlock" "$probe/scripts/keychain-unlock.c"
check $? "keychain-unlock.c builds"
gui() { "$probe/scripts/gui-run.sh" --timeout 30 -- "$@" 2>&1; }
gui /usr/bin/security lock-keychain "$dummy" >/dev/null

# Samples every process's arguments and environment; the pattern comes from
# a file, so the sampler never shows the password itself.
sample() {
  : > "$tmp/hits"; : > "$tmp/helper-seen"
  while [[ -f $tmp/sampling ]]; do
    snapshot=$(ps -axeww -o pid=,command= 2>/dev/null)
    print -r -- "$snapshot" | grep -cFf "$tmp/password" >> "$tmp/hits"
    print -r -- "$snapshot" | grep -c 'em-keychain-unlock' >> "$tmp/helper-seen"
    sleep 0.02
  done
}

# Positive control: a process that does carry the password in its arguments
# is caught.
touch "$tmp/sampling"; sample & sampler=$!
/bin/sh -c 'sleep 1; true' "$password"  # (no exec: sh stays in ps)
rm -f "$tmp/sampling"; wait $sampler
hits=$(awk '{ s += $1 } END { print s + 0 }' "$tmp/hits")
check $(( hits > 0 ? 0 : 1 )) "the sampler catches a password in a process's arguments ($hits hits)"

# The password reaches the helper through a FIFO, so the helper is certainly
# running (and sampled) while it waits for it.
mkfifo "$tmp/password.fifo"
touch "$tmp/sampling"; sample & sampler=$!
gui "$unlock" "$dummy" "$tmp/password.fifo" > "$tmp/unlock.out" &
runner=$!
sleep 1.5
cat "$tmp/password" > "$tmp/password.fifo"
wait $runner; check $? "the helper unlocks the throwaway keychain from a file"
rm -f "$tmp/sampling"; wait $sampler
hits=$(awk '{ s += $1 } END { print s + 0 }' "$tmp/hits"); seen=$(awk '$1 > 0' "$tmp/helper-seen" | wc -l | tr -d ' ')
check $(( seen > 0 ? 0 : 1 )) "the sampler saw the helper running ($seen samples)"
check $(( hits == 0 ? 0 : 1 )) "no process's arguments or environment showed the password while unlocking ($hits hits in $(wc -l < "$tmp/hits" | tr -d ' ') samples)"
[[ $(gui "$unlock" --status "$dummy") == unlocked ]]; check $? "the throwaway keychain is unlocked in the GUI session"

# A failed signing (no such identity) must leave it locked.
cp /usr/bin/true "$tmp/target"
touch "$tmp/sampling"; sample & sampler=$!
EM_SIGN_KEYCHAIN=$dummy EM_SIGN_PASSWORD_FILE=$tmp/password EM_SIGN_IDENTITY=0000000000000000000000000000000000000000 \
  "$probe/scripts/sign.sh" "$tmp/target" > "$tmp/sign.out" 2>&1
code=$?
rm -f "$tmp/sampling"; wait $sampler
hits=$(awk '{ s += $1 } END { print s + 0 }' "$tmp/hits")
check $(( code != 0 ? 0 : 1 )) "signing with a missing identity fails (exit $code)"
check $(( hits == 0 ? 0 : 1 )) "no process showed the password during sign.sh ($hits hits in $(wc -l < "$tmp/hits" | tr -d ' ') samples)"
grep -qF -f "$tmp/password" "$tmp/sign.out"; check $(( $? == 0 ? 1 : 0 )) "sign.sh's output never contains the password"
[[ $(gui "$unlock" --status "$dummy") == locked ]]; check $? "the keychain is locked again after the failure"

# A timed-out signing must leave it locked too: unlock it, then let sign.sh
# time out while its helper waits on a FIFO nobody writes.
gui "$unlock" "$dummy" "$tmp/password" >/dev/null
[[ $(gui "$unlock" --status "$dummy") == unlocked ]]; check $? "unlocked again before the timeout case"
mkfifo "$tmp/never.fifo"
EM_SIGN_KEYCHAIN=$dummy EM_SIGN_PASSWORD_FILE=$tmp/never.fifo EM_SIGN_IDENTITY=0000000000000000000000000000000000000000 \
  EM_SIGN_TIMEOUT=3 "$probe/scripts/sign.sh" "$tmp/target" > "$tmp/timeout.out" 2>&1
code=$?
check $(( code == 124 ? 0 : 1 )) "sign.sh times out (exit $code)"
[[ $(gui "$unlock" --status "$dummy") == locked ]]; check $? "the keychain is locked again after the timeout"
! ps -axww -o command= | grep -v grep | grep -qF "$tmp/never.fifo"; check $? "the timed-out helper was stopped"

security delete-keychain "$dummy" 2>/dev/null; dummy=
[[ $(security list-keychains -d user) == "$before" ]]; check $? "the user keychain search list is unchanged"

echo "failures=$failures"
(( failures == 0 ))
