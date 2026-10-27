#!/usr/bin/env python3
"""Submit an uploaded TestFlight build for external beta review.

  testflight_review.py RESULT_JSON NOTES_MD REVIEW_JSON

After scripts/testflight-local.sh uploads a build (RESULT_JSON mode "upload"),
wait up to 30 minutes for App Store Connect to process it, set its en-US
What to Test to NOTES_MD, add it to every external group of the newest
approved build and submit it for beta review. Each step accepts work already
done, so a rerun finishes the job. Pull requests and dry runs skip it all.
REVIEW_JSON records what happened; it never holds a key or token.

Needs ASC_KEY_ID, ASC_ISSUER_ID and either ASC_KEY_P8 (the base64 .p8, as
testflight-local.sh takes it) or ASC_KEY_PATH.
"""
import base64
import binascii
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

API = "https://api.appstoreconnect.apple.com/v1"
APP_ID = "6774838098"  # com.eternal.monitor
LOCALE = "en-US"
PROCESSING_TIMEOUT = 30 * 60
POLL_SECONDS = 30
MAX_NOTES = 4000


class AscError(Exception):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=")


def der_to_raw(der):
    """openssl's DER ECDSA signature as the r || s pair a JWS carries."""
    def element(at, tag):
        if der[at] != tag:
            raise AscError("openssl returned an unexpected signature")
        length, at = der[at + 1], at + 2
        if length & 0x80:
            count = length & 0x7F
            length, at = int.from_bytes(der[at:at + count], "big"), at + count
        return at, at + length

    at, _ = element(0, 0x30)
    raw = b""
    for _ in range(2):
        start, at = element(at, 0x02)
        number = der[start:at].lstrip(b"\0")
        if len(number) > 32:
            raise AscError("openssl returned an unexpected signature")
        raw += number.rjust(32, b"\0")
    return raw


def openssl_sign(pem, message):
    """ES256 with the system openssl, so the runner needs no Python packages."""
    with tempfile.TemporaryDirectory(prefix="asc-") as private:
        key = pathlib.Path(private) / "key.p8"
        key.touch(mode=0o600)
        key.write_bytes(pem)
        signed = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(key)],
                                input=message, capture_output=True)
    if signed.returncode != 0 or not signed.stdout:
        raise AscError("openssl could not sign the App Store Connect token")
    return der_to_raw(signed.stdout)


def urllib_transport(method, url, headers, data):
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def key_from_env(env):
    key_id, issuer = env.get("ASC_KEY_ID", "").strip(), env.get("ASC_ISSUER_ID", "").strip()
    if not key_id or not issuer:
        raise AscError("Set ASC_KEY_ID and ASC_ISSUER_ID")
    if not re.fullmatch(r"[A-Za-z0-9]+", key_id):
        raise AscError("Invalid ASC_KEY_ID")
    p8 = env.get("ASC_KEY_P8", "").strip()
    if p8.startswith("-----BEGIN"):
        return key_id, issuer, p8.encode() + b"\n"
    if p8:
        try:
            return key_id, issuer, base64.b64decode("".join(p8.split()), validate=True)
        except binascii.Error:
            raise AscError("ASC_KEY_P8 is not base64") from None
    path = pathlib.Path(env.get("ASC_KEY_PATH") or pathlib.Path.home() / "private_keys" / f"AuthKey_{key_id}.p8")
    if not path.is_file():
        raise AscError("App Store Connect private key file is missing")
    return key_id, issuer, path.read_bytes()


class AppStoreConnect:
    def __init__(self, key_id, issuer, pem, transport=urllib_transport, sign=openssl_sign,
                 clock=time.time, sleep=time.sleep):
        self.key_id, self.issuer, self.pem = key_id, issuer, pem
        self.transport, self.sign, self.clock, self.sleep = transport, sign, clock, sleep
        self._token, self._expires = None, 0

    @classmethod
    def from_env(cls, env=os.environ, **kwargs):
        return cls(*key_from_env(env), **kwargs)

    def token(self):
        now = int(self.clock())
        if self._token is None or now > self._expires - 120:
            self._expires = now + 1200
            header = {"alg": "ES256", "kid": self.key_id, "typ": "JWT"}
            claims = {"iss": self.issuer, "iat": now, "exp": self._expires, "aud": "appstoreconnect-v1"}
            signing = b".".join(b64url(json.dumps(part, separators=(",", ":")).encode()) for part in (header, claims))
            self._token = (signing + b"." + b64url(self.sign(self.pem, signing))).decode()
        return self._token

    def call(self, method, path, params=None, body=None, ok=(200, 201, 204)):
        url = f"{API}/{path}" + ("?" + urllib.parse.urlencode(params) if params else "")
        data = json.dumps(body).encode() if body is not None else None
        for attempt in range(4):
            headers = {"Authorization": "Bearer " + self.token(), "Content-Type": "application/json"}
            try:
                status, raw = self.transport(method, url, headers, data)
            except OSError:
                status, raw = None, b""
            if status is not None and status < 500 and status != 429:
                break
            if attempt < 3:
                self.sleep(5 * 2 ** attempt)
        try:
            doc = json.loads(raw) if raw else {}
        except ValueError:
            doc = {}
        if status not in ok:
            problems = "; ".join(f"{e.get('code', '')}: {e.get('detail') or e.get('title', '')}"
                                 for e in doc.get("errors", []))[:400]
            raise AscError(f"{method} {path}: HTTP {status or 'unreachable'}" + (f" ({problems})" if problems else ""),
                           status)
        return doc


def related(doc, item, name):
    """The included resources an item's relationship points at."""
    data = item.get("relationships", {}).get(name, {}).get("data")
    refs = data if isinstance(data, list) else [data] if data else []
    pool = {(x["type"], x["id"]): x for x in doc.get("included", [])}
    return [pool.get((ref["type"], ref["id"]), ref) for ref in refs]


def find_build(asc, version, build):
    doc = asc.call("GET", "builds", {
        "filter[app]": APP_ID, "filter[version]": str(build), "filter[preReleaseVersion.version]": version,
        "include": "buildBetaDetail,betaAppReviewSubmission,betaGroups"})
    return (doc, doc["data"][0]) if doc.get("data") else None


def wait_until_processed(asc, version, build, timeout=PROCESSING_TIMEOUT, poll=POLL_SECONDS):
    deadline = asc.clock() + timeout
    while True:
        found, state = find_build(asc, version, build), "not found yet"
        if found:
            doc, item = found
            state = item["attributes"].get("processingState")
            detail = (related(doc, item, "buildBetaDetail") or [{}])[0].get("attributes", {})
            external = detail.get("externalBuildState")
            if state in ("FAILED", "INVALID"):
                raise AscError(f"App Store Connect could not process build {build}: {state}")
            if external in ("PROCESSING_EXCEPTION", "MISSING_EXPORT_COMPLIANCE", "EXPIRED", "BETA_REJECTED"):
                raise AscError(f"Build {build} cannot go to external testers: {external}")
            if state == "VALID" and external != "PROCESSING":
                return doc, item
        if asc.clock() >= deadline:
            raise AscError(f"Build {build} was not processed within {timeout // 60} minutes ({state})")
        print(f"Waiting for App Store Connect to process build {build}: {state}", flush=True)
        asc.sleep(poll)


def set_notes(asc, build_id, notes):
    doc = asc.call("GET", f"builds/{build_id}/betaBuildLocalizations",
                   {"fields[betaBuildLocalizations]": "locale,whatsNew", "limit": "50"})
    current = next((x for x in doc.get("data", []) if x["attributes"].get("locale") == LOCALE), None)
    if current and (current["attributes"].get("whatsNew") or "").strip() == notes.strip():
        return "already set"
    if current:
        asc.call("PATCH", f"betaBuildLocalizations/{current['id']}", body={"data": {
            "type": "betaBuildLocalizations", "id": current["id"], "attributes": {"whatsNew": notes}}})
    else:
        asc.call("POST", "betaBuildLocalizations", body={"data": {
            "type": "betaBuildLocalizations", "attributes": {"locale": LOCALE, "whatsNew": notes},
            "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
    return "set"


def approved_external_groups(asc, build_id):
    """The external groups of the newest other build Apple approved."""
    doc = asc.call("GET", "builds", {
        "filter[app]": APP_ID, "sort": "-uploadedDate", "limit": "30",
        "include": "betaAppReviewSubmission,betaGroups", "fields[betaGroups]": "name,isInternalGroup"})
    for item in doc.get("data", []):
        review = related(doc, item, "betaAppReviewSubmission")
        if item["id"] == build_id or not review or review[0].get("attributes", {}).get("betaReviewState") != "APPROVED":
            continue
        groups = [g for g in related(doc, item, "betaGroups") if g.get("attributes", {}).get("isInternalGroup") is False]
        if groups:
            return item["attributes"].get("version"), groups
    raise AscError("No earlier approved build is in an external TestFlight group to copy")


def add_to_groups(asc, doc, item, groups):
    member = {g["id"] for g in related(doc, item, "betaGroups")}
    added = {}
    for group in groups:
        name = group["attributes"]["name"]
        if group["id"] in member:
            added[name] = "already in group"
            continue
        asc.call("POST", f"betaGroups/{group['id']}/relationships/builds",
                 body={"data": [{"type": "builds", "id": item["id"]}]})
        added[name] = "added"
    return added


def submit_for_review(asc, doc, item):
    review = related(doc, item, "betaAppReviewSubmission")
    state = review[0].get("attributes", {}).get("betaReviewState") if review else None
    action = "already submitted"
    if not state:
        try:
            created = asc.call("POST", "betaAppReviewSubmissions", body={"data": {
                "type": "betaAppReviewSubmissions",
                "relationships": {"build": {"data": {"type": "builds", "id": item["id"]}}}}}, ok=(201,))
            action, state = "submitted", created["data"]["attributes"].get("betaReviewState")
        except AscError as error:
            if error.status != 409:
                raise
            existing = asc.call("GET", f"builds/{item['id']}/betaAppReviewSubmission", ok=(200, 404)).get("data")
            if not existing:
                raise
            state = existing["attributes"].get("betaReviewState")
    if state == "REJECTED":
        raise AscError("Apple rejected this build's beta review")
    return action, state


def run(result_path, notes_path, review, env=os.environ, connect=AppStoreConnect.from_env):
    result = json.loads(pathlib.Path(result_path).read_text())
    review.update(version=result.get("version"), build=result.get("build"))
    if env.get("GITHUB_EVENT_NAME") == "pull_request":
        review.update(status="SKIPPED", reason="pull requests never submit for review")
        return
    if result.get("mode") != "upload":
        review.update(status="SKIPPED", reason=f"build was not uploaded (mode {result.get('mode')})")
        return
    notes = pathlib.Path(notes_path).read_text()
    if not notes.strip() or len(notes) > MAX_NOTES:
        raise AscError(f"TestFlight notes must be 1 to {MAX_NOTES} characters")
    asc = connect(env)
    version, build = str(result["version"]), int(result["build"])
    doc, item = wait_until_processed(asc, version, build)
    review["processing"] = item["attributes"]["processingState"]
    review["whatsNew"] = set_notes(asc, item["id"], notes)
    review["groupsFrom"], groups = approved_external_groups(asc, item["id"])
    review["groups"] = add_to_groups(asc, doc, item, groups)
    review["review"], review["reviewState"] = submit_for_review(asc, doc, item)
    review["status"] = "PASS"


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    review, status = {}, 0
    try:
        run(*sys.argv[1:3], review)
    except Exception as error:  # every failure still writes REVIEW_JSON
        message = str(error) if isinstance(error, AscError) else f"{type(error).__name__}: {error}"
        review.update(status="FAIL", error=message)
        print(f"TestFlight review submission failed: {message}", file=sys.stderr)
        status = 1
    out = pathlib.Path(sys.argv[3])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(review, indent=2) + "\n")
    print(f"TestFlight review: {review.get('status')} "
          + ", ".join(f"{k}={v}" for k, v in review.items() if k not in ("status", "error")))
    return status


if __name__ == "__main__":
    sys.exit(main())
