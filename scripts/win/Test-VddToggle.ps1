# Pure script checks. Every PnP command is replaced in this process.
param([string]$TogglePath)
$ErrorActionPreference = 'Stop'
$toggle = if ($TogglePath) { $TogglePath } else { Join-Path $PSScriptRoot '..\..\installer\scripts\vdd-toggle.ps1' }
$global:EMVddTestState = @{ Calls=@(); Fail=$false; Ignore=$false; Devices=@(
    [pscustomobject]@{InstanceId='PCI\REAL_GPU';HardwareIds=@('PCI\GPU');Problem=0},
    [pscustomobject]@{InstanceId='ROOT\DISPLAY\0001';HardwareIds=@('Root\OtherDriver');Problem=0},
    [pscustomobject]@{InstanceId='ROOT\DISPLAY\0007';HardwareIds=@('Root\MttVDD');Problem=22}
) }
function Get-PnpDevice {
    param($Class, [switch]$PresentOnly, $InstanceId)
    $global:EMVddTestState.Devices | Where-Object { !$InstanceId -or $_.InstanceId -eq $InstanceId }
}
function Get-PnpDeviceProperty {
    param($InstanceId, $KeyName)
    [pscustomobject]@{Data=(Get-PnpDevice -InstanceId $InstanceId).HardwareIds}
}
function Enable-PnpDevice {
    param($InstanceId, $Confirm)
    if ($global:EMVddTestState.Fail) { throw 'mock driver failure' }
    $global:EMVddTestState.Calls += "enable:$InstanceId"
    if (!$global:EMVddTestState.Ignore) { (Get-PnpDevice -InstanceId $InstanceId).Problem = 0 }
}
function Disable-PnpDevice {
    param($InstanceId, $Confirm)
    $global:EMVddTestState.Calls += "disable:$InstanceId"
    (Get-PnpDevice -InstanceId $InstanceId).Problem = 22
}
function Expect-Failure([scriptblock]$Run, [string]$Message) {
    $caught = $null
    try { & $Run } catch { $caught = $_.Exception.Message }
    if (!$caught -or !$caught.Contains($Message)) { throw "Expected failure containing '$Message', got '$caught'" }
}
try {
    & $toggle -Action enable
    & $toggle -Action disable
    if (($global:EMVddTestState.Calls -join ',') -ne 'enable:ROOT\DISPLAY\0007,disable:ROOT\DISPLAY\0007') {
        throw 'The toggle selected another GPU or a different virtual driver'
    }
    $global:EMVddTestState.Fail = $true
    Expect-Failure { & $toggle -Action enable } 'mock driver failure'
    $global:EMVddTestState.Fail = $false
    $global:EMVddTestState.Ignore = $true
    Expect-Failure { & $toggle -Action enable } 'reports 22'
    $global:EMVddTestState.Devices = @()
    Expect-Failure { & $toggle -Action enable } 'not present'
    & $toggle -Action disable
    Write-Output 'VDD toggle checks passed: selection, failure, state verification, absent device'
} finally {
    Remove-Variable EMVddTestState -Scope Global
}
