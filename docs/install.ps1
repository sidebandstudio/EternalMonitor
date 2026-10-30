# EternalMonitor for Windows, installed from a terminal:
#
#   irm https://eternalmonitor.dev/install.ps1 | iex
#
# Beta: irm https://eternalmonitor.dev/install-beta.ps1 | iex
#
# Finds the newest installer for the channel the same way
# eternalmonitor.dev/download.html does, downloads it to %TEMP%, checks its
# SHA-256 against the published one and starts it. The installer asks for
# administrator rights itself. Nothing else is sent anywhere.
#
# Stable: the version, link and SHA-256 come from
#   https://eternalmonitor.dev/latest.json (the apps' update check reads it too).
# Beta: the moving "beta" GitHub release (.github/workflows/nightly.yml); the
#   SHA-256 is the asset's digest from GitHub, else the one in its release notes.
#
# $env:EM_INSTALL_DRY_RUN = 1 stops after the check, before the installer starts.
# Works in Windows PowerShell 5.1 and PowerShell 7.

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

    Write-Host ''
    Write-Host ("  EternalMonitor for Windows" + $(if ($channel -eq 'beta') { ', Beta' } else { '' })) -ForegroundColor Cyan
    Write-Host ''

    if (-not [Environment]::Is64BitOperatingSystem) {
        Fail 'EternalMonitor needs 64-bit Windows 10 (1809 or later) or 11.'
        return
    }

    try {
        if ($channel -eq 'stable') {
            Say 'Finding the current version...'
            $latest = Invoke-RestMethod -UseBasicParsing -Uri 'https://eternalmonitor.dev/latest.json' -Headers @{ 'Cache-Control' = 'no-cache' }
            $win = $latest.windows
            if (-not $win -or $win.version -notmatch '^\d+\.\d+\.\d+$') { throw 'latest.json does not name a Windows version.' }
            $name = 'EternalMonitor-Setup.exe'
            $url = "https://github.com/$repo/releases/download/v$($win.version)/$name"
            if ($win.installer -ne $url) { throw "latest.json names an unexpected installer: $($win.installer)" }
            $sha256 = "$($win.sha256)".ToLowerInvariant()
            $label = "v$($win.version)"
        } else {
            Say 'Finding the newest Beta build...'
            try {
                $release = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repo/releases/tags/beta" -Headers @{ Accept = 'application/vnd.github+json' }
            } catch {
                $status = $null
                if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
                if ($status -eq 404) { Fail 'There is no Beta build yet. Use Stable: irm https://eternalmonitor.dev/install.ps1 | iex'; return }
                if ($status -eq 403 -or $status -eq 429) { throw 'GitHub is limiting requests from this network. Try again in an hour.' }
                throw
            }
            $name = 'EternalMonitor-Setup-beta.exe'
            $asset = @($release.assets | Where-Object { $_.name -eq $name })[0]
            if (-not $asset) { throw "The Beta release has no $name." }
            $url = "https://github.com/$repo/releases/download/beta/$name"
            $sha256 = ''
            if ("$($asset.digest)" -match '^sha256:([0-9a-f]{64})$') { $sha256 = $Matches[1] }
            elseif ("$($release.body)" -match '(?s)SHA-256.*?\b([0-9a-f]{64})\b') { $sha256 = $Matches[1] }
            $label = 'Beta'
            if ("$($release.body)" -match '\b(\d+\.\d+\.\d+-beta\.\d{8}\.\d+)\b') { $label = $Matches[1] }
        }
        if ($sha256 -notmatch '^[0-9a-f]{64}$') { throw 'No published SHA-256 was found for this build, so it cannot be checked.' }

        $path = Join-Path $env:TEMP $name
        Say "Downloading $name ($label)..."
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $path
        $size = (Get-Item -LiteralPath $path).Length
        Say ('Downloaded {0:N1} MB to {1}' -f ($size / 1MB), $path)

        Say 'Checking its SHA-256...'
        $got = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($got -ne $sha256) {
            Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
            Fail "The SHA-256 does not match, so the download was deleted.`n  Expected $sha256`n  Got      $got"
            return
        }
        Say "SHA-256 matches: $got"
    } catch {
        Fail "Could not get the installer: $($_.Exception.Message)"
        return
    }

    if ($dryRun) {
        Write-Host ''
        Say 'Dry run (EM_INSTALL_DRY_RUN): the installer was not started.'
        Write-Host ''
        return
    }

    Say 'Starting the installer. Windows asks for permission next.'
    Start-Process -FilePath $path
    Write-Host ''
}
