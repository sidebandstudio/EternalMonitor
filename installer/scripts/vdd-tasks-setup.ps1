# Registers the EternalMonitor virtual-display enable/disable scheduled tasks and leaves the
# device DISABLED by default. Run elevated by the installer.
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

$service = New-Object -ComObject Schedule.Service
$service.Connect()
$folder = $service.GetFolder('\')

# Replacing the definitions does not establish that an old instance went away.
# Stop all instances before replacing either definition.
foreach ($name in $names) {
    try {
        $task = $folder.GetTask($name)
        $task.Stop(0)
        $deadline = (Get-Date).AddSeconds(5)
        while ($folder.GetTask($name).State -in @(2, 4)) {
            if ((Get-Date) -ge $deadline) { throw "Previous VDD task did not stop: $name" }
            Start-Sleep -Milliseconds 50
        }
    } catch {
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

# Setup checks this separately from the device toggle, which requires a working driver.
if ($ResultMarker) { [IO.File]::WriteAllText($ResultMarker, 'verified') }

# Off by default — the host turns it on only while streaming to it (and only once an iPad
# connects). Resolve + disable via the same toggle script so the logic stays in one place.
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$toggle" -Action disable | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'The virtual display could not be disabled after task registration.' }

Write-Output "Registered EternalMonitor VDD tasks (device resolved at trigger time; left disabled)."
