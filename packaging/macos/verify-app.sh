#!/bin/zsh
# Checks a built EternalMonitor.app without running it:
#   - the signature, deep and strict, and the designated requirement;
#   - the hardened runtime and team on the app and on every bundled dylib;
#   - no entitlements;
#   - no Homebrew (/opt/homebrew) path in any load command or run path;
#   - the Info.plist keys the host needs, and that LSMinimumSystemVersion
#     is no older than the newest minos of any Mach-O file in the app.
#
#   packaging/macos/verify-app.sh [APP]
set -euo pipefail
here=${0:A:h}
app=${1:-${here:h:h}/target/macos/EternalMonitor.app}
app=${app:A}
bundle_id=com.sidebandstudio.eternalmonitor.machost
team=9X79V37Q89
homebrew=/opt/homebrew
failures=0
fail() { echo "FAIL $*" >&2; failures=$(( failures + 1 )); }
ok() { echo "ok   $*"; }

if signature=$(codesign --verify --deep --strict --verbose=2 "$app" 2>&1); then
  ok "codesign --verify --deep --strict"
else
  fail "codesign --verify --deep --strict"
fi
print -r -- "$signature" | grep -v -e '--prepared:' -e '--validated:' | sed 's/^/     /'
requirement=$(codesign -d -r- "$app" 2>&1 | sed -n 's/^designated => //p')
echo "     designated => $requirement"
[[ $requirement == *"identifier \"$bundle_id\""* && $requirement == *"anchor apple generic"* ]] ||
  fail "designated requirement does not pin $bundle_id to an Apple-issued certificate"
codesign --verify -R="identifier \"$bundle_id\" and anchor apple generic and certificate leaf[subject.OU] = \"$team\"" "$app" &&
  ok "satisfies: $bundle_id, team $team" || fail "does not satisfy $bundle_id, team $team"
entitlements=$(codesign -d --entitlements - --xml "$app" 2>/dev/null | plutil -convert json -o - - 2>/dev/null || echo '{}')
[[ $entitlements == '{}' || -z $entitlements ]] && ok "no entitlements" || fail "entitlements: $entitlements"

machos=("$app/Contents/MacOS/EternalMonitor" "$app"/Contents/Frameworks/*.dylib(N))
for file in $machos; do
  details=$(codesign -dvv "$file" 2>&1)
  [[ $details == *"TeamIdentifier=$team"* ]] || fail "${file:t}: not signed by team $team"
  [[ $details == *"(runtime)"* ]] || fail "${file:t}: no hardened runtime"
  if otool -L "$file" | tail -n +2 | grep -q "$homebrew"; then
    fail "${file:t} links Homebrew: $(otool -L "$file" | grep "$homebrew" | head -3 | tr -s ' \t' ' ')"
  fi
  if otool -l "$file" | grep -A2 LC_RPATH | grep -q "$homebrew"; then
    fail "${file:t} has a Homebrew run path"
  fi
done
ok "${#machos} Mach-O files: team $team, hardened runtime, checked for Homebrew paths"

plist=$app/Contents/Info.plist
for key in CFBundleIdentifier CFBundleShortVersionString LSMinimumSystemVersion NSHighResolutionCapable \
  NSScreenCaptureUsageDescription NSLocalNetworkUsageDescription; do
  value=$(plutil -extract $key raw -o - "$plist" 2>/dev/null) || { fail "Info.plist has no $key"; continue; }
  echo "     $key = $value"
done
echo "     NSBonjourServices = $(plutil -extract NSBonjourServices json -o - "$plist" 2>/dev/null)"
[[ $(plutil -extract CFBundleIdentifier raw -o - "$plist") == "$bundle_id" ]] || fail "wrong bundle ID"
declared=$(plutil -extract LSMinimumSystemVersion raw -o - "$plist")
needed=$("$here/scripts/min-macos.sh" "$app")
if [[ $(print -l "$declared" "$needed" | sort -V | tail -1) == "$declared" ]]; then
  ok "LSMinimumSystemVersion $declared covers every Mach-O file (newest needs $needed)"
else
  fail "LSMinimumSystemVersion is $declared, but bundled code needs macOS $needed: $("$here/scripts/min-macos.sh" --each "$app" | sort -V | tail -3 | tr '\n' ' ')"
fi
[[ $(plutil -extract NSBonjourServices json -o - "$plist") == '["_eternaldisplay._udp"]' ]] ||
  fail "NSBonjourServices does not match discovery.rs"

(( failures == 0 )) && echo "verify-app.sh: all checks passed" || { echo "verify-app.sh: $failures failure(s)" >&2; exit 1; }
