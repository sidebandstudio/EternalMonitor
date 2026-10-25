#!/bin/zsh
# Starts, stops or checks target/macos/EternalMonitor.app in the logged-in
# GUI session, from any shell (including one in launchd's Background
# session, through scripts/gui-run.sh).
#
#   packaging/macos/run-app.sh start    open the app (LaunchServices, so its own grants apply)
#   packaging/macos/run-app.sh stop     SIGTERM: the window closes and the host's teardown runs
#   packaging/macos/run-app.sh status   its PID, settings and the end of its log
#
# Only the process running this bundle's executable is ever signalled.
set -euo pipefail
here=${0:A:h}
app=${EM_APP:-${here:h:h}/target/macos/EternalMonitor.app}
app=${app:A}
exe=$app/Contents/MacOS/EternalMonitor
data=$HOME/Library/Application\ Support/EternalMonitor

# PIDs whose executable is this bundle's.
app_pids() {
  ps -axo pid=,command= | while read -r pid command; do
    [[ $command == "$exe" || $command == "$exe "* ]] && echo $pid
  done
  return 0
}

latest_log() {
  ls -t "$data"/logs/*.log 2>/dev/null | head -1
}

show_status() {
  local pids log
  pids=($(app_pids))
  if (( ${#pids} )); then echo "running: pid ${pids[*]}"; else echo "not running"; fi
  if [[ -f $data/settings.json ]]; then
    echo "second screen: $(plutil -extract mac_second_screen raw -o - "$data/settings.json" 2>/dev/null || echo off)"
  else
    echo "second screen: off (no settings yet)"
  fi
  log=$(latest_log)
  [[ -n $log ]] && { echo "log: $log"; tail -n 15 "$log"; }
  return 0
}

case ${1:-status} in
  start)
    [[ -d $app ]] || { echo "build it first: $here/build-app.sh" >&2; exit 1; }
    if [[ -n $(app_pids) ]]; then
      echo "already running: pid $(app_pids | tr '\n' ' ')"
      exit 0
    fi
    "$here/scripts/gui-run.sh" --timeout 30 -- /usr/bin/open "$app"
    for _ in {1..50}; do [[ -n $(app_pids) ]] && break; sleep 0.2; done
    sleep 3
    show_status
    ;;
  stop)
    pids=($(app_pids))
    (( ${#pids} )) || { echo "not running"; exit 0; }
    kill -TERM $pids
    for _ in {1..100}; do [[ -z $(app_pids) ]] && break; sleep 0.1; done
    if [[ -n $(app_pids) ]]; then
      echo "still running 10 s after SIGTERM; stopping pid ${pids[*]}" >&2
      kill -KILL $pids 2>/dev/null || true
      exit 1
    fi
    echo "stopped"
    show_status
    ;;
  status) show_status ;;
  *) echo "usage: run-app.sh start|stop|status" >&2; exit 64 ;;
esac
