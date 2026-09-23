#!/usr/bin/env bash
set -euo pipefail
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
if [ "$#" -ne 2 ]; then
    echo "Usage: $0 <simulator-udid> <output.png>" >&2
    exit 2
fi
mkdir -p "$(dirname "$2")"
xcrun simctl io "$1" screenshot "$2"
python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$2"
