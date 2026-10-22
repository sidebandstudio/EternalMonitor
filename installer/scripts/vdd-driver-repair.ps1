# Puts back the Virtual Display Driver's device when its vendor install is still registered.
# Setup keeps an existing driver instead of running the vendor setup again, so a device removed
# in Device Manager, or left without its driver or with a driver error, stayed broken through
# every reinstall. This repeats the vendor's own install.bat steps with its installed files.
# Run elevated by the installer, before the scheduled tasks disable the device again.
$ErrorActionPreference = 'Stop'
$hardwareId = 'Root\MttVDD'
$classGuid = '4D36E968-E325-11CE-BFC1-08002BE10318'

function Get-VddDevices {
    @(Get-PnpDevice -Class Display -PresentOnly | Where-Object {
        $ids = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds').Data
        $ids -contains $hardwareId
    })
}

# Bound to an installed driver package, and either off (code 22, between streams) or running.
function Test-Healthy($device) {
    $inf = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_DriverInfPath').Data
    ($inf -match '^oem[0-9]+\.inf$') -and ([int]$device.Problem -in @(0, 22))
}

$devices = @(Get-VddDevices)
$broken = @($devices | Where-Object { !(Test-Healthy $_) })
if ($devices.Count -gt 0 -and $broken.Count -eq 0) {
    Write-Output 'Virtual Display Driver device is installed.'
    return
}

$registration = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1' -ErrorAction SilentlyContinue |
    Where-Object { $_.InstallLocation } | Select-Object -First 1
$nefcon = if ($registration) { Join-Path $registration.InstallLocation 'nefconw.exe' }
$inf = if ($registration) { Join-Path $registration.InstallLocation 'MttVDD.inf' }
if (!$nefcon -or !(Test-Path $nefcon) -or !(Test-Path $inf)) {
    throw 'The Virtual Display Driver files are missing. Uninstall Virtual Display Driver in Windows Settings > Apps, then run EternalMonitor Setup again.'
}

# nefconw is a window-less GUI program: wait for it explicitly.
function Invoke-Nefcon([string]$Arguments) {
    $process = Start-Process -FilePath $nefcon -ArgumentList $Arguments -Wait -PassThru -WindowStyle Hidden
    # 3010: done, takes full effect after a restart.
    if ($process.ExitCode -notin @(0, 3010)) {
        throw "Virtual Display Driver repair failed: nefconw $($Arguments.Split(' ')[0]) exited $($process.ExitCode)."
    }
}

# --remove-device-node also removes the driver; --install-driver adds it back.
if ($devices.Count -gt 0) {
    Invoke-Nefcon "--remove-device-node --hardware-id $hardwareId --class-guid $classGuid"
}
Invoke-Nefcon "--create-device-node --hardware-id $hardwareId --class-name Display --class-guid $classGuid"
Invoke-Nefcon "--install-driver --inf-path `"$inf`""

$repaired = @(Get-VddDevices)
if ($repaired.Count -eq 0 -or @($repaired | Where-Object { !(Test-Healthy $_) }).Count -gt 0) {
    throw 'Windows did not accept the repaired Virtual Display Driver. Restart the PC, then run EternalMonitor Setup again.'
}
Write-Output "Repaired the Virtual Display Driver device: $($repaired[0].InstanceId)"
