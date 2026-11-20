# Script checks for host/src/vdd_task.ps1. Task Scheduler is replaced in
# a child process, so no real task runs. The host runs the script the same way:
# `& { <script> } -Task <name> -After <other direction>`.
$ErrorActionPreference = 'Stop'
$script = Get-Content -Raw (Join-Path $PSScriptRoot '..\..\host\src\vdd_task.ps1')
$mock = @'
# $EMBusy[name]: how many more state reads report that task as running.
# $EMMissing: tasks that do not exist, as Task Scheduler reports them.
# $EMRunState: the state of the instance Run starts (3: it has finished).
function New-Object {
    param([string]$ComObject, [Parameter(ValueFromRemainingArguments=$true)]$Rest)
    if ($ComObject -ne 'Schedule.Service') { throw "Unexpected COM object $ComObject" }
    $service = [pscustomobject]@{}
    $service | Add-Member ScriptMethod Connect {}
    $service | Add-Member ScriptMethod GetFolder {
        param($path)
        $folder = [pscustomobject]@{}
        $folder | Add-Member ScriptMethod GetTask {
            param($name)
            if ($global:EMMissing -contains $name) {
                throw [System.Runtime.InteropServices.COMException]::new(
                    'The system cannot find the file specified. (Exception from HRESULT: 0x80070002)', -2147024894)
            }
            $task = [pscustomobject]@{ Name = $name; Enabled = $true; LastTaskResult = 0
                Definition = [pscustomobject]@{ Settings = [pscustomobject]@{ AllowDemandStart = $true } } }
            $task | Add-Member ScriptProperty State {
                if ($global:EMBusy[$this.Name] -gt 0) { $global:EMBusy[$this.Name]--; 4 } else { 3 }
            }
            $task | Add-Member ScriptMethod Run {
                param($arguments)
                $busy = ($global:EMBusy.Keys | Where-Object { $global:EMBusy[$_] -gt 0 }) -join ','
                [Console]::Out.WriteLine("RUN $($this.Name) busy=[$busy]")
                $running = [pscustomobject]@{ State = $global:EMRunState }
                $running | Add-Member ScriptMethod Refresh {}
                $running
            }
            $task
        }
        $folder
    }
    $service
}
'@

function Invoke-Case([hashtable]$busy, [string]$arguments, [string[]]$missing = @(), [int]$runState = 3) {
    $file = Join-Path $env:TEMP ('em-vdd-task-test-' + [guid]::NewGuid().ToString('N') + '.ps1')
    $busyText = ($busy.GetEnumerator() | ForEach-Object { "'$($_.Key)' = $($_.Value)" }) -join '; '
    $missingText = ($missing | ForEach-Object { "'$_'" }) -join ', '
    $body = "`$global:EMBusy = @{ $busyText }`n`$global:EMMissing = @($missingText)`n`$global:EMRunState = $runState`n$mock`n& {`n$script`n} $arguments`n"
    Set-Content -Path $file -Value $body -Encoding UTF8
    # The child's stderr is expected output here, not a failure of this script.
    $ErrorActionPreference = 'Continue'
    try {
        $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $file 2>&1 | Out-String
        [pscustomobject]@{ Code = $LASTEXITCODE; Output = $output }
    } finally { Remove-Item $file -ErrorAction SilentlyContinue }
}

# A Disable waits for an Enable still running in Task Scheduler, then runs.
$case = Invoke-Case @{ Enable = 20 } "-Task 'Disable' -After 'Enable'"
if ($case.Code -ne 0 -or $case.Output -notmatch 'RUN Disable busy=\[\]') {
    throw "Disable did not wait for the running Enable: $($case.Output)"
}
# It also still waits for an earlier Disable.
$case = Invoke-Case @{ Disable = 20 } "-Task 'Disable' -After 'Enable'"
if ($case.Code -ne 0 -or $case.Output -notmatch 'RUN Disable busy=\[\]') {
    throw "Disable did not wait for the earlier Disable: $($case.Output)"
}
# An Enable that never finishes ends the wait within its bound, without
# starting the Disable, and reads as a timeout to the host.
$started = Get-Date
$case = Invoke-Case @{ Enable = 1000000 } "-Task 'Disable' -After 'Enable'"
$took = ((Get-Date) - $started).TotalSeconds
if ($case.Code -ne 1 -or $case.Output -notmatch 'Previous VDD task timed out: Enable \(in progress\)' -or $case.Output -match 'RUN') {
    throw "A hung Enable was not reported as a timeout: $($case.Output)"
}
# The script's own wait is bounded at 10 s; a cold PowerShell start for the
# child can take tens of seconds more on a busy PC. A wait that never ended
# would take hours with this mock.
if ($took -gt 120) { throw "The wait took $took s" }
# An instance Task Scheduler never starts reads as queued, so the host's log
# tells it apart from a toggle that hangs.
$case = Invoke-Case @{} "-Task 'Disable' -After 'Enable'" @() 2
if ($case.Code -ne 1 -or $case.Output -notmatch 'VDD task timed out: Disable \(queued\)') {
    throw "A queued instance was not reported as one: $($case.Output)"
}
# Without -After, as older hosts call it, only this task is waited for.
$case = Invoke-Case @{ Enable = 1000000 } "-Task 'Disable'"
if ($case.Code -ne 0 -or $case.Output -notmatch 'RUN Disable') {
    throw "A call without -After waited for another task: $($case.Output)"
}
# An install without the other task still runs the requested one.
$case = Invoke-Case @{} "-Task 'Disable' -After 'Enable'" @('Enable')
if ($case.Code -ne 0 -or $case.Output -notmatch 'RUN Disable') {
    throw "A missing -After task stopped the requested one: $($case.Output)"
}
# A missing requested task still fails, as Missing for the host.
$case = Invoke-Case @{} "-Task 'Disable' -After 'Enable'" @('Disable')
if ($case.Code -ne 1 -or $case.Output -notmatch '0x80070002' -or $case.Output -match 'RUN') {
    throw "A missing requested task was not reported: $($case.Output)"
}
Write-Output 'VDD task checks passed: waits for the other direction, earlier runs, hung Enable bound, queued instance, no -After, missing tasks'

# Run the real Setup registration on the Windows CI runner. Use an inert
# toggle so Setup's final direct disable cannot touch a display device.
$names = 'EternalMonitor VDD Enable', 'EternalMonitor VDD Disable'
foreach ($name in $names) {
    if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
        throw "Refusing to replace existing scheduled task '$name' in the test."
    }
}
$fixture = Join-Path $env:TEMP ('em-vdd-task-setup-test-' + [guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $fixture | Out-Null
    Copy-Item (Join-Path $PSScriptRoot '..\..\installer\scripts\vdd-tasks-setup.ps1') $fixture
    Set-Content -Path (Join-Path $fixture 'vdd-toggle.ps1') -Value 'param([string]$Action); exit 0'
    & (Join-Path $fixture 'vdd-tasks-setup.ps1')
    foreach ($name in $names) {
        $settings = (Get-ScheduledTask -TaskName $name).Settings
        if ($settings.DisallowStartIfOnBatteries -ne $false -or $settings.StopIfGoingOnBatteries -ne $false) {
            throw "'$name' must be allowed to start and keep running on battery."
        }
    }
    Write-Output 'Setup registered both tasks with battery power allowed.'
} finally {
    foreach ($name in $names) {
        if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
            Unregister-ScheduledTask -TaskName $name -Confirm:$false
        }
    }
    Remove-Item -LiteralPath $fixture -Recurse -Force -ErrorAction SilentlyContinue
}
# Some cases end their child with exit code 1 on purpose. Without this the
# last one stays in $LASTEXITCODE, and a CI step that ends with it fails.
# Every failure above throws, which exits 1.
exit 0
