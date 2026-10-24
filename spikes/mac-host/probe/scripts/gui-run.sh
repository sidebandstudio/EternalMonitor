#!/bin/zsh
# Runs a command in the logged-in user's GUI (Aqua) session and waits for it.
#
# Shells started by T3 (and launchd's Background sessions generally) resolve
# the keychain search list in the System preference domain, so codesign
# cannot find a user-keychain identity there, and `launchctl asuser` needs
# root. A one-shot LaunchAgent bootstrapped into gui/<uid> runs in Aqua,
# where the user search list and its unlock state apply. See signing.md.
#
#   gui-run.sh [--timeout SECONDS] -- command [args...]
#
# Prints the command's combined output and exits with its status.
set -euo pipefail
timeout=120
if [[ ${1:-} == --timeout ]]; then timeout=$2; shift 2; fi
[[ ${1:-} == -- ]] && shift
(( $# > 0 )) || { echo "usage: gui-run.sh [--timeout S] -- command [args...]" >&2; exit 64; }

uid=$(id -u)
work=$(mktemp -d /tmp/em-gui-run.XXXXXX)
label="com.sidebandstudio.em-gui-run.${work##*.}"
trap 'launchctl bootout "gui/$uid/$label" 2>/dev/null || true; rm -rf "$work"' EXIT

{
  echo '#!/bin/zsh'
  printf 'cd %q\n' "$PWD"
  printf '%q ' "$@"; echo ' > "$1/out" 2>&1'
  echo 'echo $? > "$1/status"'
} > "$work/run.sh"
chmod 700 "$work/run.sh"
cat > "$work/agent.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key><array><string>$work/run.sh</string><string>$work</string></array>
  <key>RunAtLoad</key><true/>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
</dict></plist>
PLIST

launchctl bootstrap "gui/$uid" "$work/agent.plist"
deadline=$(( SECONDS + timeout ))
until [[ -f $work/status ]]; do
  if (( SECONDS >= deadline )); then
    echo "gui-run: timed out after ${timeout}s" >&2
    cat "$work/out" 2>/dev/null || true
    exit 124
  fi
  sleep 0.2
done
cat "$work/out"
exit "$(cat "$work/status")"
