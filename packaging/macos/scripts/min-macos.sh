#!/bin/zsh
# Prints the newest macOS any Mach-O file in an app requires: the highest
# minos (LC_BUILD_VERSION, or LC_VERSION_MIN_MACOSX in older files) over the
# executable and every bundled dylib. With --each, prints "version file" for
# each one instead.
#   min-macos.sh [--each] APP
set -euo pipefail
each=0
[[ ${1:-} == --each ]] && { each=1; shift; }
app=${1:?usage: min-macos.sh [--each] APP}
versions=()
for file in "$app"/Contents/MacOS/* "$app"/Contents/Frameworks/*.dylib(N); do
  version=$(otool -l "$file" | awk '
    /cmd LC_BUILD_VERSION/ || /cmd LC_VERSION_MIN_MACOSX/ { found = 1 }
    found && ($1 == "minos" || $1 == "version") { print $2; exit }')
  [[ -n $version ]] || { echo "min-macos.sh: no minimum macOS in $file" >&2; exit 1; }
  (( each )) && echo "$version ${file:t}"
  versions+=$version
done
(( each )) || print -l $versions | sort -V | tail -1
