# A vendor setup exit code and uninstall registration do not prove that Windows
# accepted the driver. Publisher approval can fail in session 0 while the vendor
# installer returns success and leaves an unbound devnode.
$ErrorActionPreference = 'Stop'
$devices = @(Get-PnpDevice -Class Display -PresentOnly | Where-Object {
    $ids = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds').Data
    $ids -contains 'Root\MttVDD'
})
if ($devices.Count -eq 0) { throw 'The Virtual Display Driver device is absent.' }
foreach ($device in $devices) {
    $inf = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data
    $version = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverVersion').Data
    if ($inf -notmatch '^oem[0-9]+\.inf$' -or !$version) {
        throw 'Windows has not bound the Virtual Display Driver. Run setup in an interactive Windows session and approve its publisher prompt.'
    }
    # The on-demand display normally remains disabled until an iPad connects.
    if ([int]$device.Problem -notin @(0,22)) {
        throw "Virtual Display Driver reports device problem $($device.Problem)."
    }
    Write-Output "Virtual Display Driver verified: $($device.InstanceId), $inf, $version"
}
