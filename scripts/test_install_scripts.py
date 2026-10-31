"""The PowerShell one-liners on download.html must run the scripts docs/ serves,
and those scripts must find the installer where the site and nightly.py put it
and refuse one whose SHA-256 does not match."""
import json
import pathlib
import re
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import nightly  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"
SITE = "https://" + (DOCS / "CNAME").read_text().strip()


def one_liner(page, code_id):
    m = re.search(rf'<span id="{code_id}">(.+?)</span></code>', page)
    # <wbr> only marks where a narrow screen may wrap; it copies as nothing.
    return m and m.group(1).replace("<wbr>", "")


class OneLiners(unittest.TestCase):
    def test_page_runs_the_scripts_docs_serves(self):
        page = (DOCS / "download.html").read_text()
        for code_id, script in (("install-cmd", "install.ps1"), ("beta-install-cmd", "install-beta.ps1")):
            self.assertEqual(one_liner(page, code_id), f"irm {SITE}/{script} | iex")
            self.assertTrue((DOCS / script).is_file(), script)
            # The copy button copies this exact element.
            self.assertIn(f'data-copy="{code_id}"', page)

    def test_scripts_name_the_same_one_liners(self):
        install = (DOCS / "install.ps1").read_text()
        self.assertIn(f"irm {SITE}/install.ps1 | iex", install)
        self.assertIn(f"irm {SITE}/install-beta.ps1 | iex", install)
        beta = (DOCS / "install-beta.ps1").read_text()
        self.assertIn(f"irm {SITE}/install-beta.ps1 | iex", beta)
        # The Beta wrapper runs the published install.ps1 with the channel set.
        self.assertIn(f"'{SITE}/install.ps1' | Invoke-Expression", beta)
        self.assertIn("$EternalMonitorChannel = 'beta'", beta)


class InstallScript(unittest.TestCase):
    script = (DOCS / "install.ps1").read_text()

    def test_stable_reads_latest_json_and_its_installer_link(self):
        self.assertIn(f"'{SITE}/latest.json'", self.script)
        latest = json.loads((DOCS / "latest.json").read_text())["windows"]
        built = "https://github.com/$repo/releases/download/v$($win.version)/$name".replace(
            "$repo", nightly.REPO).replace("$($win.version)", latest["version"]).replace("$name", "EternalMonitor-Setup.exe")
        self.assertEqual(built, latest["installer"])
        self.assertIn("if ($win.installer -ne $url)", self.script)
        self.assertIn("$sha256 = \"$($win.sha256)\"", self.script)

    def test_beta_uses_the_moving_release_nightly_publishes(self):
        self.assertIn(f"$repo = '{nightly.REPO}'", self.script)
        self.assertIn(f"/releases/tags/{nightly.MOVING_TAG}\"", self.script)
        self.assertIn(f"$name = '{nightly.ASSET}'", self.script)
        # The bytes downloaded are the asset whose digest was read: nightly.py renames
        # and deletes the served asset, so the moving download link may change between
        # two requests, but an asset id does not.
        self.assertNotIn(f"/releases/download/{nightly.MOVING_TAG}/", self.script)
        self.assertIn("GitHub \"$api/releases/assets/$($asset.id)\" 'application/octet-stream' $path", self.script)
        # One more look if the asset vanished mid-publish, never more.
        self.assertIn("if ($look -ge 2) { throw", self.script)
        self.assertIn("GitHub is limiting requests from this network. Try again in an hour.", self.script)
        # GitHub's asset digest first, else the SHA-256 that nightly.notes() writes.
        self.assertIn("'^sha256:([0-9a-f]{64})$'", self.script)
        notes = nightly.notes("0.3.5-beta.20261030.1", "main", "a" * 40, "0.3.4", "B" * 64)
        body = re.search(r"-match '(\(\?s\)SHA-256[^']+)'", self.script).group(1)
        self.assertEqual(re.search(body.replace("(?s)", ""), notes, re.S).group(1), "b" * 64)

    def test_it_checks_the_hash_before_starting_anything(self):
        s = self.script
        # A new folder every run, never a fixed name in %TEMP%.
        self.assertIn("[guid]::NewGuid()", s)
        self.assertNotIn("Join-Path $env:TEMP", s)
        # The hash is read through a handle that denies writes and deletes...
        lock = s.index("[IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)")
        check = s.index("$hasher.ComputeHash($file)")
        refuse = s.index("if ($got -ne $sha256)")
        dry = s.index("if ($dryRun)")
        start = s.index("$installer = Start-Process -FilePath $path -PassThru")
        wait = s.index("$installer.WaitForExit()")
        # ...that stays open until the installer closes, then the folder goes.
        release = s.index("if ($file) { $file.Dispose() }")
        clean = s.index("Remove-Item -LiteralPath $dir -Recurse -Force")
        self.assertLess(s.index("} finally {"), release)
        self.assertEqual(sorted([lock, check, refuse, dry, start, wait, release, clean]),
                         [lock, check, refuse, dry, start, wait, release, clean])
        self.assertNotIn("Get-FileHash", s)
        self.assertEqual(s.count("Start-Process"), 1)
        self.assertEqual(s.count("$file.Dispose()"), 1)
        self.assertIn("'^[0-9a-f]{64}$'", s)
        self.assertIn("$env:EM_INSTALL_DRY_RUN", s)
        self.assertIn("[Net.SecurityProtocolType]::Tls12", s)

    def test_safe_to_pipe_into_iex(self):
        for name in ("install.ps1", "install-beta.ps1"):
            text = (DOCS / name).read_text()
            # iex runs in the visitor's session: exit would close their window.
            self.assertIsNone(re.search(r"\bexit\b", text, re.I), name)
            # Nothing reaches outside its own scope.
            self.assertIsNone(re.search(r"\$(global|script):|-Scope\b", text, re.I), name)
            # Everything runs in a script block, so nothing is left in their session.
            code = [l for l in text.splitlines() if l.strip() and not l.lstrip().startswith("#")]
            self.assertEqual((code[0], code[-1]), ("& {", "}"), name)
            text.encode("ascii")


if __name__ == "__main__":
    unittest.main()
