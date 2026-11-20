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

    # A driver failure must never skip task registration. Keep the existing
    # repair/verify path gated by the failure state before registration.
    $tasks = $Text -split "\r?\n" | Where-Object { $_ -match '^Filename: "powershell\.exe";.*vdd-tasks-setup\.ps1' }
    if (@($tasks).Count -ne 1 -or $tasks -match 'Check:|skipifsilent|postinstall' -or
        $tasks -notmatch 'BeforeInstall: PrepareVddTasks; AfterInstall: FinishVddTasks') {
        $failures += 'Setup does not register and check the tasks on every driver-bundled install'
    }
    $prepare = Get-Routine $Text 'procedure PrepareVddTasks;'
    if ($prepare -notmatch '(?s)begin\s*VddFailedBeforeTasks := VddInstallFailed;\s*if not VddFailedBeforeTasks then\s*RepairVddDriver;\s*end;') {
        $failures += 'Setup does not repair the driver when the earlier driver step succeeded'
    }
    $finish = Get-Routine $Text 'procedure FinishVddTasks;'
    $runMarker = [regex]::Match(($tasks -join "`n"), '-ResultMarker ""([^"]+)""')
    $checkedMarker = [regex]::Match($finish, "Marker := ExpandConstant\('([^']+)'\);")
    if (!$runMarker.Success -or !$checkedMarker.Success -or
        $runMarker.Groups[1].Value -cne $checkedMarker.Groups[1].Value) {
        $failures += 'The task [Run] entry and its completion check use different marker paths'
    }
    $runner = Get-Routine $Text 'procedure RunVddTaskStep(const Script, Args, Step: String);'
    if ($finish -notmatch "(?s)if not FileExists\(Marker\) then begin.*?RunVddTaskStep\('vdd-tasks-setup\.ps1', '-ResultMarker `"' \+ Marker" -or
        $runner -notmatch '(?s)ExecAndLogOutput\(.*?ewWaitUntilTerminated' -or
        $finish -notmatch '(?s)VddTasksFailed := not FileExists\(Marker\);\s*if VddTasksFailed then\s*Log\([^\n]*after two attempts' -or
        $finish -notmatch '(?s)if not VddFailedBeforeTasks then\s*VerifyVddDriver;\s*end;' -or
        $finish -match 'FailVddStep\(|VddInstallFailed\s*:=|RaiseException\(') {
        $failures += 'Setup does not retry unverified registration once and log the result independently of the driver'
    }
    $exitCode = Get-Routine $Text 'function GetCustomSetupExitCode: Integer;'
    if ($exitCode -notmatch '(?s)Result := 0;\s*if VddInstallFailed then\s*Result := 20\s*else if VddTasksFailed and WizardSilent then\s*Result := 21;') {
        $failures += 'Setup does not report task failure only to a silent caller or preserve driver failure priority'
    }
    if ($finish -notmatch "(?s)Log\('Both virtual display tasks were verified\.'\);\s*(?:\{[^}]*\}\s*)*RunVddTaskStep\('vdd-toggle\.ps1', '-Action disable'" -or
        [regex]::Matches($finish, "RunVddTaskStep\('vdd-toggle\.ps1'").Count -ne 1) {
        $failures += 'Setup does not attempt one device disable after the task registration outcome'
    }

    # A failed step fails Setup on Windows 11 only (build 22000 and later): it sets the failure
    # exit code and raises. On Windows 10 it is logged and Setup goes on.
    if ($fail -notmatch '(?s)\nbegin\s*if not IsWindows11 then begin\s*Log\([^;]*\);\s*exit;\s*end;\s*VddInstallFailed := True;\s*RaiseException\(Message\);\s*end;$') {
        $failures += 'FailVddStep does not fail Setup on Windows 11 only'
    }

    # Repair and verify run their scripts on every Windows, with nothing before them, and hand
    # every failure to FailVddStep: a script that can't start or exits non-zero (repair: any
    # code but 0, with its message for 10 and 11).
    $steps = [ordered]@{
        'procedure RepairVddDriver;' = '(?s)\nbegin\s*(?:\{[^}]*\}\s*)*if not Exec\([^;]*?vdd-driver-repair\.ps1[^;]*?, ewWaitUntilTerminated, ExitCode\) then\s*ExitCode := -1;\s*if ExitCode = 0 then\s*exit;\s*case ExitCode of\s*10: Message := [^\n]*\n\s*11: Message := [^\n]*\n\s*else\s*Message := [^\n]*\n\s*end;\s*FailVddStep\(Message\);\s*end;$'
        'procedure VerifyVddDriver;' = '(?s)\nbegin\s*(?:\{[^}]*\}\s*)*if not Exec\([^;]*?vdd-driver-verify\.ps1[^;]*?, ewWaitUntilTerminated, ExitCode\) or \(ExitCode <> 0\) then\s*FailVddStep\(''[^\n]*\);\s*end;$'
    }
    foreach ($routine in $steps.Keys) {
        if ((Get-Routine $Text $routine) -notmatch $steps[$routine]) {
            $failures += "$($routine.Split(' ')[1].TrimEnd(';')) does not run its script on every Windows and hand each failure to FailVddStep"
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
    'tasks on Windows 11 only' = $iss.Replace('BeforeInstall: PrepareVddTasks;', 'Check: IsWindows11; BeforeInstall: PrepareVddTasks;')
    'tasks skipped after a driver failure' = $iss.Replace('BeforeInstall: PrepareVddTasks;', 'Check: VddInstallSucceeded; BeforeInstall: PrepareVddTasks;')
    'retry skipped' = $iss.Replace('if not FileExists(Marker) then begin', 'if False then begin')
    'retry failure not logged' = $iss.Replace('could not be verified after two attempts', 'done')
    'marker paths differ' = $iss.Replace("Marker := ExpandConstant('{tmp}\vdd-tasks-verified');", "Marker := ExpandConstant('{tmp}\other-marker');")
    'task exit code missing' = $iss.Replace('Result := 21;', 'Result := 0;')
    'task exit code during interactive setup' = $iss.Replace('VddTasksFailed and WizardSilent', 'VddTasksFailed')
    'disable skipped after failed registration' = $iss.Replace("  RunVddTaskStep('vdd-toggle.ps1'", "  if not VddTasksFailed then RunVddTaskStep('vdd-toggle.ps1'")
    'repair skipped on Windows 10' = Add-AfterBegin $iss 'procedure RepairVddDriver;' '  if not IsWindows11 then exit;'
    'verify skipped on Windows 10' = Add-AfterBegin $iss 'procedure VerifyVddDriver;' '  if not IsWindows11 then exit;'
    'repair skipped' = Add-AfterBegin $iss 'procedure RepairVddDriver;' '  exit;'
    'failure fails Windows 10' = $iss.Replace('  if not IsWindows11 then begin', '  if False then begin')
    'failure passes Windows 11' = [regex]::Replace($iss, '\r?\n  RaiseException\(Message\);', '')
    'verify fails by itself' = $iss.Replace("FailVddStep('Windows did not finish", "RaiseException('Windows did not finish")
    'repair fails by itself' = $iss.Replace('  FailVddStep(Message);', '  VddInstallFailed := True; RaiseException(Message);')
    'cutoff off by one' = $iss.Replace('Result := Windows.Build >= FirstWindows11Build;', 'Result := Windows.Build > FirstWindows11Build;')
    'failure only logged on Windows 11' = $iss.Replace('  if not IsWindows11 then begin', '  if True then begin')
    'failure logged on Windows 11 and fails Windows 10' = $iss.Replace('  if not IsWindows11 then begin', '  if IsWindows11 then begin')
    'failure exit code dropped' = [regex]::Replace($iss, '\r?\n  VddInstallFailed := True;', '')
    'repair accepts failures' = $iss.Replace('  if ExitCode = 0 then', '  if ExitCode <> 0 then')
    'repair that cannot start accepted' = $iss.Replace('    ExitCode := -1;', '    ExitCode := 0;')
    'verify accepts failures' = $iss.Replace('or (ExitCode <> 0) then', 'or (ExitCode = 0) then')
    'verify that cannot start accepted' = [regex]::Replace($iss, '(?s)(procedure VerifyVddDriver;.*?)if not Exec\(', '$1if Exec(')
}
foreach ($case in $broken.Keys) {
    if ($broken[$case] -ceq $iss) { throw "The '$case' copy is the real script; update this test" }
    if (!@(Get-Failures $broken[$case])) { throw "The checks pass a broken script: $case" }
}
Write-Output "PASS: Windows 10 installs the driver and its tasks, repairs and checks it, and a failed step fails Setup on Windows 11 only; $($broken.Count) broken copies rejected"
