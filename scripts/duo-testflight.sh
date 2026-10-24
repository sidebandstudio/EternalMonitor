#!/usr/bin/env bash
# Archive the iPhone Duo test app (com.eternal.monitor.duo) and upload it to
# its own App Store Connect app, "EternalMonitor Duo Beta". Adapted from
# testflight-local.sh; this file lives only on feat/iphone-duo.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
MODE=auto
if [ "${1:-}" = --dry-run ]; then MODE=dry; shift; fi
if [ "$#" != 0 ]; then echo "Usage: $0 [--dry-run]" >&2; exit 2; fi
BUNDLE=com.eternal.monitor.duo
BUILD="${BUILD:?Set BUILD to a build number of the Duo app}"
[[ "$BUILD" =~ ^[1-9][0-9]*(\.[0-9]+){0,2}$ ]] || { echo "BUILD must be one to three integers, like 4 or 4.2" >&2; exit 2; }
# The Duo runs full screen only with the iOS 27.1 SDK; an older SDK shows a
# centred window. DUO_MIN_SDK lowers this for a fallback build.
MIN_SDK="${DUO_MIN_SDK:-27.1}"
# Empty keeps project.yml's deployment target.
DEPLOYMENT="${DUO_DEPLOYMENT_TARGET:-}"
OUT="${EM_TF_OUTPUT_DIR:-$ROOT/build/duo-testflight}"
mkdir -p "$OUT"
rm -f "$OUT/result.json"
"$ROOT/scripts/check-versions.sh"
VERSION=$(python3 - "$ROOT/ios/project.yml" <<'PY'
import pathlib,re,sys
print(re.search(r'MARKETING_VERSION:\s*"([^"]+)"',pathlib.Path(sys.argv[1]).read_text())[1])
PY
)
SDK=$(xcrun --sdk iphoneos --show-sdk-version)
python3 - "$SDK" "$MIN_SDK" <<'PY'
import sys
version=lambda text: tuple(int(part) for part in text.split('.'))
if version(sys.argv[1]) < version(sys.argv[2]):
    sys.exit(f'The iOS {sys.argv[1]} SDK is older than {sys.argv[2]}; select a newer Xcode with DEVELOPER_DIR')
PY
TF_PRIVATE=""
TF_KEYCHAIN=""
TF_PROFILE=""
EXPORT_OPTIONS="$ROOT/ios/exportOptions.plist"
TF_OLD_KEYCHAINS=()
cleanup() {
    status=$?
    trap - EXIT
    if [ -n "$TF_KEYCHAIN" ]; then
        security list-keychains -d user -s "${TF_OLD_KEYCHAINS[@]}"
        security delete-keychain "$TF_KEYCHAIN" || status=1
    fi
    if [ -n "$TF_PROFILE" ]; then rm -f "$TF_PROFILE"; fi
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
# Command-line settings apply to every target the archive builds; only the
# app reads its bundle ID, and the static Opus library ignores it.
ARCHIVE_ARGS=(-project "$ROOT/ios/EternalMonitor.xcodeproj" -scheme EternalMonitor
    -configuration Release -destination 'generic/platform=iOS'
    -archivePath "$OUT/EternalMonitor.xcarchive" -derivedDataPath "$OUT/DerivedData"
    "PRODUCT_BUNDLE_IDENTIFIER=$BUNDLE" "CURRENT_PROJECT_VERSION=$BUILD")
if [ -n "$DEPLOYMENT" ]; then ARCHIVE_ARGS+=("IPHONEOS_DEPLOYMENT_TARGET=$DEPLOYMENT"); fi
if [ "$MODE" = dry ]; then
    ARCHIVE_ARGS+=(CODE_SIGNING_ALLOWED=NO)
else
    : "${ETERNAL_DIAGNOSTICS_ENDPOINT:?Set the automatic diagnostics HTTPS endpoint}" "${ETERNAL_DIAGNOSTICS_INGEST_KEY:?Set the write-only diagnostics ingest credential}"
    TF_PRIVATE=$(mktemp -d "${TMPDIR:-/tmp}/eternal-duo-testflight.XXXXXX")
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
        security find-identity -v -p codesigning "$TF_KEYCHAIN"
        # Automatic archives use development signing. The imported distribution
        # identity is selected during export, when the app is signed for upload.
    fi
    if [ -n "${IOS_DUO_PROVISION_PROFILE_BASE64:-}" ]; then
        : "${IOS_DIST_P12_BASE64:?A provisioning profile requires a distribution certificate}"
        python3 - "$TF_PRIVATE/profile.mobileprovision" <<'PYPROFILE'
import base64,os,pathlib,sys
pathlib.Path(sys.argv[1]).write_bytes(base64.b64decode(''.join(os.environ['IOS_DUO_PROVISION_PROFILE_BASE64'].split()),validate=True))
PYPROFILE
        # The profile's plist is signed, not encrypted; read it without a keychain.
        TF_PROFILE=$(python3 - "$TF_PRIVATE/profile.mobileprovision" "$EXPORT_OPTIONS" "$TF_PRIVATE/exportOptions.plist" "$BUNDLE" <<'PYPROFILE'
import pathlib,plistlib,shutil,sys,uuid
raw=pathlib.Path(sys.argv[1]).read_bytes()
profile=plistlib.loads(raw[raw.index(b'<?xml'):raw.index(b'</plist>')+8])
options=plistlib.loads(pathlib.Path(sys.argv[2]).read_bytes())
team=options['teamID']; bundle=sys.argv[4]
assert profile['TeamIdentifier']==[team], 'Wrong profile team'
assert profile['Entitlements']['application-identifier']==team+'.'+bundle, 'Wrong profile app'
assert not profile['Entitlements']['get-task-allow'], 'Distribution profile required'
assert 'ProvisionedDevices' not in profile, 'App Store profile required'
print('Profile:', profile['Name'], profile['UUID'], 'expires', profile['ExpirationDate'].date(), file=sys.stderr)
# A unique installed filename allows cleanup without removing existing profiles.
folder=pathlib.Path.home()/'Library/Developer/Xcode/UserData/Provisioning Profiles'
folder.mkdir(parents=True,exist_ok=True)
installed=folder/('eternal-duo-testflight-'+str(uuid.uuid4())+'.mobileprovision')
shutil.copyfile(sys.argv[1],installed)
options.update(signingStyle='manual',signingCertificate='Apple Distribution',provisioningProfiles={bundle:profile['UUID']})
pathlib.Path(sys.argv[3]).write_bytes(plistlib.dumps(options))
print(installed)
PYPROFILE
)
        EXPORT_OPTIONS="$TF_PRIVATE/exportOptions.plist"
    fi
fi
if [ -n "${ETERNAL_DIAGNOSTICS_ENDPOINT:-}" ] || [ -n "${ETERNAL_DIAGNOSTICS_INGEST_KEY:-}" ]; then
    : "${ETERNAL_DIAGNOSTICS_ENDPOINT:?Set both diagnostics build settings}" "${ETERNAL_DIAGNOSTICS_INGEST_KEY:?Set both diagnostics build settings}"
    ARCHIVE_ARGS+=("ETERNAL_DIAGNOSTICS_ENDPOINT=$ETERNAL_DIAGNOSTICS_ENDPOINT" "ETERNAL_DIAGNOSTICS_INGEST_KEY=$ETERNAL_DIAGNOSTICS_INGEST_KEY")
fi
xcodebuild -version
echo "Archiving EternalMonitor Duo $VERSION build $BUILD with the iOS $SDK SDK, mode=$MODE"
xcodebuild "${ARCHIVE_ARGS[@]}" archive > "$OUT/archive.log" 2>&1 || { tail -70 "$OUT/archive.log"; exit 1; }
# Show any deployment target complaint from the new SDK in the job log.
grep -i 'deployment target' "$OUT/archive.log" | sort -u || true
python3 - "$OUT/EternalMonitor.xcarchive" "$VERSION" "$BUILD" "$BUNDLE" "$OUT/archive-info.json" <<'PY'
import json,os,pathlib,plistlib,sys
app=pathlib.Path(sys.argv[1])/'Products/Applications/EternalMonitor.app'
info=plistlib.loads((app/'Info.plist').read_bytes())
assert info['CFBundleIdentifier']==sys.argv[4], info['CFBundleIdentifier']
assert info['CFBundleShortVersionString']==sys.argv[2], info
assert info['CFBundleVersion']==sys.argv[3], info
assert info['UIDeviceFamily']==[1,2], info['UIDeviceFamily']
assert (app/'PrivacyInfo.xcprivacy').is_file(), 'Privacy manifest was not bundled'
if os.environ.get('ETERNAL_DIAGNOSTICS_ENDPOINT'):
    assert info.get('DiagnosticsEndpoint') == os.environ['ETERNAL_DIAGNOSTICS_ENDPOINT'], 'Diagnostics endpoint was not bundled'
    assert info.get('DiagnosticsIngestKey') == os.environ['ETERNAL_DIAGNOSTICS_INGEST_KEY'], 'Diagnostics ingest credential was not bundled'
facts={key:info.get(key) for key in ['CFBundleIdentifier','CFBundleVersion','MinimumOSVersion','DTSDKName','DTXcode','DTXcodeBuild']}
pathlib.Path(sys.argv[5]).write_text(json.dumps(facts,indent=2)+'\n')
print('Archive verified:', ', '.join(f'{k}={v}' for k,v in facts.items()))
PY
if [ "$MODE" = upload ]; then
    xcodebuild -exportArchive -archivePath "$OUT/EternalMonitor.xcarchive" \
        -exportOptionsPlist "$EXPORT_OPTIONS" -exportPath "$OUT/export" \
        "${AUTH[@]}" > "$OUT/export.log" 2>&1 || { tail -70 "$OUT/export.log"; exit 1; }
    echo "Uploaded EternalMonitor Duo $VERSION build $BUILD to App Store Connect"
    # Wait for processing, set What to Test, and check the Duo internal group.
    ASC_KEY_PATH="$ASC_KEY_PATH" "${DUO_PYTHON:-python3}" "$ROOT/scripts/duo_testflight_build.py" \
        --version "$VERSION" --build "$BUILD" --result "$OUT/asc.json"
else
    echo "Unsigned Release archive passed; upload skipped because this is a dry run"
fi
python3 - "$OUT/result.json" "$MODE" "$VERSION" "$BUILD" "$OUT/archive-info.json" "$OUT/asc.json" <<'PY'
import json,pathlib,sys
result=dict(mode=sys.argv[2],version=sys.argv[3],build=sys.argv[4],status='PASS')
result['archive']=json.loads(pathlib.Path(sys.argv[5]).read_text())
if pathlib.Path(sys.argv[6]).exists(): result['app_store_connect']=json.loads(pathlib.Path(sys.argv[6]).read_text())
pathlib.Path(sys.argv[1]).write_text(json.dumps(result,indent=2)+'\n')
PY
