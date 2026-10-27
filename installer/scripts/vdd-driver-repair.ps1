# Puts back the Virtual Display Driver's device when EternalMonitor installed the driver and no
# working device is left. Setup keeps an existing driver instead of running the vendor setup
# again, so a device removed in Device Manager, or left without its driver or with a driver
# error, stayed broken through every reinstall. Run elevated by the installer, before the
# scheduled tasks disable the device again.
#
# It changes nothing while any device works, and nothing for a driver another app installed.
# The driver package is staged first, and a replacement device must work before the broken
# ones are removed, so a failure leaves the devices as they were. A rerun after success does
# nothing.
#
# Exit codes, which Setup turns into its messages (installer/EternalMonitor.iss):
#   0 working or repaired   1 repair failed   10 another app installed the driver   11 files missing
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

function Get-Working { @(Get-VddDevices | Where-Object { Test-Healthy $_ }) }

function Invoke-PnpUtil([string[]]$Arguments) {
    pnputil.exe @Arguments | Out-Null
    # 3010: done, takes effect after a restart. 259: staged, no device needed updating.
    if ($LASTEXITCODE -notin @(0, 3010, 259)) {
        throw "pnputil $($Arguments[0]) exited $LASTEXITCODE."
    }
}

# pnputil can report success, 259 or a pending restart and leave the device in
# place, so a removal counts only once the device is gone.
function Remove-Device([string]$Id) {
    Invoke-PnpUtil @('/remove-device', $Id)
    if (@(Get-VddDevices | Where-Object { $_.InstanceId -eq $Id }).Count -gt 0) {
        throw "Windows did not remove the Virtual Display Driver device $Id."
    }
}

try {
    if (@(Get-Working).Count -gt 0) {
        Write-Output 'Virtual Display Driver device is installed.'
        exit 0
    }

    # The ownership rule of vdd-driver-remove.ps1: Setup recorded this exact vendor install
    # when it ran the vendor setup itself.
    $marker = Join-Path $PSScriptRoot '..\driver\installed-by-eternalmonitor.txt'
    $registration = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1' -ErrorAction SilentlyContinue |
        Where-Object { $_.InstallLocation -and $_.DisplayVersion } | Select-Object -First 1
    $owned = $registration -and (Test-Path $marker) -and
        ([IO.File]::ReadAllText($marker) -eq ((Join-Path $registration.InstallLocation 'unins000.exe') + "`r`n" + $registration.DisplayVersion))
    if (!$owned) {
        [Console]::Error.WriteLine('Another app installed the Virtual Display Driver. EternalMonitor leaves it alone.')
        exit 10
    }
    $inf = Join-Path $registration.InstallLocation 'MttVDD.inf'
    $nefcon = Join-Path $registration.InstallLocation 'nefconw.exe'
    if (!(Test-Path $inf) -or !(Test-Path $nefcon)) {
        [Console]::Error.WriteLine("The Virtual Display Driver files are missing from $($registration.InstallLocation).")
        exit 11
    }

    # Stage the package and bind it to matching devices. This alone repairs a device whose
    # driver package was deleted, and nothing is removed if it fails.
    Invoke-PnpUtil @('/add-driver', $inf, '/install')
    if (@(Get-Working).Count -gt 0) {
        Write-Output 'Repaired the Virtual Display Driver by reinstalling its driver package.'
        exit 0
    }

    # Still nothing works: add a device as the vendor install does, and bind it.
    $broken = @(Get-VddDevices | ForEach-Object { $_.InstanceId })
    $process = Start-Process -FilePath $nefcon -WindowStyle Hidden -Wait -PassThru `
        -ArgumentList "--create-device-node --hardware-id $hardwareId --class-name Display --class-guid $classGuid"
    $added = @(Get-VddDevices | Where-Object { $broken -notcontains $_.InstanceId })
    if ($process.ExitCode -ne 0 -or $added.Count -ne 1) {
        foreach ($device in $added) { Remove-Device $device.InstanceId }
        throw "Could not add a Virtual Display Driver device: nefconw exited $($process.ExitCode)."
    }
    $new = $added[0].InstanceId
    # A failure here is judged by the device's state below, which undoes the new device.
    try { Invoke-PnpUtil @('/add-driver', $inf, '/install') } catch {}
    if (@(Get-Working | Where-Object { $_.InstanceId -eq $new }).Count -eq 0) {
        Remove-Device $new
        throw 'Windows did not start the new Virtual Display Driver device.'
    }
    # The new device works: only now remove the broken ones, by instance.
    foreach ($id in $broken) { Remove-Device $id }
    Write-Output "Repaired the Virtual Display Driver device: $new"
    exit 0
} catch {
    [Console]::Error.WriteLine("Virtual Display Driver repair failed: $($_.Exception.Message)")
    exit 1
}
