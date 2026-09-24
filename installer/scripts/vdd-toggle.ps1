# Enables or disables the bundled Virtual Display Driver device. Invoked by the
# "EternalMonitor VDD Enable" / "EternalMonitor VDD Disable" scheduled tasks (which run as
# SYSTEM with highest privileges), so the non-elevated EternalMonitor host can flip the virtual
# display on/off via `schtasks /Run` without a UAC prompt.
#
# The device is resolved AT TRIGGER TIME rather than baked in at install time, so it stays
# correct across driver versions, friendly-name changes, and PnP enumeration order. The
# VirtualDrivers/Virtual-Display-Driver enumerates under ROOT\DISPLAY and reports a friendly
# name containing "Virtual Display", so we match on either — a real monitor is never ROOT\.
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('enable', 'disable')]
    [string]$Action
)

$ErrorActionPreference = 'Stop'

$devices = @(Get-PnpDevice -Class Display -PresentOnly | Where-Object {
    $ids = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds').Data
    $ids -contains 'Root\MttVDD'
})

if ($devices.Count -eq 0) {
    if ($Action -eq 'disable') { exit 0 }
    throw 'The bundled Virtual Display Driver device is not present. Reinstall EternalMonitor with its display driver.'
}

foreach ($device in $devices) {
    # PnPUtil can report this root device as disconnected even when its
    # present devnode is disabled. The PnpDevice cmdlets address that devnode.
    if ($Action -eq 'enable') {
        Enable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false
    } else {
        Disable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false
    }
    $actual = Get-PnpDevice -InstanceId $device.InstanceId
    $expectedProblem = if ($Action -eq 'enable') { 0 } else { 22 }
    if ([int]$actual.Problem -ne $expectedProblem) {
        throw "Virtual display $Action failed: $($actual.InstanceId) reports $($actual.Problem)"
    }
    Write-Output "Virtual display $Action completed: $($actual.InstanceId)"
}
