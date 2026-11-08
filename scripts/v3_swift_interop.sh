#!/usr/bin/env bash
# The iPad's transport v3 core (ios/EternalTransportV3) against the Rust
# reference, end to end: builds and starts the Rust interop host
# (proto/examples/v3_interop_host.rs) on a loopback port, then runs the
# Swift package's tests with the host's port and test keys, so the live
# interop test runs alongside the golden-transcript, state-machine and
# fuzz-like tests.
#
#   scripts/v3_swift_interop.sh                    # swift test, on macOS
#   scripts/v3_swift_interop.sh --simulator UDID   # xcodebuild test, on an iOS simulator
#
# The host uses a random free port, never 9876 or 19873-19878. It exits once
# the test sends its final query; this script fails if it doesn't.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PACKAGE="$ROOT/ios/EternalTransportV3"
SIMULATOR=""
if [[ "${1:-}" == "--simulator" ]]; then
  SIMULATOR="${2:?--simulator needs a simulator UDID}"
fi

cargo build --quiet --manifest-path "$ROOT/proto/Cargo.toml" --features test-hooks --example v3_interop_host
HOST_BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/examples/v3_interop_host"

WORK="$(mktemp -d -t v3-interop)"
HOST_PID=""
cleanup() {
  if [[ -n "$HOST_PID" ]]; then kill "$HOST_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

"$HOST_BIN" >"$WORK/host.json" 2>"$WORK/host.log" &
HOST_PID=$!
for _ in $(seq 1 100); do
  [[ -s "$WORK/host.json" ]] && break
  sleep 0.1
done
[[ -s "$WORK/host.json" ]] || { echo "interop host didn't start"; cat "$WORK/host.log"; exit 1; }

# Test keys only: the host makes fresh random ones each run.
eval "$(python3 -I - "$WORK/host.json" <<'EOF'
import json, sys
c = json.load(open(sys.argv[1]))
names = {"port": "PORT", "host_id": "HOST_ID", "a_dev_id": "A_DEV_ID", "a_k_dev": "A_K_DEV",
         "b_dev_id": "B_DEV_ID", "b_k_dev": "B_K_DEV"}
for key, name in names.items():
    print(f"export EM_V3_INTEROP_{name}={c[key]}")
EOF
)"

if [[ -n "$SIMULATOR" ]]; then
  # xcodebuild hands TEST_RUNNER_-prefixed variables to the test process.
  for name in PORT HOST_ID A_DEV_ID A_K_DEV B_DEV_ID B_K_DEV; do
    var="EM_V3_INTEROP_$name"
    export "TEST_RUNNER_$var=${!var}"
  done
  (cd "$PACKAGE" && xcodebuild test -scheme EternalTransportV3 \
    -destination "platform=iOS Simulator,id=$SIMULATOR" \
    -derivedDataPath "${EM_DERIVED_DATA:-$WORK/derived}" -quiet)
else
  swift test --package-path "$PACKAGE"
fi

for _ in $(seq 1 50); do
  kill -0 "$HOST_PID" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$HOST_PID" 2>/dev/null; then
  echo "interop host still running: the live test never sent its final query"
  cat "$WORK/host.log"
  exit 1
fi
wait "$HOST_PID"
HOST_PID=""
cat "$WORK/host.log"
