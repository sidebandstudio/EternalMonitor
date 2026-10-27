# Enables or disables the bundled Virtual Display Driver device. Invoked by the
# "EternalMonitor VDD Enable" / "EternalMonitor VDD Disable" scheduled tasks (which run as
# SYSTEM with highest privileges), so the non-elevated EternalMonitor host can flip the virtual
# display on/off via `schtasks /Run` without a UAC prompt.
#
# The device is resolved AT TRIGGER TIME rather than baked in at install time, so it stays
# correct across driver versions, friendly-name changes, and PnP enumeration order. The
# VirtualDrivers/Virtual-Display-Driver enumerates under ROOT\DISPLAY and reports a friendly
# name containing "Virtual Display", so we match on either — a real monitor is never ROOT\.
#
# Enable fails when the display cannot appear: no device, no driver bound, or a Windows problem
# code. The exit code says why, and the host reads it as the task's result (host/src/vdd_device.rs):
#   2 no device   3 no driver bound   100 + N   Windows problem code N (122: still disabled)
# A device that has not started yet or has no monitor yet still succeeds: its monitor can arrive
# later, and the host waits for the display itself and reads the device's state if it never comes.
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
    [Console]::Error.WriteLine('The bundled Virtual Display Driver device is not present. Run EternalMonitor Setup again to reinstall it.')
    exit 2
}

# Why an enabled device cannot show a display yet, or $null when it can. Each PnP call takes about
# half a second while the device starts, so the check makes two: the device's properties, then the
# device and its monitors together.
function Get-EnableProblem($instance) {
    $properties = @(Get-PnpDeviceProperty -InstanceId $instance -KeyName 'DEVPKEY_Device_DriverInfPath', 'DEVPKEY_Device_Children')
    $inf = ($properties | Where-Object KeyName -eq 'DEVPKEY_Device_DriverInfPath').Data
    if ($inf -notmatch '^oem[0-9]+\.inf$') { return @{Code=3; Text="$instance has no driver bound"} }
    $children = @(($properties | Where-Object KeyName -eq 'DEVPKEY_Device_Children').Data | Where-Object { $_ })
    $found = @(Get-PnpDevice -InstanceId (@($instance) + $children))
    $device = $found | Where-Object InstanceId -eq $instance
    if ([int]$device.Problem -ne 0) {
        return @{Code=100 + [int]$device.Problem; Text="$instance reports problem code $([int]$device.Problem)"}
    }
    if ($device.Status -ne 'OK') { return @{Code=4; Text="$instance has not started"} }
    $monitor = $found | Where-Object { $_.InstanceId -ne $instance -and $_.Status -eq 'OK' }
    if (!$monitor) { return @{Code=5; Text="$instance started, but no monitor arrived"} }
    return $null
}

foreach ($device in $devices) {
    # PnPUtil can report this root device as disconnected even when its
    # present devnode is disabled. The PnpDevice cmdlets address that devnode.
    if ($Action -eq 'enable') {
        # A device the Disable task turned off reports code 22 until this runs.
        Enable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false
        $problem = Get-EnableProblem $device.InstanceId
        if ($problem -and $problem.Code -in 4, 5) {
            Write-Output "Virtual display enabled; $($problem.Text) yet"
        } elseif ($problem) {
            [Console]::Error.WriteLine("Virtual display enable failed: $($problem.Text)")
            exit $problem.Code
        }
    } else {
        Disable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false
        $actual = Get-PnpDevice -InstanceId $device.InstanceId
        if ([int]$actual.Problem -ne 22) {
            throw "Virtual display disable failed: $($actual.InstanceId) reports $($actual.Problem)"
        }
    }
    Write-Output "Virtual display $Action completed: $($device.InstanceId)"
}
