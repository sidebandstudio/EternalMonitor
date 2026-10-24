#!/usr/bin/env python3
"""Before a Duo upload, tell whether the app changed since the newest build
and choose its build number. After it: wait for App Store
Connect to process the build, set its What to Test, and check that the Duo
internal group can install it.

Touches only the "EternalMonitor Duo Beta" app and its "Duo internal" group.
Prints no secrets. Needs PyJWT with cryptography, and ASC_KEY_ID,
ASC_ISSUER_ID and ASC_KEY_PATH. This file lives only on feat/iphone-duo.
"""
import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

import jwt

APP_ID = "6826678258"
GROUP_ID = "6958e9f2-bb61-42f5-905b-5f810200cc0f"
API = "https://api.appstoreconnect.apple.com/v1"
WHAT_TO_TEST = """Thanks for testing EternalMonitor on iPhone Duo. Please try:
- Fit and Fill, on the inner and the outer screen
- Folding and unfolding while a stream is running
- Apple Pencil, once iOS 27.2 is installed
- A USB connection
- Pairing with the phone in landscape
- Settings > Diagnostics > Copy, then send us the text so we can read the [pencil] lines
"""
# A build in a group with every build counts as in beta testing at once.
READY ={"READY_FOR_BETA_TESTING", "IN_BETA_TESTING"}


def token():
    now = int(time.time())
    key = pathlib.Path(os.environ["ASC_KEY_PATH"]).read_text()
    return jwt.encode({"iss": os.environ["ASC_ISSUER_ID"], "iat": now, "exp": now + 900,
                       "aud": "appstoreconnect-v1"}, key, algorithm="ES256",
                      headers={"kid": os.environ["ASC_KEY_ID"], "typ": "JWT"})


def call(method, path, body=None):
    request = urllib.request.Request(API + path, method=method,
                                     data=json.dumps(body).encode() if body else None,
                                     headers={"Authorization": "Bearer " + token(),
                                              "Content-Type": "application/json"})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.status, json.loads(response.read() or b"{}")
        except urllib.error.HTTPError as error:
            if error.code < 500 or attempt == 2:
                return error.code, json.loads(error.read() or b"{}")
        except urllib.error.URLError:
            if attempt == 2:
                raise
        time.sleep(10)


def errors(data):
    return "; ".join(f"{e.get('code')}: {e.get('detail')}" for e in data.get("errors", []))


def listed_numbers(path, field):
    """Every build number in a paginated App Store Connect listing."""
    numbers = []
    while path:
        status, data = call("GET", path)
        if status != 200:
            sys.exit(f"Listing {path.split('?')[0]} failed ({status}): {errors(data)}")
        numbers += [int(found["attributes"][field].split(".")[0]) for found in data["data"]]
        following = data.get("links", {}).get("next")
        path = following[len(API):] if following else None
    return numbers


def next_build():
    """One above the highest build number the Duo app has used, in any
    version, so a re-run of an older run can't go below a newer upload.

    An upload record exists from the moment xcodebuild starts sending, while
    its build reaches /builds only once processing begins. A run stopped in
    between would leave a number that /builds doesn't show yet, so the upload
    records count too, failed ones included."""
    builds = listed_numbers(f"/builds?filter[app]={APP_ID}&fields[builds]=version&limit=200", "version")
    uploads = listed_numbers(f"/apps/{APP_ID}/buildUploads?limit=200", "cfBundleVersion")
    return max(builds + uploads + [0]) + 1


def what_to_test(commit, xcode):
    return WHAT_TO_TEST + f"\nBuilt from {commit} with Xcode {xcode}.\n"


def uploaded_source():
    """The commit and Xcode build of the newest Duo build, from its What to
    Test, or None when the newest number has no processed build that names
    them (an upload is still processing, failed, or was stopped)."""
    newest = next_build() - 1
    status, data = call("GET", f"/builds?filter[app]={APP_ID}&filter[version]={newest}&limit=5")
    if status != 200:
        sys.exit(f"Build lookup failed ({status}): {errors(data)}")
    valid = [b for b in data["data"] if b["attributes"]["processingState"] == "VALID"]
    if len(valid) != 1 or len(data["data"]) != 1:
        return None
    status, data = call("GET", f"/builds/{valid[0]['id']}/betaBuildLocalizations")
    if status != 200:
        sys.exit(f"Reading What to Test failed ({status}): {errors(data)}")
    notes = next((l["attributes"]["whatsNew"] or "" for l in data["data"] if l["attributes"]["locale"] == "en-US"), "")
    found = re.search(r"^Built from ([0-9a-f]{40}) with Xcode (\w+)\.$", notes, re.M)
    return found and (found[1], found[2])


# Paths that never reach the app bundle. Anything else counts as an app
# change, so a path this list doesn't know about still uploads.
NOT_APP = re.compile(r"^(\.github/|scripts/|docs/|notes/|host/|installer/|updater/|proto/"
                     r"|ios/EternalMonitorTests/|ios/EternalMonitorUITests/"
                     r"|Cargo\.(toml|lock)$|rust-toolchain\.toml$|LICENSE$|\.gitignore$)"
                     # The app bundles whatever its source folder holds.
                     r"|^(?!ios/EternalMonitor/).*\.md$")


def app_changes(commit, xcode):
    """Why HEAD's app may differ from the newest upload, or nothing when only
    CI, scripts, docs, tests or the PC side changed."""
    source = uploaded_source()
    if not source:
        return "the newest Duo build doesn't name the commit it came from"
    if source[1] != xcode:
        return f"the newest Duo build used Xcode {source[1]}, not {xcode}"
    diff = subprocess.run(["git", "diff", "--name-only", source[0], commit], capture_output=True, text=True)
    if diff.returncode:
        return f"commit {source[0]} of the newest Duo build can't be compared here"
    changed = [path for path in diff.stdout.splitlines() if not NOT_APP.search(path)]
    if changed:
        return f"{len(changed)} app files changed since {source[0][:7]}, such as {changed[0]}"
    return None


def find_build(version, build):
    status, data = call("GET", f"/builds?filter[app]={APP_ID}&filter[version]={build}"
                        f"&filter[preReleaseVersion.version]={version}&include=buildBetaDetail&limit=5")
    if status != 200:
        sys.exit(f"Build lookup failed ({status}): {errors(data)}")
    detail = next((i for i in data.get("included", []) if i["type"] == "buildBetaDetails"), None)
    return (data["data"][0] if data["data"] else None), detail


def wait_for_processing(version, build, deadline):
    last = None
    while True:
        found, detail = find_build(version, build)
        state = found and found["attributes"]["processingState"]
        if state != last:
            print(f"Build {version} ({build}): {state or 'not visible yet'}", flush=True)
            last = state
        if state == "VALID":
            return found
        if state in {"FAILED", "INVALID"}:
            sys.exit(f"App Store Connect could not process build {build}: {state}")
        if time.monotonic() > deadline:
            sys.exit(f"Build {build} was still {state or 'not visible'} at the deadline")
        time.sleep(30)


def set_what_to_test(build_id, notes):
    status, data = call("GET", f"/builds/{build_id}/betaBuildLocalizations")
    if status != 200:
        sys.exit(f"Reading What to Test failed ({status}): {errors(data)}")
    current = next((l for l in data["data"] if l["attributes"]["locale"] == "en-US"), None)
    if current:
        status, data = call("PATCH", f"/betaBuildLocalizations/{current['id']}", {"data": {
            "type": "betaBuildLocalizations", "id": current["id"],
            "attributes": {"whatsNew": notes}}})
    else:
        status, data = call("POST", "/betaBuildLocalizations", {"data": {
            "type": "betaBuildLocalizations",
            "attributes": {"locale": "en-US", "whatsNew": notes},
            "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
    if status not in (200, 201):
        sys.exit(f"Setting What to Test failed ({status}): {errors(data)}")
    print("What to Test (en-US) set")


def group_has(build_id):
    status, data = call("GET", f"/betaGroups/{GROUP_ID}/builds?limit=200")
    if status != 200:
        sys.exit(f"Reading the Duo internal group failed ({status}): {errors(data)}")
    return any(b["id"] == build_id for b in data["data"])


def ensure_in_group(build_id):
    if group_has(build_id):
        print("Duo internal lists the build")
        return True
    status, data = call("POST", f"/betaGroups/{GROUP_ID}/relationships/builds",
                        {"data": [{"type": "builds", "id": build_id}]})
    if status not in (200, 204):
        print(f"Adding the build to Duo internal returned {status}: {errors(data)}")
    listed = group_has(build_id)
    print("Duo internal lists the build" if listed else "Duo internal does not list the build")
    return listed


def wait_for_beta_state(version, build, deadline):
    last = None
    while True:
        _, detail = find_build(version, build)
        state = detail and detail["attributes"].get("internalBuildState")
        if state != last:
            print(f"Internal testing state: {state}", flush=True)
            last = state
        if state in READY:
            return state
        if state in {"PROCESSING_EXCEPTION", "MISSING_EXPORT_COMPLIANCE", "EXPIRED"}:
            sys.exit(f"Testers can't install build {build}: {state}")
        if time.monotonic() > deadline:
            sys.exit(f"Build {build} was still {state} for internal testers at the deadline")
        time.sleep(30)


def testers():
    status, data = call("GET", f"/betaGroups/{GROUP_ID}/betaTesters?limit=50")
    if status != 200:
        sys.exit(f"Reading the Duo internal testers failed ({status}): {errors(data)}")
    return data["data"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--next-build", action="store_true", help="print the build number to upload next")
    parser.add_argument("--app-changes", action="store_true",
                        help="print why --commit needs a new upload, or nothing when it doesn't")
    parser.add_argument("--commit", help="the full commit being built")
    parser.add_argument("--xcode", help="the Xcode build doing it, such as 27A9275")
    parser.add_argument("--version")
    parser.add_argument("--build")
    parser.add_argument("--timeout-minutes", type=float, default=60)
    parser.add_argument("--result")
    args = parser.parse_args()
    if args.next_build:
        print(next_build())
        return
    if not args.commit or not re.fullmatch(r"[0-9a-f]{40}", args.commit) or not args.xcode:
        parser.error("--commit (a full commit) and --xcode are required")
    if args.app_changes:
        print(app_changes(args.commit, args.xcode) or "")
        return
    if not args.version or not args.build:
        parser.error("--version and --build are required")
    deadline = time.monotonic() + args.timeout_minutes * 60
    found = wait_for_processing(args.version, args.build, deadline)
    set_what_to_test(found["id"], what_to_test(args.commit, args.xcode))
    listed = ensure_in_group(found["id"])
    state = wait_for_beta_state(args.version, args.build, deadline)
    # States only, never names or addresses.
    tester_states = [t["attributes"].get("state") for t in testers()]
    print(f"Duo internal testers: {len(tester_states)}, states {tester_states}")
    result = dict(build_id=found["id"], version=args.version, build=args.build,
                  processing_state=found["attributes"]["processingState"],
                  internal_build_state=state, in_duo_internal=listed,
                  minimum_os=found["attributes"].get("minOsVersion"), tester_states=tester_states)
    if args.result:
        pathlib.Path(args.result).write_text(json.dumps(result, indent=2) + "\n")
    if not listed:
        sys.exit("The build is not in Duo internal")


if __name__ == "__main__":
    main()
