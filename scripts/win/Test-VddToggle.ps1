# Pure script checks. Every PnP command is replaced in this process.
param([string]$TogglePath)
$ErrorActionPreference = 'Stop'
$toggle = if ($TogglePath) { $TogglePath } else { Join-Path $PSScriptRoot '..\..\installer\scripts\vdd-toggle.ps1' }
function Reset-State {
    $global:EMVddTestState = @{ Calls=@(); Fail=$false; Ignore=$false
        # What the device reports once Enable-PnpDevice has run.
        Enabled=@{Problem=0; Status='OK'; Monitor='OK'}
        Devices=@(
            [pscustomobject]@{InstanceId='PCI\REAL_GPU';HardwareIds=@('PCI\GPU');Problem=0;Status='OK';Inf='oem1.inf';Children=@()},
            [pscustomobject]@{InstanceId='ROOT\DISPLAY\0001';HardwareIds=@('Root\OtherDriver');Problem=0;Status='OK';Inf='oem2.inf';Children=@()},
            [pscustomobject]@{InstanceId='ROOT\DISPLAY\0007';HardwareIds=@('Root\MttVDD');Problem=22;Status='Error';Inf='oem33.inf';Children=@()},
            [pscustomobject]@{InstanceId='DISPLAY\MTT1337\1&2&3&UID256';HardwareIds=@('MONITOR\MTT1337');Problem=24;Status='Unknown';Inf='monitor.inf';Children=@()}
        ) }
}
function Find-Device($InstanceId) { $global:EMVddTestState.Devices | Where-Object InstanceId -eq $InstanceId }
# Like the real cmdlets, each takes several instance IDs or keys at once.
function Get-PnpDevice {
    param($Class, [switch]$PresentOnly, [string[]]$InstanceId)
    $global:EMVddTestState.Calls += 'get'
    $global:EMVddTestState.Devices | Where-Object {
        ($InstanceId -and $InstanceId -contains $_.InstanceId) -or (!$InstanceId -and $_.InstanceId -notlike 'DISPLAY\*')
    }
}
function Get-PnpDeviceProperty {
    param($InstanceId, [string[]]$KeyName)
    $global:EMVddTestState.Calls += 'property'
    $device = Find-Device $InstanceId
    foreach ($key in $KeyName) {
        $data = switch ($key) {
            'DEVPKEY_Device_HardwareIds' { $device.HardwareIds }
            'DEVPKEY_Device_DriverInfPath' { $device.Inf }
            'DEVPKEY_Device_Children' { $device.Children }
            default { throw "Unexpected property $key" }
        }
        [pscustomobject]@{KeyName=$key; Data=$data}
    }
}
function Enable-PnpDevice {
    param($InstanceId, $Confirm)
    if ($global:EMVddTestState.Fail) { throw 'mock driver failure' }
    $global:EMVddTestState.Calls += "enable:$InstanceId"
    if ($global:EMVddTestState.Ignore) { return }
    $device = Find-Device $InstanceId
    $enabled = $global:EMVddTestState.Enabled
    $device.Problem = $enabled.Problem
    $device.Status = $enabled.Status
    $monitor = Find-Device 'DISPLAY\MTT1337\1&2&3&UID256'
    if ($enabled.Monitor) {
        $device.Children = @($monitor.InstanceId)
        $monitor.Status = $enabled.Monitor
    }
}
function Disable-PnpDevice {
    param($InstanceId, $Confirm)
    $global:EMVddTestState.Calls += "disable:$InstanceId"
    $device = Find-Device $InstanceId
    $device.Problem = 22
    $device.Status = 'Error'
    $device.Children = @()
}
function Expect-Failure([scriptblock]$Run, [string]$Message) {
    $caught = $null
    try { & $Run } catch { $caught = $_.Exception.Message }
    if (!$caught -or !$caught.Contains($Message)) { throw "Expected failure containing '$Message', got '$caught'" }
}
# Runs one enable with a short start wait, and checks its exit code.
function Expect-Enable([int]$Code, [string]$Case) {
    $global:LASTEXITCODE = 0
    & $toggle -Action enable -StartSeconds 0 2>$null | Out-Null
    if ($LASTEXITCODE -ne $Code) { throw "$Case`: expected exit $Code, got $LASTEXITCODE" }
}
try {
    Reset-State
    # The Disable task leaves the device at code 22; Enable turns it back on.
    $global:LASTEXITCODE = 0
    & $toggle -Action enable | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "A disabled device was not re-enabled: exit $LASTEXITCODE" }
    & $toggle -Action disable | Out-Null
    if ((($global:EMVddTestState.Calls | Where-Object { $_ -like '*:*' }) -join ',') -ne 'enable:ROOT\DISPLAY\0007,disable:ROOT\DISPLAY\0007') {
        throw 'The toggle selected another GPU or a different virtual driver'
    }
    $vdd = Get-PnpDevice -InstanceId 'ROOT\DISPLAY\0007'
    if ($vdd.Problem -ne 22) { throw 'Disable did not leave code 22' }

    # Each PnP call costs about half a second while the device starts: a ready device is checked
    # with two after Enable-PnpDevice.
    Reset-State
    & $toggle -Action enable | Out-Null
    $calls = $global:EMVddTestState.Calls
    $after = $calls[([array]::IndexOf($calls, 'enable:ROOT\DISPLAY\0007') + 1)..($calls.Count - 1)]
    if (($after -join ',') -ne 'property,get') { throw "A ready device took these calls to check: $($after -join ',')" }
    & $toggle -Action disable | Out-Null

    $global:EMVddTestState.Fail = $true
    Expect-Failure { & $toggle -Action enable } 'mock driver failure'
    $global:EMVddTestState.Fail = $false

    # Enable-PnpDevice returned, but Windows kept the device off.
    $global:EMVddTestState.Ignore = $true
    Expect-Enable 122 'still disabled'
    $global:EMVddTestState.Ignore = $false

    # Problem codes Windows reports once the device is on: driver error, start
    # failure, unverified signature.
    foreach ($code in 43, 31, 10, 52, 28) {
        Reset-State
        $global:EMVddTestState.Enabled = @{Problem=$code; Status='Error'; Monitor=$null}
        Expect-Enable (100 + $code) "problem $code"
    }

    Reset-State
    $global:EMVddTestState.Enabled = @{Problem=0; Status='Unknown'; Monitor=$null}
    Expect-Enable 4 'not started'

    Reset-State
    $global:EMVddTestState.Enabled = @{Problem=0; Status='OK'; Monitor=$null}
    Expect-Enable 5 'no monitor'
    $global:EMVddTestState.Enabled = @{Problem=0; Status='OK'; Monitor='Error'}
    Expect-Enable 5 'monitor with a problem'

    Reset-State
    (Get-PnpDevice -InstanceId 'ROOT\DISPLAY\0007').Inf = $null
    Expect-Enable 3 'no driver bound'

    # A monitor that arrives a moment after the device starts is waited for.
    Reset-State
    $global:EMVddTestState.Enabled = @{Problem=0; Status='OK'; Monitor='Unknown'}
    function Start-Sleep { param($Milliseconds) (Get-PnpDevice -InstanceId 'DISPLAY\MTT1337\1&2&3&UID256').Status = 'OK' }
    $global:LASTEXITCODE = 0
    & $toggle -Action enable -StartSeconds 5 | Out-Null
    Remove-Item function:Start-Sleep
    if ($LASTEXITCODE -ne 0) { throw "A monitor that arrived during the wait failed: exit $LASTEXITCODE" }

    Reset-State
    $global:EMVddTestState.Devices = @($global:EMVddTestState.Devices[0])
    Expect-Enable 2 'absent device'
    $global:LASTEXITCODE = 0
    & $toggle -Action disable | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Disabling an absent device failed' }
    Write-Output 'VDD toggle checks passed: selection, re-enable after code 22, two calls per check, still disabled, problem codes, not started, no monitor, late monitor, unbound driver, absent device'
    # The failure cases leave their exit codes behind; CI ends with this one.
    $global:LASTEXITCODE = 0
} finally {
    Remove-Variable EMVddTestState -Scope Global -ErrorAction SilentlyContinue
}
