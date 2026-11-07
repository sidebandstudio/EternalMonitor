#!/usr/bin/env python3
"""Sign the Beta channel's manifest, so Beta installs offer the newest Beta.

  sign_beta_manifest.py [--repo R] [--public-key PEM] < private-key.pem
      Read the beta release, write beta-manifest.txt from it, sign that with
      the Ed25519 private key read from stdin, upload both files to the
      release, then verify them as a Beta install does.
  sign_beta_manifest.py --verify-only [--repo R] [--public-key PEM]
      Only verify the published manifest and signature.

A Beta install offers a Beta only when beta-manifest.sig is a valid Ed25519
signature, by the key pinned in host/src/updates.rs, over the exact bytes of
beta-manifest.txt, and the manifest names the version, SHA-256 and size the
release does. Until a build is signed, Beta installs are offered no Beta.

The private key goes from stdin to openssl through a pipe. It is never an
argument or a file. Needs gh (signed in, for the upload) and OpenSSL 3 or
later; OPENSSL picks the binary.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

REPO = "sidebandstudio/EternalMonitor"
TAG = "beta"
ASSET = "EternalMonitor-Setup-beta.exe"
MANIFEST = "beta-manifest.txt"
SIGNATURE = "beta-manifest.sig"
# The public half of the key host/src/updates.rs pins (BETA_PUBLIC_KEY).
PUBLIC_KEY = b"""-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAgsKvIcaF0sBCxR9yo0x4vbXQBLrYnnWoMGpDIk0W7Fo=
-----END PUBLIC KEY-----
"""
VERSION_LINE = re.compile(r"(?m)^Version (\S+), from the Beta channel")
HASH_BLOCK = re.compile(r"(?m)^SHA-256 of EternalMonitor-Setup-beta\.exe:\s*```\s*([0-9A-Fa-f]{64})\s*```")
# x.y.z-beta.<identifiers>, without build metadata; numbers have no leading zeros.
NUMBER = r"(?:0|[1-9]\d*)"
BETA_VERSION = re.compile(rf"{NUMBER}\.{NUMBER}\.{NUMBER}-beta(?:\.(?:{NUMBER}|\d*[A-Za-z-][0-9A-Za-z-]*))+")


class SigningError(Exception):
    pass


def manifest_text(version, sha256, size):
    """The canonical manifest, version 1 (host/src/updates.rs BetaManifest::text)."""
    return (f"eternalmonitor-beta-manifest 1\nversion={version}\nsha256={sha256}\n"
            f"size={size}\nasset={ASSET}\n")


def only(pattern, text, what):
    found = pattern.findall(text)
    if len(found) != 1:
        raise SigningError(f"the release body names {len(found)} {what}, not one")
    return found[0]


def read_release(release):
    """(version, sha256, size) of the beta release's installer, once its body, asset
    and GitHub digest agree."""
    if release.get("tag_name") != TAG or release.get("draft") or release.get("prerelease") is not True:
        raise SigningError("not the published beta pre-release")
    body = release.get("body") or ""
    version = only(VERSION_LINE, body, "versions")
    if not BETA_VERSION.fullmatch(version):
        raise SigningError(f"not a Beta version: {version!r}")
    sha256 = only(HASH_BLOCK, body, "installer hashes").lower()
    assets = [a for a in release.get("assets", []) if a.get("name") == ASSET]
    if len(assets) != 1:
        raise SigningError(f"the release has no {ASSET}")
    size, digest = assets[0].get("size"), (assets[0].get("digest") or "").lower()
    if digest != f"sha256:{sha256}":
        raise SigningError(f"GitHub's digest {digest or '(none)'} differs from the body's {sha256}")
    if not isinstance(size, int) or size <= 0:
        raise SigningError(f"bad installer size: {size!r}")
    return version, sha256, size


class GitHub:
    def __init__(self, repo=REPO, run=subprocess.run):
        self.repo, self.run = repo, run

    def gh(self, *args):
        return self.run(["gh", *args], check=True, capture_output=True, text=True).stdout

    def release(self):
        return json.loads(self.gh("api", f"repos/{self.repo}/releases/tags/{TAG}"))

    def upload(self, paths):
        self.gh("release", "upload", TAG, *paths, "--repo", self.repo, "--clobber")

    def download(self, names, directory):
        patterns = [arg for name in names for arg in ("--pattern", name)]
        self.gh("release", "download", TAG, "--repo", self.repo, "--dir", directory, "--clobber", *patterns)


def openssl():
    return os.environ.get("OPENSSL", "openssl")


def sign(manifest, signature, private_key, run=subprocess.run):
    """Signs the file manifest into signature with the private key PEM (bytes),
    which reaches openssl on its stdin."""
    result = run([openssl(), "pkeyutl", "-sign", "-rawin", "-inkey", "/dev/stdin",
                  "-in", manifest, "-out", signature], input=private_key, capture_output=True)
    if result.returncode != 0:
        raise SigningError(f"openssl could not sign: {result.stderr.decode(errors='replace').strip()}")
    if os.path.getsize(signature) != 64:
        raise SigningError("the signature is not 64 bytes: is the key Ed25519?")


def verified(manifest, signature, public_key, run=subprocess.run):
    """Whether signature is public_key's (PEM bytes) signature over manifest."""
    with tempfile.TemporaryDirectory() as tmp:
        key = os.path.join(tmp, "public.pem")
        with open(key, "wb") as f:
            f.write(public_key)
        result = run([openssl(), "pkeyutl", "-verify", "-rawin", "-pubin", "-inkey", key,
                      "-in", manifest, "-sigfile", signature], capture_output=True)
    return result.returncode == 0 and os.path.getsize(signature) == 64


def verify_published(github, public_key=PUBLIC_KEY, run=subprocess.run):
    """Checks the published manifest and signature as a Beta install does, and
    returns the version they vouch for."""
    version, sha256, size = read_release(github.release())
    with tempfile.TemporaryDirectory() as tmp:
        try:
            github.download([MANIFEST, SIGNATURE], tmp)
        except subprocess.CalledProcessError as error:
            raise SigningError(f"could not download the manifest and signature: {error.stderr}")
        manifest, signature = os.path.join(tmp, MANIFEST), os.path.join(tmp, SIGNATURE)
        if not (os.path.isfile(manifest) and os.path.isfile(signature)):
            raise SigningError("the release lacks the manifest or its signature")
        if not verified(manifest, signature, public_key, run):
            raise SigningError("the signature does not verify with the public key")
        with open(manifest, "rb") as f:
            if f.read() != manifest_text(version, sha256, size).encode():
                raise SigningError("the signed manifest does not match the release")
    return version


def sign_and_publish(github, private_key, public_key=PUBLIC_KEY, run=subprocess.run):
    """Writes, signs and uploads the manifest for the release as it is now, then
    verifies what was published. Returns the version."""
    version, sha256, size = read_release(github.release())
    with tempfile.TemporaryDirectory() as tmp:
        manifest, signature = os.path.join(tmp, MANIFEST), os.path.join(tmp, SIGNATURE)
        with open(manifest, "w", encoding="utf-8", newline="\n") as f:
            f.write(manifest_text(version, sha256, size))
        sign(manifest, signature, private_key, run)
        # A wrong key must not replace a good signature.
        if not verified(manifest, signature, public_key, run):
            raise SigningError("the private key does not match the public key")
        github.upload([manifest, signature])
    published = verify_published(github, public_key, run)
    if published != version:
        raise SigningError(f"the release moved to {published} while signing {version}; run again")
    return version


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=os.environ.get("GH_REPO", REPO))
    parser.add_argument("--public-key", help="PEM file to verify with instead of the pinned key")
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args(argv)
    public_key = PUBLIC_KEY
    if args.public_key:
        with open(args.public_key, "rb") as f:
            public_key = f.read()
    github = GitHub(args.repo)
    try:
        if args.verify_only:
            print(f"Verified the signed manifest for {verify_published(github, public_key)}")
        else:
            private_key = sys.stdin.buffer.read()
            if b"PRIVATE KEY" not in private_key:
                raise SigningError("pipe the Ed25519 private key PEM into stdin")
            print(f"Signed and verified the manifest for {sign_and_publish(github, private_key, public_key)}")
    except SigningError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
