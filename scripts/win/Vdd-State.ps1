param([switch]$Disable)
$ErrorActionPreference = 'Stop'
$instance = 'ROOT\DISPLAY\0000'
if ($Disable) {
    & schtasks /Run /TN 'EternalMonitor VDD Disable'
    if ($LASTEXITCODE -ne 0) { throw 'VDD cleanup task failed' }
    $deadline = (Get-Date).AddSeconds(20)
    do {
        $code = (Get-PnpDeviceProperty -InstanceId $instance -KeyName 'DEVPKEY_Device_ProblemCode').Data
        if ($code -eq 22) { break }
        if ((Get-Date) -gt $deadline) { throw 'VDD did not become disabled' }
        Start-Sleep -Milliseconds 200
    } while ($true)
}
$device = Get-PnpDevice -InstanceId $instance
$code = (Get-PnpDeviceProperty -InstanceId $instance -KeyName 'DEVPKEY_Device_ProblemCode').Data
$xml = if (Test-Path 'C:\VirtualDisplayDriver\vdd_settings.xml') { Get-Content 'C:\VirtualDisplayDriver\vdd_settings.xml' -Raw } else { '' }
@{instance=$instance; status=$device.Status; problem_code=$code; disabled=($code -eq 22); settings_xml=$xml} | ConvertTo-Json -Compress
