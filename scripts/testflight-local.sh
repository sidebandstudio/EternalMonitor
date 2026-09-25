#!/usr/bin/env bash
# Archive with the selected Xcode; upload only when all signing inputs exist.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
MODE=auto
if [ "${1:-}" = --dry-run ]; then MODE=dry; shift; fi
if [ "$#" != 0 ]; then echo "Usage: $0 [--dry-run]" >&2; exit 2; fi
BUILD="${BUILD:-6}"
[[ "$BUILD" =~ ^[0-9]+$ ]] && [ "$BUILD" -gt 5 ] || { echo "BUILD must be an integer above 5" >&2; exit 2; }
OUT="${EM_TF_OUTPUT_DIR:-$ROOT/build/testflight}"
mkdir -p "$OUT"
rm -f "$OUT/result.json"
"$ROOT/scripts/check-versions.sh"
VERSION=$(python3 - "$ROOT/ios/project.yml" "${GITHUB_REF:-}" <<'PY'
import pathlib,re,sys
version=re.search(r'MARKETING_VERSION:\s*"([^"]+)"',pathlib.Path(sys.argv[1]).read_text())[1]
if sys.argv[2].startswith('refs/tags/'):
    tag=sys.argv[2].removeprefix('refs/tags/')
    if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?',tag) or tag[1:].split('-')[0]!=version:
        sys.exit(f'Tag {tag!r} does not match MARKETING_VERSION {version}')
print(version)
PY
)
TF_PRIVATE=""
TF_KEYCHAIN=""
TF_OLD_KEYCHAINS=()
cleanup() {
    status=$?
    trap - EXIT
    if [ -n "$TF_KEYCHAIN" ]; then
        security list-keychains -d user -s "${TF_OLD_KEYCHAINS[@]}"
        security delete-keychain "$TF_KEYCHAIN" || status=1
    fi
    if [ -n "$TF_PRIVATE" ]; then rm -rf "$TF_PRIVATE"; fi
    exit "$status"
}
trap cleanup EXIT
if [ "$MODE" = auto ]; then
    if [ -z "${ASC_KEY_ID:-}${ASC_ISSUER_ID:-}${ASC_KEY_P8:-}${ASC_KEY_PATH:-}" ]; then
        MODE=dry
    else
        : "${ASC_KEY_ID:?Set ASC_KEY_ID}" "${ASC_ISSUER_ID:?Set ASC_ISSUER_ID}"
        [[ "$ASC_KEY_ID" =~ ^[A-Za-z0-9]+$ ]] || { echo "Invalid ASC_KEY_ID" >&2; exit 2; }
        MODE=upload
    fi
fi
(cd "$ROOT/ios" && xcodegen generate)
ARCHIVE_ARGS=(-project "$ROOT/ios/EternalMonitor.xcodeproj" -scheme EternalMonitor
    -configuration Release -destination 'generic/platform=iOS'
    -archivePath "$OUT/EternalMonitor.xcarchive" -derivedDataPath "$OUT/DerivedData"
    "CURRENT_PROJECT_VERSION=$BUILD")
if [ "$MODE" = dry ]; then
    ARCHIVE_ARGS+=(CODE_SIGNING_ALLOWED=NO)
else
    SDK=$(xcrun --sdk iphoneos --show-sdk-version)
    [ "${SDK%%.*}" -ge 26 ] || { echo "App Store Connect uploads require the iOS 26 SDK or later; select Xcode 26+ with DEVELOPER_DIR" >&2; exit 2; }
    TF_PRIVATE=$(mktemp -d "${TMPDIR:-/tmp}/eternal-testflight.XXXXXX")
    chmod 700 "$TF_PRIVATE"
    if [ -n "${ASC_KEY_P8:-}" ]; then
        ASC_KEY_PATH="$TF_PRIVATE/AuthKey_$ASC_KEY_ID.p8"
        python3 - "$ASC_KEY_PATH" <<'PY'
import base64,os,pathlib,sys
path=pathlib.Path(sys.argv[1]);path.write_bytes(base64.b64decode(''.join(os.environ['ASC_KEY_P8'].split()),validate=True));path.chmod(0o600)
PY
    else
        ASC_KEY_PATH="${ASC_KEY_PATH:-$HOME/private_keys/AuthKey_$ASC_KEY_ID.p8}"
    fi
    [ -f "$ASC_KEY_PATH" ] || { echo "App Store Connect private key file is missing" >&2; exit 2; }
    AUTH=(-allowProvisioningUpdates -allowProvisioningDeviceRegistration
        -authenticationKeyPath "$ASC_KEY_PATH" -authenticationKeyID "$ASC_KEY_ID"
        -authenticationKeyIssuerID "$ASC_ISSUER_ID")
    ARCHIVE_ARGS+=("${AUTH[@]}")
    if [ -n "${IOS_DIST_P12_BASE64:-}" ]; then
        : "${IOS_DIST_P12_PASSWORD:?Set IOS_DIST_P12_PASSWORD for the fallback certificate}"
        python3 - "$TF_PRIVATE/distribution.p12" <<'PY'
import base64,os,pathlib,sys
path=pathlib.Path(sys.argv[1]);path.write_bytes(base64.b64decode(''.join(os.environ['IOS_DIST_P12_BASE64'].split()),validate=True));path.chmod(0o600)
PY
        while IFS= read -r chain; do TF_OLD_KEYCHAINS+=("$chain"); done < <(security list-keychains -d user | sed 's/^[[:space:]]*"//;s/"[[:space:]]*$//')
        TF_KEYCHAIN_PASSWORD=$(python3 -c 'import secrets;print(secrets.token_urlsafe(32))')
        TF_KEYCHAIN="$TF_PRIVATE/distribution.keychain-db"
        security create-keychain -p "$TF_KEYCHAIN_PASSWORD" "$TF_KEYCHAIN"
        security set-keychain-settings -lut 21600 "$TF_KEYCHAIN"
        security unlock-keychain -p "$TF_KEYCHAIN_PASSWORD" "$TF_KEYCHAIN"
        security import "$TF_PRIVATE/distribution.p12" -P "$IOS_DIST_P12_PASSWORD" -A -t cert -f pkcs12 -k "$TF_KEYCHAIN"
        security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$TF_KEYCHAIN_PASSWORD" "$TF_KEYCHAIN" >/dev/null
        security list-keychains -d user -s "$TF_KEYCHAIN" "${TF_OLD_KEYCHAINS[@]}"
        # Automatic archives use development signing. The imported distribution
        # identity is selected during export, when the app is signed for upload.
    fi
fi
echo "Archiving EternalMonitor $VERSION build $BUILD, mode=$MODE"
xcodebuild "${ARCHIVE_ARGS[@]}" archive > "$OUT/archive.log" 2>&1 || { tail -70 "$OUT/archive.log"; exit 1; }
python3 - "$OUT/EternalMonitor.xcarchive" "$VERSION" "$BUILD" <<'PY'
import pathlib,plistlib,sys
app=pathlib.Path(sys.argv[1])/'Products/Applications/EternalMonitor.app'
info=plistlib.loads((app/'Info.plist').read_bytes())
assert info['CFBundleShortVersionString']==sys.argv[2], info
assert info['CFBundleVersion']==sys.argv[3], info
assert info['UIDeviceFamily']==[2], info['UIDeviceFamily']
assert (app/'PrivacyInfo.xcprivacy').is_file(), 'Privacy manifest was not bundled'
print(f'Archive verified: {sys.argv[2]} ({sys.argv[3]})')
PY
if [ "$MODE" = upload ]; then
    xcodebuild -exportArchive -archivePath "$OUT/EternalMonitor.xcarchive" \
        -exportOptionsPlist "$ROOT/ios/exportOptions.plist" -exportPath "$OUT/export" \
        "${AUTH[@]}" > "$OUT/export.log" 2>&1 || { tail -70 "$OUT/export.log"; exit 1; }
    echo "Uploaded EternalMonitor $VERSION build $BUILD to App Store Connect"
else
    echo "Unsigned Release archive passed; upload skipped because this is a dry run"
fi
python3 - "$OUT/result.json" "$MODE" "$VERSION" "$BUILD" <<'PY'
import json,pathlib,sys
pathlib.Path(sys.argv[1]).write_text(json.dumps(dict(mode=sys.argv[2],version=sys.argv[3],build=int(sys.argv[4]),status='PASS'),indent=2)+'\n')
PY
