# Registers the EternalMonitor virtual-display enable/disable scheduled tasks.
# Run elevated by the installer, which disables the device after registration.
#
# The tasks run as SYSTEM with highest privileges, so the (non-elevated) EternalMonitor host
# can flip the virtual display on/off via `schtasks /Run` without a UAC prompt. The task names
# must match host/src/vdd.rs.
#
# Each task invokes vdd-toggle.ps1, which resolves the VDD device at TRIGGER time. We deliberately
# do NOT bake a device instance id into the tasks here — baking a guessed id (the old
# ROOT\DISPLAY\0000 fallback) breaks whenever the driver enumerates under a different id or
# reports a different friendly name.

param(
    [ValidatePattern('^[A-Za-z0-9 -]+$')]
    [string]$TaskNamePrefix = 'EternalMonitor VDD',
    [string]$ResultMarker
)
$ErrorActionPreference = 'Stop'
$names = "$TaskNamePrefix Enable", "$TaskNamePrefix Disable"

$toggle = Join-Path $PSScriptRoot 'vdd-toggle.ps1'
if (-not (Test-Path $toggle)) {
    Write-Error "vdd-toggle.ps1 not found next to this script ($toggle); cannot register VDD tasks."
    exit 1
}

$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Minutes 2)

# Stop all instances before replacing either definition. The in-app updater
# waits for the host to exit. Interactive Setup offers to close it through
# Restart Manager, and the user can decline.
# Share ten seconds across both tasks. A Scheduler call that never returns
# can still hold Setup, as can registration and the later PnP disable.
$stopWatch = [Diagnostics.Stopwatch]::StartNew()
function Assert-VddStopTime {
    if ($stopWatch.ElapsedMilliseconds -ge 10000) {
        throw 'Timed out stopping previous virtual display tasks.'
    }
}

Assert-VddStopTime
$service = New-Object -ComObject Schedule.Service
Assert-VddStopTime
$service.Connect()
Assert-VddStopTime
$folder = $service.GetFolder('\')
Assert-VddStopTime
foreach ($name in $names) {
    try {
        Assert-VddStopTime
        $task = $folder.GetTask($name)
        Assert-VddStopTime
        $task.Stop(0)
        Assert-VddStopTime
        while ($true) {
            $state = $task.State
            Assert-VddStopTime
            if ($state -notin @(2, 4)) { break }
            Start-Sleep -Milliseconds 50
            Assert-VddStopTime
        }
    } catch {
        Assert-VddStopTime
        # ERROR_FILE_NOT_FOUND and SCHED_E_TASK_NOT_RUNNING are expected if
        # the task is absent or its last instance has already finished.
        if ($_.Exception.GetBaseException().HResult -notin @(-2147024894, -2147216629)) { throw }
    }
}

function Register-VddTask([string]$Name, [string]$ToggleAction) {
    $argument = "-NoProfile -ExecutionPolicy Bypass -File `"$toggle`" -Action $ToggleAction"
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $argument
    Register-ScheduledTask -TaskPath '\' -TaskName $Name -Action $action -Principal $principal -Settings $settings -Force | Out-Null
}

Register-VddTask $names[0] 'enable'
Register-VddTask $names[1] 'disable'

# Let a normal host process read/run the fixed SYSTEM tasks. Users receive no
# permission to change the task action or its protected Program Files script.
foreach ($name in $names) {
    $task = $folder.GetTask($name)
    $task.SetSecurityDescriptor('D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;BU)', 0)
    $actual = $task.Definition.Settings
    if ($actual.DisallowStartIfOnBatteries -ne $false -or $actual.StopIfGoingOnBatteries -ne $false) {
        throw "Scheduled task '$name' did not retain its battery settings."
    }
}

if ($ResultMarker) { [IO.File]::WriteAllText($ResultMarker, 'verified') }

Write-Output 'Registered and verified both virtual display tasks.'
