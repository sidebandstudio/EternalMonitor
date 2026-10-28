"""testflight_review.py must submit each uploaded build once, and finish reruns."""
import base64
import contextlib
import io
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest
import urllib.parse

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import testflight_review as tr  # noqa: E402

SCRIPT = pathlib.Path(tr.__file__)
NOTES = "# EternalMonitor 0.3.4: What to Test\n\nTry Second screen.\n"


class FakeAsc:
    """Enough of App Store Connect for one app, its groups and its builds."""

    def __init__(self):
        self.calls = []
        self.queued = {}
        self.page_size = None
        self.groups = {"g-int": ("Friends", True), "g-ext": ("FriendsExternal", False)}
        self.builds = {
            "b365": dict(version="365", states=["NOT_FOUND", "PROCESSING", "VALID"], external="READY_FOR_BETA_SUBMISSION",
                         groups=["g-int"], review=None, notes={"l1": None}),
            "b364": dict(version="364", states=["VALID"], external="WAITING_FOR_BETA_REVIEW",
                         groups=["g-int", "g-ext"], review="WAITING_FOR_REVIEW", notes={}),
            "b316": dict(version="316", states=["VALID"], external="IN_BETA_TESTING",
                         groups=["g-int", "g-ext"], review="APPROVED", notes={}),
        }

    def resource(self, build_id, doc):
        build = self.builds[build_id]
        detail = {"type": "buildBetaDetails", "id": build_id, "attributes": {"externalBuildState": build["external"]}}
        review = build["review"] and {"type": "betaAppReviewSubmissions", "id": build_id,
                                      "attributes": {"betaReviewState": build["review"]}}
        groups = [{"type": "betaGroups", "id": g, "attributes": {"name": self.groups[g][0],
                                                                 "isInternalGroup": self.groups[g][1]}}
                  for g in build["groups"]]
        doc.setdefault("included", []).extend([detail, *groups] + ([review] if review else []))
        ref = lambda x: {"type": x["type"], "id": x["id"]}  # noqa: E731
        return {"type": "builds", "id": build_id,
                "attributes": {"version": build["version"], "processingState": build["states"][0]},
                "relationships": {"buildBetaDetail": {"data": ref(detail)},
                                  "betaAppReviewSubmission": {"data": review and ref(review)},
                                  "betaGroups": {"data": [ref(g) for g in groups]}}}

    def __call__(self, method, url, headers, data):
        assert headers["Authorization"].startswith("Bearer ")
        parts = urllib.parse.urlsplit(url)
        path, query = parts.path.removeprefix("/v1/"), dict(urllib.parse.parse_qsl(parts.query))
        body = json.loads(data) if data else None
        self.calls.append((method, path, query, body))
        if self.queued.get((method, path)):
            return self.queued[(method, path)].pop(0), b'{"errors": [{"code": "QUEUED", "detail": "queued"}]}'
        status, doc = self.handle(method, path, query, body)
        return status, json.dumps(doc).encode() if doc is not None else b""

    def handle(self, method, path, query, body):
        if (method, path) == ("GET", "builds") and "filter[version]" in query:
            assert query["filter[app]"] == tr.APP_ID and query["filter[preReleaseVersion.version]"] == "0.3.4"
            build_id = next(k for k, b in self.builds.items() if b["version"] == query["filter[version]"])
            states = self.builds[build_id]["states"]
            if states[0] == "NOT_FOUND":
                states.pop(0)
                return 200, {"data": []}
            doc = {}
            doc["data"] = [self.resource(build_id, doc)]
            if len(states) > 1:
                states.pop(0)
            return 200, doc
        if (method, path) == ("GET", "builds"):
            assert query["sort"] == "-uploadedDate"
            size, start = self.page_size or int(query["limit"]), int(query.get("cursor", 0))
            page = list(self.builds)[start:start + size]
            doc = {}
            doc["data"] = [self.resource(build_id, doc) for build_id in page]
            if start + size < len(self.builds):
                doc["links"] = {"next": f"{tr.API}/builds?" + urllib.parse.urlencode({**query, "cursor": start + size})}
            return 200, doc
        if method == "GET" and path.endswith("/betaBuildLocalizations"):
            notes = self.builds[path.split("/")[1]]["notes"]
            return 200, {"data": [{"type": "betaBuildLocalizations", "id": k, "attributes": {"locale": "en-US", "whatsNew": v}}
                                  for k, v in notes.items()]}
        if method == "PATCH" and path.startswith("betaBuildLocalizations/"):
            self.builds["b365"]["notes"][path.split("/")[1]] = body["data"]["attributes"]["whatsNew"]
            return 200, {"data": body["data"]}
        if (method, path) == ("POST", "betaBuildLocalizations"):
            build = self.builds[body["data"]["relationships"]["build"]["data"]["id"]]
            build["notes"]["l2"] = body["data"]["attributes"]["whatsNew"]
            return 201, {"data": {"type": "betaBuildLocalizations", "id": "l2"}}
        if method == "POST" and path.startswith("betaGroups/"):
            for ref in body["data"]:
                self.builds[ref["id"]]["groups"].append(path.split("/")[1])
            return 204, None
        if (method, path) == ("POST", "betaAppReviewSubmissions"):
            build = self.builds[body["data"]["relationships"]["build"]["data"]["id"]]
            build["review"], build["external"] = "WAITING_FOR_REVIEW", "WAITING_FOR_BETA_REVIEW"
            return 201, {"data": {"type": "betaAppReviewSubmissions", "id": "b365",
                                  "attributes": {"betaReviewState": "WAITING_FOR_REVIEW"}}}
        if method == "GET" and path.endswith("/betaAppReviewSubmission"):
            build = self.builds[path.split("/")[1]]
            return 200, {"data": build["review"] and {"type": "betaAppReviewSubmissions", "id": "b365",
                                                      "attributes": {"betaReviewState": build["review"]}}}
        raise AssertionError(f"unexpected {method} {path}")

    def writes(self):
        return [(m, p) for m, p, _, _ in self.calls if m != "GET"]


class Clock:
    def __init__(self):
        self.now = 1_800_000_000.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class TestFlightReviewTest(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        self.result = self.tmp / "result.json"
        self.result.write_text(json.dumps({"mode": "upload", "version": "0.3.4", "build": 365, "status": "PASS"}))
        self.notes = self.tmp / "TESTFLIGHT_NOTES.md"
        self.notes.write_text(NOTES)
        self.asc, self.clock = FakeAsc(), Clock()

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def connect(self, env):
        return tr.AppStoreConnect("K1", "issuer", b"pem", transport=self.asc, sign=lambda pem, message: b"\1" * 64,
                                  clock=self.clock, sleep=self.clock.sleep)

    def run_review(self, event="push"):
        review = {}
        with contextlib.redirect_stdout(io.StringIO()):
            tr.run(self.result, self.notes, review, env={"GITHUB_EVENT_NAME": event}, connect=self.connect)
        return review

    def test_uploaded_build_is_noted_grouped_and_submitted(self):
        review = self.run_review()
        self.assertEqual(review["status"], "PASS")
        self.assertEqual((review["whatsNew"], review["groupsFrom"], review["review"], review["reviewState"]),
                         ("set", "316", "submitted", "WAITING_FOR_REVIEW"))
        self.assertEqual(review["groups"], {"FriendsExternal": "added"})
        build = self.asc.builds["b365"]
        self.assertEqual(build["notes"], {"l1": NOTES})
        self.assertIn("g-ext", build["groups"])
        self.assertEqual(self.asc.writes(), [("PATCH", "betaBuildLocalizations/l1"),
                                             ("POST", "betaGroups/g-ext/relationships/builds"),
                                             ("POST", "betaAppReviewSubmissions")])

    def test_rerun_finds_everything_done(self):
        self.run_review()
        self.asc.calls.clear()
        review = self.run_review()
        self.assertEqual(review["status"], "PASS")
        self.assertEqual((review["whatsNew"], review["review"]), ("already set", "already submitted"))
        self.assertEqual(review["groups"], {"FriendsExternal": "already in group"})
        self.assertEqual(self.asc.writes(), [])

    def test_groups_come_from_the_newest_approved_build(self):
        self.asc.groups["g-old"] = ("OldExternal", False)
        self.asc.builds["b316"]["groups"] = ["g-old"]
        review = self.run_review()
        self.assertEqual(review["groups"], {"OldExternal": "added"})

    def test_newest_approved_build_without_external_groups_stops_there(self):
        self.asc.groups["g-ret"] = ("RetiredExternal", False)
        self.asc.builds["b316"]["groups"] = ["g-int"]
        self.asc.builds["b300"] = dict(version="300", states=["VALID"], external="IN_BETA_TESTING",
                                       groups=["g-int", "g-ret"], review="APPROVED", notes={})
        review = self.run_review()
        self.assertEqual((review["status"], review["groups"], review["review"], review["groupsFrom"]),
                         ("NO_EXTERNAL_GROUP", {}, "not submitted", "316"))
        self.assertIn("no external group to copy", review["reason"])
        self.assertEqual(self.asc.writes(), [("PATCH", "betaBuildLocalizations/l1")])

    def test_approved_build_lookup_follows_pages(self):
        for n in range(5):
            self.asc.builds[f"b37{n}"] = dict(version=f"37{n}", states=["VALID"], external="BETA_REJECTED",
                                              groups=[], review="REJECTED", notes={})
        self.asc.builds = {k: self.asc.builds[k] for k in ["b365", "b370", "b371", "b372", "b373", "b374", "b364", "b316"]}
        self.asc.page_size = 2
        review = self.run_review()
        self.assertEqual((review["status"], review["groupsFrom"]), ("PASS", "316"))
        pages = [q for m, p, q, _ in self.asc.calls if (m, p) == ("GET", "builds") and "sort" in q]
        self.assertEqual([q.get("cursor") for q in pages], [None, "2", "4", "6"])

    def test_approved_build_lookup_is_bounded(self):
        self.asc.builds["b316"]["review"] = "REJECTED"
        for n in range(30):
            self.asc.builds[f"b2{n:02}"] = dict(version=f"2{n:02}", states=["VALID"], external="BETA_REJECTED",
                                                groups=[], review="REJECTED", notes={})
        self.asc.builds["b100"] = dict(version="100", states=["VALID"], external="IN_BETA_TESTING",
                                       groups=["g-ext"], review="APPROVED", notes={})
        self.asc.page_size = 3
        with self.assertRaisesRegex(tr.AscError, "newest 10 pages"):
            self.run_review()
        self.assertEqual(sum(1 for m, p, q, _ in self.asc.calls if (m, p) == ("GET", "builds") and "sort" in q), 10)

    def test_links_off_the_api_are_refused(self):
        with self.assertRaisesRegex(tr.AscError, "outside the App Store Connect API"):
            self.connect({}).call("GET", "https://example.com/v1/builds?cursor=2")
        self.assertEqual(self.asc.calls, [])

    def test_missing_localization_is_created(self):
        self.asc.builds["b365"]["notes"] = {}
        self.assertEqual(self.run_review()["whatsNew"], "set")
        self.assertEqual(self.asc.builds["b365"]["notes"], {"l2": NOTES})

    def test_conflicting_submission_that_exists_passes(self):
        self.asc.queued[("POST", "betaAppReviewSubmissions")] = [409]
        self.asc.builds["b365"]["review"] = None
        original = self.asc.handle

        def submitted_elsewhere(method, path, query, body):
            if method == "GET" and path.endswith("/betaAppReviewSubmission"):
                self.asc.builds["b365"]["review"] = "WAITING_FOR_REVIEW"
            return original(method, path, query, body)
        self.asc.handle = submitted_elsewhere
        review = self.run_review()
        self.assertEqual((review["review"], review["reviewState"]), ("already submitted", "WAITING_FOR_REVIEW"))

    def test_server_errors_are_retried(self):
        self.asc.queued[("POST", "betaAppReviewSubmissions")] = [503, 500]
        self.assertEqual(self.run_review()["review"], "submitted")

    def test_client_errors_fail_with_apple_detail(self):
        self.asc.queued[("POST", "betaAppReviewSubmissions")] = [422]
        with self.assertRaisesRegex(tr.AscError, r"HTTP 422 \(QUEUED: queued\)"):
            self.run_review()

    def test_processing_wait_is_bounded(self):
        self.asc.builds["b365"]["states"] = ["PROCESSING", "PROCESSING"]
        with self.assertRaisesRegex(tr.AscError, "not processed within 30 minutes"):
            self.run_review()
        self.assertGreaterEqual(self.clock.now - 1_800_000_000, tr.PROCESSING_TIMEOUT)
        self.assertEqual(self.asc.writes(), [])

    def test_failed_processing_stops(self):
        self.asc.builds["b365"]["states"] = ["INVALID"]
        with self.assertRaisesRegex(tr.AscError, "could not process build 365: INVALID"):
            self.run_review()

    def test_no_approved_external_group_fails(self):
        self.asc.builds["b316"]["review"] = "REJECTED"
        with self.assertRaisesRegex(tr.AscError, "No earlier approved build in the newest 10 pages"):
            self.run_review()

    def test_rejected_build_fails(self):
        self.asc.builds["b365"]["review"] = "REJECTED"
        with self.assertRaisesRegex(tr.AscError, "rejected"):
            self.run_review()

    def test_pull_requests_and_dry_runs_skip_without_connecting(self):
        def refuse(env):
            raise AssertionError("connected")
        review = {}
        tr.run(self.result, self.notes, review, env={"GITHUB_EVENT_NAME": "pull_request"}, connect=refuse)
        self.assertEqual(review["status"], "SKIPPED")
        self.result.write_text(json.dumps({"mode": "dry", "version": "0.3.4", "build": 365}))
        review = {}
        tr.run(self.result, self.notes, review, env={"GITHUB_EVENT_NAME": "push"}, connect=refuse)
        self.assertEqual(review["status"], "SKIPPED")

    def test_cli_writes_the_review_without_secrets(self):
        out = self.tmp / "review.json"
        env = {k: v for k, v in os.environ.items() if not k.startswith("ASC_")}
        secret = base64.b64encode(b"-----BEGIN PRIVATE KEY-----\nnot a key\n-----END PRIVATE KEY-----\n").decode()
        env |= {"GITHUB_EVENT_NAME": "workflow_dispatch", "ASC_KEY_ID": "K1", "ASC_ISSUER_ID": "issuer-secret",
                "ASC_KEY_P8": secret}
        result = subprocess.run([sys.executable, str(SCRIPT), str(self.result), str(self.notes), str(out)],
                                capture_output=True, text=True, env=env, timeout=60)
        self.assertEqual(result.returncode, 1)
        review = json.loads(out.read_text())
        self.assertEqual((review["status"], review["build"]), ("FAIL", 365))
        self.assertIn("openssl could not sign", review["error"])
        for text in (out.read_text(), result.stdout, result.stderr):
            self.assertNotIn("issuer-secret", text)
            self.assertNotIn(secret[:12], text)

        env["GITHUB_EVENT_NAME"] = "pull_request"
        result = subprocess.run([sys.executable, str(SCRIPT), str(self.result), str(self.notes), str(out)],
                                capture_output=True, text=True, env=env, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(out.read_text())["status"], "SKIPPED")

    def test_key_sources(self):
        pem = b"-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n"
        encoded = base64.b64encode(pem).decode()
        wrapped = "\n".join(encoded[i:i + 8] for i in range(0, len(encoded), 8))
        self.assertEqual(tr.key_from_env({"ASC_KEY_ID": "K1", "ASC_ISSUER_ID": "i", "ASC_KEY_P8": wrapped})[2], pem)
        self.assertEqual(tr.key_from_env({"ASC_KEY_ID": "K1", "ASC_ISSUER_ID": "i", "ASC_KEY_P8": pem.decode()})[2], pem)
        path = self.tmp / "key.p8"
        path.write_bytes(pem)
        self.assertEqual(tr.key_from_env({"ASC_KEY_ID": "K1", "ASC_ISSUER_ID": "i", "ASC_KEY_PATH": str(path)})[2], pem)
        for env in ({"ASC_ISSUER_ID": "i", "ASC_KEY_P8": encoded}, {"ASC_KEY_ID": "K/1", "ASC_ISSUER_ID": "i"},
                    {"ASC_KEY_ID": "K1", "ASC_ISSUER_ID": "i", "ASC_KEY_P8": "!!"}):
            with self.subTest(env), self.assertRaises(tr.AscError):
                tr.key_from_env(env)

    def test_token_is_reused_until_it_nears_expiry(self):
        signed = []
        asc = tr.AppStoreConnect("K1", "issuer", b"pem", transport=self.asc,
                                 sign=lambda pem, message: signed.append(message) or b"\1" * 64, clock=self.clock)
        first = asc.token()
        self.assertEqual(asc.token(), first)
        self.clock.now += 1100
        self.assertNotEqual(asc.token(), first)
        self.assertEqual(len(signed), 2)


@unittest.skipUnless(shutil.which("openssl"), "openssl is not installed")
class TokenSignatureTest(unittest.TestCase):
    def test_openssl_signature_verifies_as_es256(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = pathlib.Path(tmp)
            subprocess.run(["openssl", "genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256",
                            "-out", str(tmp / "key.pem")], check=True, capture_output=True)
            subprocess.run(["openssl", "pkey", "-in", str(tmp / "key.pem"), "-pubout", "-out", str(tmp / "pub.pem")],
                           check=True, capture_output=True)
            asc = tr.AppStoreConnect("K1", "issuer", (tmp / "key.pem").read_bytes())
            header, claims, signature = asc.token().split(".")
            pad = lambda s: base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))  # noqa: E731
            self.assertEqual(json.loads(pad(header)), {"alg": "ES256", "kid": "K1", "typ": "JWT"})
            self.assertEqual(json.loads(pad(claims))["aud"], "appstoreconnect-v1")
            raw = pad(signature)
            self.assertEqual(len(raw), 64)

            def integer(value):
                value = value.lstrip(b"\0") or b"\0"
                value = b"\0" + value if value[0] & 0x80 else value
                return b"\x02" + bytes([len(value)]) + value
            body = integer(raw[:32]) + integer(raw[32:])
            (tmp / "sig.der").write_bytes(b"\x30" + bytes([len(body)]) + body)
            (tmp / "signed").write_bytes(f"{header}.{claims}".encode())
            verified = subprocess.run(["openssl", "dgst", "-sha256", "-verify", str(tmp / "pub.pem"),
                                       "-signature", str(tmp / "sig.der"), str(tmp / "signed")], capture_output=True)
            self.assertEqual(verified.returncode, 0, verified.stderr)


if __name__ == "__main__":
    unittest.main()
