#!/bin/zsh
# Starts the Mac probe in the logged-in GUI session (Ali's, which Parsec
# shows) from any shell, waits for it, and prints its log.
#
#   probe.sh grant            first run: asks for Screen Recording and Accessibility, stays open
#   probe.sh status           prints the three permission states
#   probe.sh selftest         encoder + decoder without capture (no permissions needed)
#   probe.sh d|e              capture / events
#   probe.sh c                virtual display: only once Ali is connected and says go
#   probe.sh all              c, d, then e
# Extra flags pass through: --seconds N, --experimental, --h264, --no-tone,
# --persist-arrangement, --deadline S. ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER=1
# adds --experimental (full-size HEVC at 120, informational).
#
# Every run has a hard deadline: the probe exits by itself when it passes,
# and this script waits 60 s longer. On timeout or interruption it stops the
# probe and its children by the PIDs they recorded (after checking each is
# still this probe's binary). After every run it reports whether any probe
# virtual display is still online, and stops its recorded owners if so.
set -euo pipefail
here=${0:A:h}
app="$here/build/EternalMonitor Mac Probe.app"
exe="$app/Contents/MacOS/EternalMonitorMacProbe"
mode=${1:-grant}; shift || true
[[ -d $app ]] || { echo "build it first: $here/build.sh" >&2; exit 1; }
extra=()
[[ $mode == c || $mode == all ]] && extra+=(--confirm-display)
[[ ${ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER:-} == 1 ]] && extra+=(--experimental)
# Same table as Options.hardDeadline in Sources/main.swift.
case $mode in
  grant) deadline=1800 ;; status) deadline=30 ;; selftest) deadline=120 ;;
  d) deadline=240 ;; e) deadline=120 ;; c) deadline=600 ;; all) deadline=900 ;; *) deadline=60 ;;
esac
args=("$@")
for (( i = 1; i < ${#args[@]}; i++ )); do
  [[ ${args[i]} == --deadline ]] && deadline=${args[i+1]}
done
logs=$HOME/Library/Logs/EternalMonitorMacProbe
run=$(mktemp -d /tmp/em-probe-run.XXXXXX)

# Stops one recorded PID if it is still this probe's binary: TERM (the
# probe restores the pointer and kills its children), then KILL after 3 s.
stop_recorded() {
  local pid=$1 command
  command=$(ps -p "$pid" -o command= 2>/dev/null) || return 0
  [[ $command == "$exe"* ]] || return 0
  echo "probe.sh: stopping pid $pid (${command#"$here/"})" >&2
  kill -TERM "$pid" 2>/dev/null || return 0
  for _ in {1..30}; do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.1; done
  kill -KILL "$pid" 2>/dev/null || true
}
stop_all() {
  local pid
  for pid in $(cat "$run/children" "$run/probe.pid" 2>/dev/null || true); do stop_recorded "$pid"; done
}
# Prints the probe's virtual displays that are online; returns 2 if it can't tell.
probe_displays() {
  local out
  out=$("$here/scripts/gui-run.sh" --timeout 30 -- "$exe" --mode displays 2>&1) || { echo "$out" >&2; return 2; }
  echo "$out" | grep ' probe=1$' || true
}
finish() {
  local result=$? left
  trap - EXIT
  [[ $mode == grant ]] || stop_all
  if ! left=$(probe_displays); then
    echo "probe.sh: FAIL could not list displays after the run" >&2
    result=1
  elif [[ -n $left ]]; then
    echo "probe.sh: probe virtual display(s) still online after the run:" >&2
    echo "$left" >&2
    stop_all
    sleep 2
    left=$(probe_displays) || true
    if [[ -n $left ]]; then
      echo "probe.sh: FAIL still online with no recorded owner running:" >&2
      echo "$left" >&2
    else
      echo "probe.sh: removed by stopping their recorded owners" >&2
    fi
    result=1
  else
    echo "probe.sh: no probe virtual display is online"
  fi
  rm -rf "$run"
  exit $result
}
trap finish EXIT
trap 'exit 130' INT TERM HUP

if [[ $mode == grant ]]; then
  # Stays open until Ali has switched both permissions on (its own deadline
  # is 30 min); don't wait for it.
  "$here/scripts/gui-run.sh" -- /usr/bin/open -n "$app" --args --mode grant --run-dir "$run" --deadline "$deadline"
  sleep 3
  echo "probe is open in the GUI session (pid $(cat "$run/probe.pid" 2>/dev/null || echo '?')); log: $logs/latest-grant.log"
  cat "$logs/latest-grant.log"
  exit 0
fi
"$here/scripts/gui-run.sh" --timeout $(( deadline + 60 )) -- /usr/bin/open -W -n "$app" --args \
  --mode "$mode" --run-dir "$run" --deadline "$deadline" "${extra[@]}" "$@"
cat "$logs/latest-$mode.log"
grep -q "failures=0" "$logs/latest-$mode.log"
