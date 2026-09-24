param([switch]$Disable)
$ErrorActionPreference = 'Stop'
$devices = @(Get-PnpDevice -Class Display -PresentOnly | Where-Object {
    $ids = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds').Data
    $ids -contains 'Root\MttVDD'
})
if ($devices.Count -ne 1) { throw 'Expected one present Virtual Display Driver device' }
$instance = $devices[0].InstanceId
if ($Disable) {
    & schtasks /Run /TN 'EternalMonitor VDD Disable'
    if ($LASTEXITCODE -ne 0) { throw 'VDD cleanup task failed' }
    $deadline = (Get-Date).AddSeconds(20)
    do {
        if ([int](Get-PnpDevice -InstanceId $instance).Problem -eq 22) { break }
        if ((Get-Date) -gt $deadline) { throw 'VDD did not become disabled' }
        Start-Sleep -Milliseconds 200
    } while ($true)
}
$device = Get-PnpDevice -InstanceId $instance
# After a reboot a disabled root device has no live problem-code property;
# the device object still reports code 22, as the installer's toggle checks.
$code = [int]$device.Problem
$inf = (Get-PnpDeviceProperty -InstanceId $instance -KeyName 'DEVPKEY_Device_DriverInfPath').Data
$version = (Get-PnpDeviceProperty -InstanceId $instance -KeyName 'DEVPKEY_Device_DriverVersion').Data
if ($inf -notmatch '^oem[0-9]+\.inf$' -or !$version) { throw 'VDD device exists but has no bound driver' }
$xml = if (Test-Path 'C:\VirtualDisplayDriver\vdd_settings.xml') { Get-Content 'C:\VirtualDisplayDriver\vdd_settings.xml' -Raw } else { '' }
@{instance=$instance; status=$device.Status; problem_code=$code; driver_inf=$inf; driver_version=$version; disabled=($code -eq 22); settings_xml="$xml"} | ConvertTo-Json -Compress
