# Checks for the Setup step that puts the PC's screens back after the virtual display driver
# steps. The rule runs on fake screens in this process; nothing calls SetDisplayConfig. Then the
# script runs as Setup runs it, and must exit 0 whatever it is given.
param([string]$GuardPath)
$ErrorActionPreference = 'Stop'
$source = if ($GuardPath) { $GuardPath } else { Join-Path $PSScriptRoot '..\scripts\display-guard.ps1' }
$iss = Get-Content -Raw (Join-Path $PSScriptRoot '..\EternalMonitor.iss')
$root = Join-Path ([IO.Path]::GetTempPath()) ('em-display-guard-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $root | Out-Null
$script = Join-Path $root 'display-guard.ps1'
Copy-Item $source $script
$saved = Join-Path $root 'display-layout.json'

. $script

function Screen([string]$Kind) {
    switch ($Kind) {
        'panel' { [pscustomobject]@{Adapter='d2c1'; Target=4352; Internal=$true; Virtual=$false; Mode='1920x1200@165Hz'} }
        'monitor' { [pscustomobject]@{Adapter='d2c1'; Target=4353; Internal=$false; Virtual=$false; Mode='2560x1440@59.94Hz'} }
        'vdd' { [pscustomobject]@{Adapter='200000101'; Target=256; Internal=$false; Virtual=$true; Mode='2732x2048@60Hz'} }
    }
}
# Windows as the guard sees it: each read takes the next layout, the last one repeating. Each
# layout's bytes name the read that returned it.
function Reset-Fixture([object[]]$Reads) {
    $global:EMGuard = @{
        Reads = [Collections.Generic.Queue[object]]::new(); Last = $null; Count = 0
        Reapplied = $null; Internal = $null; RefuseReapply = $false; RefuseInternal = $false
        ReapplyCalls = @(); InternalCalls = 0; Waited = 0; Throw = $false
    }
    foreach ($read in $Reads) { $global:EMGuard.Reads.Enqueue($read) }
    Remove-Item -LiteralPath $saved -ErrorAction SilentlyContinue
}
function Layout([string[]]$Kinds) { ,@($Kinds | Where-Object { $_ } | ForEach-Object { Screen $_ }) }
function Read-Screens {
    if ($global:EMGuard.Throw) { throw 'QueryDisplayConfig is not available' }
    if ($global:EMGuard.Reads.Count) { $global:EMGuard.Last = $global:EMGuard.Reads.Dequeue() }
    $global:EMGuard.Count++
    if ($global:EMGuard.Last -is [string] -and $global:EMGuard.Last -eq 'unknown') { return $null }
    [pscustomobject]@{Screens=$global:EMGuard.Last; Paths=[byte[]]@($global:EMGuard.Count); Modes=[byte[]]@(7); PathCount=1; ModeCount=1}
}
function Set-SavedLayout($Saved) {
    $global:EMGuard.ReapplyCalls += [int]$Saved.Paths[0]
    if ($global:EMGuard.RefuseReapply) { return 87 }
    if ($null -ne $global:EMGuard.Reapplied) { $global:EMGuard.Reads.Clear(); $global:EMGuard.Last = $global:EMGuard.Reapplied }
    0
}
function Set-PcScreenOnly {
    $global:EMGuard.InternalCalls++
    if ($global:EMGuard.RefuseInternal) { return 31 }
    if ($null -ne $global:EMGuard.Internal) { $global:EMGuard.Reads.Clear(); $global:EMGuard.Last = $global:EMGuard.Internal }
    0
}
function Wait-Display([int]$Milliseconds) { $global:EMGuard.Waited += $Milliseconds }

# Setup saves, runs the driver steps, then checks. Returns the output.
function Invoke-Setup { (@(Invoke-DisplayGuard 'save' $saved) + @(Invoke-DisplayGuard 'check' $saved)) -join "`n" }
$failures = @()
function Expect([string]$Case, [string]$Output, [string]$Want, [int[]]$Reapply, [int]$Internal) {
    $calls = "$($global:EMGuard.ReapplyCalls -join ',')/$($global:EMGuard.InternalCalls)"
    if (!$Output.Contains($Want) -or $calls -ne "$($Reapply -join ',')/$Internal") {
        $script:failures += "$Case`: calls $calls (want $($Reapply -join ',')/$Internal), output:`n$Output"
    }
}
$both = 'paths=2 [internal tgt=d2c1/4352 1920x1200@165Hz | external tgt=d2c1/4353 2560x1440@59.94Hz]'
try {
    # Healthy: no SetDisplayConfig call and no wait.
    Reset-Fixture @((Layout panel, monitor), (Layout panel, monitor))
    $out = Invoke-Setup
    Expect 'healthy' $out "screens before the display driver steps: $both vdd=0`nDisplay guard: screens after the display driver steps: $both vdd=0" @() 0
    if ($global:EMGuard.Waited) { $failures += "healthy: waited $($global:EMGuard.Waited) ms" }
    if ($out -match 'restor') { $failures += "healthy: $out" }
    # An external monitor unplugged while the panel stays.
    Reset-Fixture @((Layout panel, monitor), (Layout panel))
    Expect 'monitor unplugged' (Invoke-Setup) 'paths=1 [internal' @() 0
    # A real screen it did not have before still counts.
    Reset-Fixture @((Layout panel), (Layout monitor))
    Expect 'other screen' (Invoke-Setup) 'paths=1 [external' @() 0

    # Every real screen lost: the layout read before the driver steps goes back on.
    Reset-Fixture @((Layout panel, monitor), (Layout))
    $global:EMGuard.Reapplied = Layout panel, monitor
    Expect 'all lost' (Invoke-Setup) "outcome=restored applied=saved_layout $both vdd=0" @(1) 0
    if ($global:EMGuard.Waited -ne $SettleMs) { $failures += "all lost: waited $($global:EMGuard.Waited) ms, not $SettleMs" }
    # The virtual display left alone counts as lost.
    Reset-Fixture @((Layout panel), (Layout vdd))
    $global:EMGuard.Reapplied = Layout panel
    Expect 'virtual only' (Invoke-Setup) 'outcome=restored applied=saved_layout paths=1' @(1) 0
    # Windows brings a screen back itself within the wait: nothing is changed.
    Reset-Fixture @((Layout panel), (Layout), (Layout), (Layout panel))
    Expect 'back by itself' (Invoke-Setup) 'after the display driver steps: paths=1' @() 0

    # The saved layout refused: "PC screen only", because the panel was on.
    Reset-Fixture @((Layout panel, monitor), (Layout))
    $global:EMGuard.RefuseReapply = $true; $global:EMGuard.Internal = Layout panel
    Expect 'refused, panel on' (Invoke-Setup) 'outcome=restored applied=internal paths=1 [internal' @(1) 1
    # Taken, but no screen came back: the same.
    Reset-Fixture @((Layout panel), (Layout))
    $global:EMGuard.Internal = Layout panel
    Expect 'taken, no screen' (Invoke-Setup) 'outcome=restored applied=internal' @(1) 1
    # The panel was not on: no switch to it.
    Reset-Fixture @((Layout monitor), (Layout))
    $global:EMGuard.RefuseReapply = $true
    Expect 'panel off' (Invoke-Setup) 'outcome=restore_failed reason=saved_layout_refused code=87 paths=0 [] vdd=0' @(1) 0

    # One attempt: both refused, each tried once, and a second check tries nothing.
    Reset-Fixture @((Layout panel), (Layout))
    $global:EMGuard.RefuseReapply = $true; $global:EMGuard.RefuseInternal = $true
    Expect 'one attempt' (Invoke-Setup) 'outcome=restore_failed reason=saved_layout_refused code=87 reason=internal_refused code=31 paths=0' @(1) 1
    Expect 'second check' ((Invoke-DisplayGuard 'check' $saved) -join "`n") 'nothing saved to compare with' @(1) 1
    if ($global:EMGuard.Count -gt 50) { $failures += "one attempt: $($global:EMGuard.Count) reads" }

    # Nothing known: no change.
    Reset-Fixture @('unknown')
    Expect 'unknown before' (Invoke-Setup) 'nothing saved to compare with' @() 0
    Reset-Fixture @((Layout panel), 'unknown')
    Expect 'unknown after' (Invoke-Setup) 'after the display driver steps: paths=unknown' @() 0
    # A failing Windows call is reported, never thrown.
    Reset-Fixture @(); $global:EMGuard.Throw = $true
    Expect 'call fails' (Invoke-Setup) 'Display guard skipped: QueryDisplayConfig is not available' @() 0

    # As Setup runs it: always exit 0, with a line for the log.
    $shell = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
    $bad = Join-Path $root 'bad.json'
    [IO.File]::WriteAllText($bad, '{not json')
    $cases = @(
        @('-Action', 'check', '-Path', (Join-Path $root 'missing.json')),
        @('-Action', 'check', '-Path', $bad),
        @('-Action', 'save', '-Path', (Join-Path $root 'no\such\folder\layout.json')),
        @('-Action', 'reset', '-Path', $saved),
        @('-Action', 'save'),
        @('-Path', $saved))
    foreach ($arguments in $cases) {
        $out = & $shell -NoProfile -ExecutionPolicy Bypass -File $script @arguments 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0 -or $out -notmatch 'Display guard') {
            $failures += "run $($arguments -join ' '): exit $LASTEXITCODE, output: $out"
        }
    }
    if ($env:OS -eq 'Windows_NT') {
        # The real Windows calls compile and run here; nothing is lost between the two, so nothing changes.
        $out = (& $shell -NoProfile -ExecutionPolicy Bypass -File $script -Action save -Path $saved 2>&1 | Out-String) +
            (& $shell -NoProfile -ExecutionPolicy Bypass -File $script -Action check -Path $saved 2>&1 | Out-String)
        if ($LASTEXITCODE -ne 0 -or $out -match 'skipped|restor' -or $out -notmatch 'screens before' -or $out -notmatch 'screens after') {
            $failures += "real run: exit $LASTEXITCODE, output: $out"
        }
        Write-Output $out.Trim()
    }

    # Setup ships the guard, saves before the vendor setup and checks after the tasks' Disable,
    # with no Check, so a failed driver step still gets the check. [Run] ignores its exit code.
    if ($iss -notmatch 'Source: "scripts\\display-guard\.ps1"') { $failures += 'Setup does not install the guard' }
    $run = @(($iss -split "`n") | Where-Object { $_ -match '^Filename:' })
    $at = { param($pattern) for ($i = 0; $i -lt $run.Count; $i++) { if ($run[$i] -match $pattern) { return $i } }; -1 }
    $save = & $at 'display-guard\.ps1"" -Action save'
    $check = & $at 'display-guard\.ps1"" -Action check'
    $vendor = & $at 'vdd-setup-x64\.exe'
    $tasks = & $at 'vdd-tasks-setup\.ps1'
    if ($save -lt 0 -or $check -lt 0 -or !($save -lt $vendor -and $vendor -lt $tasks -and $tasks -lt $check)) {
        $failures += "Setup does not save before the driver steps and check after them ($save, $vendor, $tasks, $check)"
    }
    foreach ($i in $save, $check) {
        if ($i -ge 0 -and ($run[$i] -match 'Check:|postinstall' -or $run[$i] -notmatch 'logoutput')) {
            $failures += "Setup runs the guard conditionally or without logging it: $($run[$i])"
        }
    }
    # "View README" opens the Quickstart, which Notepad opens on any PC.
    $readme = @(($iss -split "`n") | Where-Object { $_ -match '^Source:.*\bisreadme\b' })
    if ($readme.Count -ne 1 -or $readme[0] -notmatch 'QUICKSTART\.txt') { $failures += "isreadme is on: $readme" }

    if ($failures) { throw "Display guard checks failed:`n  " + ($failures -join "`n  ") }
    Write-Output 'PASS: healthy, monitor unplugged, other screen, all lost, virtual only, back by itself, refused with panel on, taken with no screen, panel off, one attempt, unknown, failing call, never fails as run by Setup, Setup wiring, readme'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
