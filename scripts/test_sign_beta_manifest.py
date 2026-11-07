"""The Beta manifest is written from a release whose parts agree, signed without the key
touching an argument or a file, and verified the way a Beta install verifies it."""
import base64
import copy
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import nightly  # noqa: E402
import sign_beta_manifest as signer  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
SHA = "62b9d9d" + "0" * 33
HASH = "ab" * 32
VERSION = "0.3.5-beta.20261107.26"
SIZE = 63301683
# A published, test-only seed: host/src/updates/tests.rs signs with it too.
TEST_SEED = bytes(range(32))


def run_openssl(*args, data=None):
    return subprocess.run([signer.openssl(), *args], input=data, capture_output=True, check=True).stdout


def ed25519_available():
    try:
        key = run_openssl("genpkey", "-algorithm", "ed25519")
        with tempfile.TemporaryDirectory() as tmp:
            manifest, signature = os.path.join(tmp, "m"), os.path.join(tmp, "s")
            pathlib.Path(manifest).write_bytes(b"x\n")
            signer.sign(manifest, signature, key)
        return True
    except (OSError, subprocess.CalledProcessError, signer.SigningError):
        return False


def private_from_seed(seed):
    der = bytes.fromhex("302e020100300506032b657004220420") + seed
    return b"-----BEGIN PRIVATE KEY-----\n" + base64.b64encode(der) + b"\n-----END PRIVATE KEY-----\n"


def new_key():
    """A throwaway Ed25519 key pair: (private PEM, public PEM)."""
    private = run_openssl("genpkey", "-algorithm", "ed25519")
    return private, run_openssl("pkey", "-pubout", data=private)


def release(body=None, digest=None, size=SIZE, **fields):
    notes = nightly.notes(VERSION, "release/0.3.5-candidate", SHA, "0.3.4", HASH)
    data = {
        "tag_name": "beta", "draft": False, "prerelease": True,
        "body": nightly.beta_body(notes, SHA) if body is None else body,
        "assets": [{"name": signer.ASSET, "size": size,
                    "digest": f"sha256:{HASH}" if digest is None else digest}],
    }
    data.update(fields)
    return data


class FakeGitHub:
    """The beta release and its uploaded assets, in memory."""

    def __init__(self, data=None):
        self.data, self.assets, self.uploads = data or release(), {}, 0

    def release(self):
        return copy.deepcopy(self.data)

    def upload(self, paths):
        self.uploads += 1
        for path in paths:
            self.assets[os.path.basename(path)] = pathlib.Path(path).read_bytes()

    def download(self, names, directory):
        for name in names:
            if name in self.assets:
                pathlib.Path(directory, name).write_bytes(self.assets[name])


class Recorder:
    """subprocess.run, recording each call's arguments and whether it had input."""

    def __init__(self):
        self.calls = []

    def __call__(self, args, **kwargs):
        self.calls.append((list(args), kwargs.get("input")))
        return subprocess.run(args, **kwargs)


class ManifestTest(unittest.TestCase):
    def test_the_manifest_is_the_canonical_text_the_host_parses(self):
        fixture = (ROOT / "scripts/fixtures/beta-manifest.txt").read_bytes().replace(b"\r\n", b"\n")
        self.assertEqual(signer.manifest_text(VERSION, HASH, SIZE).encode(), fixture)

    def test_the_release_parts_must_agree(self):
        self.assertEqual(signer.read_release(release()), (VERSION, HASH, SIZE))
        good = release()["body"]
        bad = [
            release(digest=f"sha256:{'0' * 64}"),
            release(digest=""),
            release(body=good.replace(HASH, "")),
            release(body=good.replace(VERSION, "0.3.5")),
            release(body=good.replace(VERSION, "0.3.5-beta.01")),
            release(body=good.replace(VERSION, "0.3.5-beta.1+ci")),
            release(body=f"Version 0.3.5-beta.20261108.27, from the Beta channel\n{good}"),
            release(size=0),
            release(tag_name="v0.3.5"),
            release(prerelease=False),
            release(draft=True),
            release(assets=[]),
        ]
        for data in bad:
            with self.assertRaises(signer.SigningError, msg=str(data)[:200]):
                signer.read_release(data)


@unittest.skipUnless(ed25519_available(), "needs OpenSSL 3 with Ed25519")
class SigningTest(unittest.TestCase):
    def test_signs_uploads_and_verifies(self):
        private, public = new_key()
        github, run = FakeGitHub(), Recorder()
        self.assertEqual(signer.sign_and_publish(github, private, public, run), VERSION)
        self.assertEqual(github.assets[signer.MANIFEST], signer.manifest_text(VERSION, HASH, SIZE).encode())
        self.assertEqual(len(github.assets[signer.SIGNATURE]), 64)
        self.assertEqual(signer.verify_published(github, public), VERSION)
        # The key went to openssl's stdin only: never an argument, never uploaded.
        with_key = [args for args, given in run.calls if given == private]
        self.assertEqual(len(with_key), 1)
        self.assertIn("/dev/stdin", with_key[0])
        for args, _ in run.calls:
            self.assertFalse(any("PRIVATE KEY" in arg for arg in args), args)
        for data in github.assets.values():
            self.assertNotIn(b"PRIVATE KEY", data)

    def test_a_key_that_is_not_the_pinned_one_uploads_nothing(self):
        private, _ = new_key()
        _, other = new_key()
        github = FakeGitHub()
        with self.assertRaises(signer.SigningError):
            signer.sign_and_publish(github, private, other)
        self.assertEqual(github.uploads, 0)
        # Against the production key, a throwaway key fails too.
        with self.assertRaises(signer.SigningError):
            signer.sign_and_publish(github, private)
        self.assertEqual(github.uploads, 0)

    def test_a_release_whose_parts_disagree_is_not_signed(self):
        private, public = new_key()
        github = FakeGitHub(release(digest=f"sha256:{'0' * 64}"))
        with self.assertRaises(signer.SigningError):
            signer.sign_and_publish(github, private, public)
        self.assertEqual(github.uploads, 0)

    def test_verify_only_refuses_a_bad_or_missing_manifest(self):
        private, public = new_key()
        github = FakeGitHub()
        signer.sign_and_publish(github, private, public)
        _, other = new_key()
        with self.assertRaises(signer.SigningError):
            signer.verify_published(github, other)
        good = dict(github.assets)
        flipped = bytearray(good[signer.MANIFEST])
        flipped[40] ^= 1
        cases = {
            "flipped byte": {**good, signer.MANIFEST: bytes(flipped)},
            "no signature": {signer.MANIFEST: good[signer.MANIFEST]},
            "no manifest": {signer.SIGNATURE: good[signer.SIGNATURE]},
        }
        for name, assets in cases.items():
            github.assets = assets
            with self.assertRaises(signer.SigningError, msg=name):
                signer.verify_published(github, public)
        # Signed, but the nightly has moved the release on since.
        github.assets = good
        github.data = release(body=github.data["body"].replace(VERSION, "0.3.5-beta.20261108.27"))
        with self.assertRaises(signer.SigningError):
            signer.verify_published(github, public)

    def test_openssl_signs_what_the_host_verifies(self):
        # Ed25519 is deterministic: the host's test signs this fixture with ring and
        # gets the same bytes.
        fixture = ROOT / "scripts/fixtures/beta-manifest.test.sig"
        with tempfile.TemporaryDirectory() as tmp:
            manifest = os.path.join(tmp, signer.MANIFEST)
            signature = os.path.join(tmp, signer.SIGNATURE)
            shutil.copyfile(ROOT / "scripts/fixtures/beta-manifest.txt", manifest)
            pathlib.Path(manifest).write_bytes(pathlib.Path(manifest).read_bytes().replace(b"\r\n", b"\n"))
            signer.sign(manifest, signature, private_from_seed(TEST_SEED))
            self.assertEqual(pathlib.Path(signature).read_bytes(), fixture.read_bytes())


class CommandTest(unittest.TestCase):
    def test_signing_needs_a_private_key_on_stdin(self):
        class Stdin:
            buffer = __import__("io").BytesIO(b"not a key")

        saved = sys.stdin
        sys.stdin = Stdin()
        try:
            self.assertEqual(signer.main(["--repo", "example/none"]), 1)
        finally:
            sys.stdin = saved


if __name__ == "__main__":
    unittest.main()
