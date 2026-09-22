#!/usr/bin/env bash
# Compile the unchanged pixel assertions once, before starting a measured stream.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
BINARY="$ROOT/build/px"
if [ ! -x "$BINARY" ] || [ "$ROOT/scripts/px.swift" -nt "$BINARY" ]; then
    mkdir -p "$ROOT/build"
    xcrun swiftc -O "$ROOT/scripts/px.swift" -o "$BINARY.tmp.$$"
    mv "$BINARY.tmp.$$" "$BINARY"
fi
if [ "${1:-}" != --prepare ]; then exec "$BINARY" "$@"; fi
