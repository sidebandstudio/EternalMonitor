# Static checks for Setup on Windows 10, where Windows adds the virtual display only on some
# PCs: Setup installs the Virtual Display Driver and its tasks, repairs and checks it, as on
# Windows 11, but a failed repair or check is logged instead of failing Setup. Windows 11 is
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
    $isWindows11 = Get-Routine $Text 'function IsWindows11: Boolean;'
    if ($isWindows11 -notmatch 'GetWindowsVersionEx\((\w+)\);' -or
        $isWindows11 -notmatch "Result := $($Matches[1])\.Build >= FirstWindows11Build;") {
        $failures += 'IsWindows11 does not compare the Windows build with the first Windows 11 build'
    }

    # Only a failed driver step depends on the Windows version: nothing is skipped on Windows 10.
    $fail = Get-Routine $Text 'procedure FailVddStep(const Message: String);'
    $rest = $Text
    foreach ($routine in @($isWindows11, $fail) | Where-Object { $_ }) { $rest = $rest.Replace($routine, '') }
    if (!$fail -or $rest -match '\bIsWindows11\b|GetWindowsVersion|WindowsRunsVdd') {
        $failures += 'Setup reads the Windows version outside FailVddStep'
    }

    # A fresh install installs the driver on every Windows; a registered one is kept.
    $needs = Get-Routine $Text 'function NeedsVddInstall: Boolean;'
    if ($needs -notmatch '(?s)\nbegin\s*Result := not ReadVddRegistration\(Directory, Version\);\s*if not Result then\s*Log\([^;]*\);\s*end;$') {
        $failures += 'NeedsVddInstall does not install the driver wherever none is registered'
    }
    if ($Text -notmatch '(?m)^Filename: "\{app\}\\driver\\vdd-setup-x64\.exe";.*Check: NeedsVddInstall; AfterInstall: RecordVddOwnership') {
        $failures += 'The vendor setup does not run under NeedsVddInstall'
    }

    # The tasks: whenever the driver steps have not failed, as in 0.3.4.
    $tasks = $Text -split "\r?\n" | Where-Object { $_ -match '^Filename: "powershell\.exe";.*vdd-tasks-setup\.ps1' }
    if (@($tasks).Count -ne 1 -or $tasks -notmatch 'Check: VddInstallSucceeded; BeforeInstall: RepairVddDriver; AfterInstall: VerifyVddDriver') {
        $failures += 'The driver tasks are not set up under VddInstallSucceeded'
    }

    # A failed step fails Setup on Windows 11 only. On Windows 10 it is logged and Setup goes on.
    if ($fail -notmatch '(?s)\nbegin\s*if not IsWindows11 then begin\s*Log\([^;]*\);\s*exit;\s*end;\s*VddInstallFailed := True;\s*RaiseException\(Message\);\s*end;$') {
        $failures += 'FailVddStep does not fail Setup on Windows 11 only'
    }

    # Repair and verify run on every Windows and report failures through FailVddStep.
    $steps = [ordered]@{
        'procedure RepairVddDriver;' = @('vdd-driver-repair\.ps1', 'FailVddStep\(Message\);')
        'procedure VerifyVddDriver;' = @('vdd-driver-verify\.ps1', "then\s*FailVddStep\('")
    }
    foreach ($routine in $steps.Keys) {
        $name = $routine.Split(' ')[1].TrimEnd(';')
        $body = Get-Routine $Text $routine
        $scriptName, $report = $steps[$routine]
        $start = [regex]::Match($body, '(?s)\nbegin(?<before>.*?)\bExec\(')
        if (!$start.Success -or $start.Groups['before'].Value -match '\bexit\s*;' -or $body -notmatch $scriptName) {
            $failures += "$name does not run its script on every Windows"
        }
        if ($body -notmatch $report -or $body -match 'RaiseException|VddInstallFailed') {
            $failures += "$name does not report its failure through FailVddStep"
        }
    }
    $failures
}

$failures = @(Get-Failures $iss)
if ($failures) { throw "Windows 10 checks failed:`n  " + ($failures -join "`n  ") }

# Each broken copy must fail.
function Add-AfterBegin([string]$Text, [string]$Header, [string]$Line) {
    [regex]::Replace($Text, '(?s)(\n' + [regex]::Escape($Header) + '.*?\nbegin\r?\n)', "`$1$Line`n")
}
$broken = [ordered]@{
    'Windows 10 build' = $iss.Replace('FirstWindows11Build = 22000;', 'FirstWindows11Build = 19041;')
    'build not read' = $iss.Replace('Result := Windows.Build >= FirstWindows11Build;', 'Result := True;')
    'fresh install skipped on Windows 10' = $iss.Replace('Result := not ReadVddRegistration(Directory, Version);', 'Result := IsWindows11 and not ReadVddRegistration(Directory, Version);')
    'vendor setup unchecked' = $iss.Replace('Check: NeedsVddInstall;', 'Check: VddInstallSucceeded;')
    'tasks on Windows 11 only' = $iss.Replace('Check: VddInstallSucceeded; BeforeInstall: RepairVddDriver', 'Check: IsWindows11; BeforeInstall: RepairVddDriver')
    'repair skipped on Windows 10' = Add-AfterBegin $iss 'procedure RepairVddDriver;' '  if not IsWindows11 then exit;'
    'verify skipped on Windows 10' = Add-AfterBegin $iss 'procedure VerifyVddDriver;' '  if not IsWindows11 then exit;'
    'repair skipped' = Add-AfterBegin $iss 'procedure RepairVddDriver;' '  exit;'
    'failure fails Windows 10' = $iss.Replace('  if not IsWindows11 then begin', '  if False then begin')
    'failure passes Windows 11' = [regex]::Replace($iss, '\r?\n  RaiseException\(Message\);', '')
    'verify fails by itself' = $iss.Replace("FailVddStep('Windows did not finish", "RaiseException('Windows did not finish")
    'repair fails by itself' = $iss.Replace('  FailVddStep(Message);', '  VddInstallFailed := True; RaiseException(Message);')
}
foreach ($case in $broken.Keys) {
    if ($broken[$case] -ceq $iss) { throw "The '$case' copy is the real script; update this test" }
    if (!@(Get-Failures $broken[$case])) { throw "The checks pass a broken script: $case" }
}
Write-Output "PASS: Windows 10 installs the driver and its tasks, repairs and checks it, and a failed step fails Setup on Windows 11 only; $($broken.Count) broken copies rejected"
