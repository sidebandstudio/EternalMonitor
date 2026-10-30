# EternalMonitor for Windows, installed from a terminal:
#
#   irm https://eternalmonitor.dev/install.ps1 | iex
#
# Beta: irm https://eternalmonitor.dev/install-beta.ps1 | iex
#
# Finds the newest installer for the channel the same way
# eternalmonitor.dev/download.html does, downloads it to a new folder in %TEMP%,
# checks its SHA-256 against the published one and starts it. The file stays
# locked against changes from the check until the installer closes, then the
# folder is deleted. The installer asks for administrator rights itself.
# Nothing else is sent anywhere.
#
# Stable: the version, link and SHA-256 come from
#   https://eternalmonitor.dev/latest.json (the apps' update check reads it too).
# Beta: the moving "beta" GitHub release (.github/workflows/nightly.yml); the
#   SHA-256 is the asset's digest from GitHub, else the one in its release notes.
#
# $env:EM_INSTALL_DRY_RUN = 1 stops after the check, before the installer starts.
# Works in Windows PowerShell 5.1 and PowerShell 7. iex runs this in your
# session, so it never exits it and leaves no variables or functions behind.

& {
    $ErrorActionPreference = 'Stop'
    # Invoke-WebRequest draws its own progress bar slowly in 5.1; the steps below say what happens.
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'sidebandstudio/EternalMonitor'
    # install-beta.ps1 sets $EternalMonitorChannel before running this script.
    $channel = if ((Get-Variable EternalMonitorChannel -ValueOnly -ErrorAction SilentlyContinue) -eq 'beta') { 'beta' } else { 'stable' }
    $dryRun = $env:EM_INSTALL_DRY_RUN -and $env:EM_INSTALL_DRY_RUN -ne '0'

    function Say($text) { Write-Host "  $text" }
    function Fail($text) {
        Write-Host ''
        Write-Host "  $text" -ForegroundColor Red
        Write-Host '  Nothing was installed. The installer is also at https://eternalmonitor.dev/download.html'
        Write-Host ''
    }
    # A GitHub API request: the reply, $true once -OutFile is written, or $null for a 404.
    # Anonymous requests are limited to 60 an hour; a Beta install makes two, at most four.
    function GitHub($uri, $accept, $outFile) {
        $request = @{ UseBasicParsing = $true; Uri = $uri; Headers = @{ Accept = $accept } }
        try {
            if ($outFile) { Invoke-WebRequest @request -OutFile $outFile; $true } else { Invoke-RestMethod @request }
        } catch {
            $status = $null
            if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
            if ($status -eq 404) { return $null }
            if ($status -eq 403 -or $status -eq 429) { throw 'GitHub is limiting requests from this network. Try again in an hour.' }
            throw
        }
    }

    Write-Host ''
    Write-Host ("  EternalMonitor for Windows" + $(if ($channel -eq 'beta') { ', Beta' } else { '' })) -ForegroundColor Cyan
    Write-Host ''

    if (-not [Environment]::Is64BitOperatingSystem) {
        Fail 'EternalMonitor needs 64-bit Windows 10 (1809 or later) or 11.'
        return
    }

    # A folder of its own for every run, so nothing else expects the file's name.
    $dir = Join-Path ([IO.Path]::GetTempPath()) ('EternalMonitor-' + [guid]::NewGuid().ToString('N'))
    $file = $null
    $step = 'get the installer'
    try {
        New-Item -ItemType Directory -Path $dir | Out-Null
        if ($channel -eq 'stable') {
            Say 'Finding the current version...'
            $latest = Invoke-RestMethod -UseBasicParsing -Uri 'https://eternalmonitor.dev/latest.json' -Headers @{ 'Cache-Control' = 'no-cache' }
            $win = $latest.windows
            if (-not $win -or $win.version -notmatch '^\d+\.\d+\.\d+$') { throw 'latest.json does not name a Windows version.' }
            $name = 'EternalMonitor-Setup.exe'
            $url = "https://github.com/$repo/releases/download/v$($win.version)/$name"
            if ($win.installer -ne $url) { throw "latest.json names an unexpected installer: $($win.installer)" }
            $sha256 = "$($win.sha256)".ToLowerInvariant()
            if ($sha256 -notmatch '^[0-9a-f]{64}$') { throw 'No published SHA-256 was found for this build, so it cannot be checked.' }
            $path = Join-Path $dir $name
            Say "Downloading $name (v$($win.version))..."
            Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $path
        } else {
            $name = 'EternalMonitor-Setup-beta.exe'
            $path = Join-Path $dir $name
            $api = "https://api.github.com/repos/$repo"
            # nightly.py swaps in a new build by renaming the served asset and deleting it
            # a second or two later. The download asks for the asset by its id, so it gets
            # the bytes whose SHA-256 was just read; if that asset is gone, look once more.
            for ($look = 1; ; $look++) {
                Say 'Finding the newest Beta build...'
                $release = GitHub "$api/releases/tags/beta" 'application/vnd.github+json'
                if (-not $release) { Fail 'There is no Beta build yet. Use Stable: irm https://eternalmonitor.dev/install.ps1 | iex'; return }
                $asset = @($release.assets | Where-Object { $_.name -eq $name })[0]
                if ($asset) {
                    $sha256 = ''
                    if ("$($asset.digest)" -match '^sha256:([0-9a-f]{64})$') { $sha256 = $Matches[1] }
                    elseif ("$($release.body)" -match '(?s)SHA-256.*?\b([0-9a-f]{64})\b') { $sha256 = $Matches[1] }
                    if ($sha256 -notmatch '^[0-9a-f]{64}$') { throw 'No published SHA-256 was found for this build, so it cannot be checked.' }
                    $label = 'Beta'
                    if ("$($release.body)" -match '\b(\d+\.\d+\.\d+-beta\.\d{8}\.\d+)\b') { $label = $Matches[1] }
                    Say "Downloading $name ($label)..."
                    if (GitHub "$api/releases/assets/$($asset.id)" 'application/octet-stream' $path) { break }
                }
                if ($look -ge 2) { throw "The Beta release has no $name right now. Try again in a few minutes." }
                Say 'A new Beta build is going up. Looking again...'
                Start-Sleep -Seconds 5
            }
        }
        Say ('Downloaded {0:N1} MB to {1}' -f ((Get-Item -LiteralPath $path).Length / 1MB), $path)

        # Hash through a handle that lets others read the file but not change, rename or
        # delete it, and keep that handle until the installer closes: the checked bytes
        # are the ones that run, including when the installer starts itself again with
        # administrator rights.
        Say 'Checking its SHA-256...'
        $file = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        $hasher = [Security.Cryptography.SHA256]::Create()
        $got = ([BitConverter]::ToString($hasher.ComputeHash($file)) -replace '-').ToLowerInvariant()
        $hasher.Dispose()
        if ($got -ne $sha256) {
            Fail "The SHA-256 does not match, so the download was deleted.`n  Expected $sha256`n  Got      $got"
            return
        }
        Say "SHA-256 matches: $got"

        if ($dryRun) {
            Write-Host ''
            Say 'Dry run (EM_INSTALL_DRY_RUN): the installer was not started.'
            Write-Host ''
            return
        }

        $step = 'start the installer'
        Say 'Starting the installer. Windows asks for permission next.'
        $installer = Start-Process -FilePath $path -PassThru
        Say 'Waiting for the installer to close...'
        $installer.WaitForExit()
        Write-Host ''
    } catch {
        Fail "Could not ${step}: $($_.Exception.Message)"
    } finally {
        if ($file) { $file.Dispose() }
        Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
