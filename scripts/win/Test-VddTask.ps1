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

# The mock cases above also run in a normal shell.
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$user = [Security.Principal.WindowsPrincipal]::new($identity)
if (!$user.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Output 'Skipping real VDD task registration checks: this shell is not elevated.'
    exit 0
}

$id = [guid]::NewGuid().ToString('N')
$prefix = "EternalMonitor VDD Test $id"
$names = "$prefix Enable", "$prefix Disable"
# A task surviving an interrupted test must not run a script a normal user can edit.
$fixture = Join-Path $env:ProgramFiles "em-vdd-task-setup-test-$id"
$created = @()
New-Item -ItemType Directory -Path $fixture | Out-Null
try {
    Copy-Item (Join-Path $PSScriptRoot '..\..\installer\scripts\vdd-tasks-setup.ps1') $fixture
    Set-Content -Path (Join-Path $fixture 'vdd-toggle.ps1') -Value 'param([string]$Action); exit 0'

    # Install older definitions with sleeping actions. Allow starts so this
    # test also works on a laptop already on battery.
    $action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
        -Argument '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 300"'
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $oldSettings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries
    $service = New-Object -ComObject Schedule.Service
    $service.Connect()
    $folder = $service.GetFolder('\')
    foreach ($name in $names) {
        Register-ScheduledTask -TaskPath '\' -TaskName $name -Action $action -Principal $principal -Settings $oldSettings | Out-Null
        $created += $name
        Start-ScheduledTask -TaskPath '\' -TaskName $name
    }
    $deadline = (Get-Date).AddSeconds(30)
    foreach ($name in $names) {
        while ($folder.GetTask($name).State -ne 4) {
            if ((Get-Date) -ge $deadline) { throw "The sleeping test task '$name' did not start." }
            Start-Sleep -Milliseconds 100
        }
    }

    $marker = Join-Path $fixture 'verified'
    & (Join-Path $fixture 'vdd-tasks-setup.ps1') -TaskNamePrefix $prefix -ResultMarker $marker
    foreach ($name in $names) {
        $task = $folder.GetTask($name)
        if ($task.State -in @(2, 4) -or $task.GetInstances(0).Count -ne 0) {
            throw "An instance of '$name' survived task registration."
        }
        $settings = (Get-ScheduledTask -TaskPath '\' -TaskName $name).Settings
        if ($settings.DisallowStartIfOnBatteries -ne $false -or $settings.StopIfGoingOnBatteries -ne $false) {
            throw "'$name' must be allowed to start and keep running on battery."
        }
    }
    if (!(Test-Path -LiteralPath $marker)) { throw 'Setup did not record verified task registration.' }
    Write-Output 'Setup stopped both old task instances and registered both tasks with battery power allowed.'
} finally {
    $cleanupErrors = @()
    foreach ($name in $created) {
        Stop-ScheduledTask -TaskPath '\' -TaskName $name -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskPath '\' -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue -ErrorVariable +cleanupErrors
    }
    Remove-Item -LiteralPath $fixture -Recurse -Force -ErrorAction SilentlyContinue
    if ($cleanupErrors) { throw "Could not remove the VDD test tasks: $cleanupErrors" }
}
# Some cases end their child with exit code 1 on purpose. Without this the
# last one stays in $LASTEXITCODE, and a CI step that ends with it fails.
# Every failure above throws, which exits 1.
exit 0
