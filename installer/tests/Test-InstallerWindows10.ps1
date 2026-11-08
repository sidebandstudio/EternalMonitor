# Static checks for Setup on Windows 10, where Second screen needs Windows 11: a fresh install
# skips the Virtual Display Driver and its tasks, and an upgrade keeps a driver an earlier
# version installed, with its tasks, without repairing or verifying it. Windows 11 is
# unchanged. Nothing is compiled or installed. The checks also run against broken copies of
# the script, which they must reject.
param([string]$IssPath)
$ErrorActionPreference = 'Stop'
$source = if ($IssPath) { $IssPath } else { Join-Path $PSScriptRoot '..\EternalMonitor.iss' }
$iss = Get-Content -Raw -Encoding UTF8 $source

# One [Code] routine, from its header to its closing "end;".
function Get-Routine([string]$Text, [string]$Header) {
    [regex]::Match($Text, '(?s)\n' + [regex]::Escape($Header) + '.*?\nend;').Value
}

function Get-Failures([string]$Text) {
    $failures = @()
    if ($Text -notmatch '(?m)^\s*FirstWindows11Build = 22000;') {
        $failures += 'The first Windows 11 build is not 22000'
    }
    $runs = Get-Routine $Text 'function WindowsRunsVdd: Boolean;'
    if ($runs -notmatch 'GetWindowsVersionEx\((\w+)\);' -or
        $runs -notmatch "Result := $($Matches[1])\.Build >= FirstWindows11Build;") {
        $failures += 'WindowsRunsVdd does not compare the Windows build with the first Windows 11 build'
    }

    # Fresh install: a registered driver is kept on every Windows; with none, only Windows 11
    # installs one.
    $needs = Get-Routine $Text 'function NeedsVddInstall: Boolean;'
    if ($needs -notmatch '(?s)Result := not ReadVddRegistration\(Directory, Version\);.*else if not WindowsRunsVdd then begin.*Result := False;') {
        $failures += 'NeedsVddInstall installs the driver on Windows 10'
    }
    if ($Text -notmatch '(?m)^Filename: "\{app\}\\driver\\vdd-setup-x64\.exe";.*Check: NeedsVddInstall;') {
        $failures += 'The vendor setup does not run under NeedsVddInstall'
    }

    # The tasks: wherever a driver is installed, and on Windows 11.
    $wanted = Get-Routine $Text 'function VddTasksWanted: Boolean;'
    if ($wanted -notmatch 'Result := VddInstallSucceeded and \(WindowsRunsVdd or ReadVddRegistration\(Directory, Version\)\);') {
        $failures += 'VddTasksWanted does not keep the tasks for an installed driver only'
    }
    $tasks = $Text -split "\r?\n" | Where-Object { $_ -match '^Filename: "powershell\.exe";.*vdd-tasks-setup\.ps1' }
    if (@($tasks).Count -ne 1 -or $tasks -notmatch 'Check: VddTasksWanted;') {
        $failures += 'The driver tasks are set up without VddTasksWanted'
    }

    # A driver kept on Windows 10 is neither repaired nor checked: no device changes, and no
    # failed Setup over a driver Windows 10 doesn't use.
    foreach ($routine in 'procedure RepairVddDriver;', 'procedure VerifyVddDriver;') {
        $body = Get-Routine $Text $routine
        $first = [regex]::Match($body, '(?s)\nbegin\s*(?:\{[^}]*\}\s*)*(?<first>if not WindowsRunsVdd then(?: begin)?\s*(?:Log\([^;]*\);\s*)?exit;)')
        if (!$first.Success) { $failures += "$($routine.Split(' ')[1].TrimEnd(';')) runs on Windows 10" }
    }
    $failures
}

$failures = @(Get-Failures $iss)
if ($failures) { throw "Windows 10 checks failed:`n  " + ($failures -join "`n  ") }

# Each broken copy must fail.
$broken = [ordered]@{
    'Windows 10 build' = $iss.Replace('FirstWindows11Build = 22000;', 'FirstWindows11Build = 19041;')
    'build not read' = $iss.Replace('Result := Windows.Build >= FirstWindows11Build;', 'Result := True;')
    'fresh install on Windows 10' = $iss.Replace("  else if not WindowsRunsVdd then begin`r`n", "  else if False then begin`r`n").Replace("  else if not WindowsRunsVdd then begin`n", "  else if False then begin`n")
    'vendor setup unchecked' = $iss.Replace('Check: NeedsVddInstall;', 'Check: VddInstallSucceeded;')
    'tasks without a driver' = $iss.Replace('(WindowsRunsVdd or ReadVddRegistration(Directory, Version))', '(True or ReadVddRegistration(Directory, Version))')
    'tasks unchecked' = $iss.Replace('Check: VddTasksWanted;', 'Check: VddInstallSucceeded;')
    'repair on Windows 10' = [regex]::Replace($iss, '(?s)(procedure RepairVddDriver;.*?\nbegin.*?)if not WindowsRunsVdd then begin', '$1if False then begin')
    'verify on Windows 10' = [regex]::Replace($iss, '(?s)(procedure VerifyVddDriver;.*?\nbegin.*?)if not WindowsRunsVdd then', '$1if False then')
}
foreach ($case in $broken.Keys) {
    if ($broken[$case] -ceq $iss) { throw "The '$case' copy is the real script; update this test" }
    if (!@(Get-Failures $broken[$case])) { throw "The checks pass a broken script: $case" }
}
Write-Output "PASS: Windows 10 skips a fresh driver install and its tasks, keeps an installed driver unrepaired and unchecked; $($broken.Count) broken copies rejected"
