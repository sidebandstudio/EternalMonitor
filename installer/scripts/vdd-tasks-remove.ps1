# Removes the EternalMonitor virtual-display scheduled tasks and DISABLES the device before the
# driver is uninstalled. Run elevated on uninstall.
#
# We disable (not enable) the device here so that if the subsequent driver uninstall step fails
# partway, the VDD can't be left behind as a phantom monitor. If the driver uninstall succeeds the
# device disappears anyway.

$ErrorActionPreference = 'SilentlyContinue'

foreach ($task in 'EternalMonitor VDD Enable', 'EternalMonitor VDD Disable') {
    Unregister-ScheduledTask -TaskName $task -Confirm:$false
}

& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'vdd-toggle.ps1') -Action disable
if ($LASTEXITCODE -ne 0) { throw 'The virtual display could not be disabled before uninstall.' }
