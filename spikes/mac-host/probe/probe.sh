#!/bin/zsh
# Starts the Mac probe in the logged-in GUI session (Ali's, which Parsec
# shows) from any shell, waits for it, and prints its log.
#
#   probe.sh grant            first run: asks for Screen Recording and Accessibility, stays open
#   probe.sh status           prints the three permission states
#   probe.sh selftest         encoder + decoder without capture (no permissions needed)
#   probe.sh d|e              capture / events
#   probe.sh c                virtual display: only once Ali is connected and says go
#   probe.sh all              d, e, then c
# Extra flags pass through: --seconds N, --experimental, --h264, --no-tone.
set -euo pipefail
here=${0:A:h}
app="$here/build/EternalMonitor Mac Probe.app"
mode=${1:-grant}; shift || true
[[ -d $app ]] || { echo "build it first: $here/build.sh" >&2; exit 1; }
extra=()
[[ $mode == c || $mode == all ]] && extra+=(--confirm-display)
logs=$HOME/Library/Logs/EternalMonitorMacProbe
if [[ $mode == grant ]]; then
  # Stays open until Ali has switched both permissions on; don't wait for it.
  "$here/scripts/gui-run.sh" -- /usr/bin/open -n "$app" --args --mode grant
  sleep 3
  echo "probe is open in the GUI session; log: $logs/latest-grant.log"
  cat "$logs/latest-grant.log"
  exit 0
fi
"$here/scripts/gui-run.sh" --timeout 900 -- /usr/bin/open -W -n "$app" --args --mode "$mode" "${extra[@]}" "$@"
cat "$logs/latest-$mode.log"
grep -q "failures=0" "$logs/latest-$mode.log"
