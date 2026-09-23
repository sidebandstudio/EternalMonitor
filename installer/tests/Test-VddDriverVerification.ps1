$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot '..\scripts\vdd-driver-verify.ps1'
$global:EMVddFixture = @()
function Get-PnpDevice {
    [CmdletBinding()] param([string]$Class,[switch]$PresentOnly)
    if ($Class -ne 'Display' -or !$PresentOnly) { throw 'Must query present display devices' }
    $global:EMVddFixture
}
function Get-PnpDeviceProperty {
    [CmdletBinding()] param([string]$InstanceId,[string]$KeyName)
    $device = $global:EMVddFixture | Where-Object { $_.InstanceId -eq $InstanceId }
    $value = switch ($KeyName) {
        'DEVPKEY_Device_HardwareIds' { $device.HardwareIds }
        'DEVPKEY_Device_DriverInfPath' { $device.Inf }
        'DEVPKEY_Device_DriverVersion' { $device.Version }
        default { throw "Unexpected property $KeyName" }
    }
    [pscustomobject]@{Data=$value}
}
function Expect-Failure([string]$Message) {
    $failure = $null
    try { & $script } catch { $failure = $_.Exception.Message }
    if (!$failure -or $failure -notlike "*$Message*") { throw "Expected '$Message', got '$failure'" }
}
$gpu = [pscustomobject]@{InstanceId='PCI\GPU';HardwareIds=@('PCI\GPU');Inf='oem1.inf';Version='1.0';Problem=0}
$vdd = [pscustomobject]@{InstanceId='ROOT\DISPLAY\0002';HardwareIds=@('Root\MttVDD');Inf=$null;Version=$null;Problem=22}
$global:EMVddFixture = @($gpu)
Expect-Failure 'device is absent'
$global:EMVddFixture = @($gpu,$vdd)
# Setup and task registration can succeed with a disabled, unbound devnode.
Expect-Failure 'not bound'
$vdd.Inf = 'oem33.inf'
Expect-Failure 'not bound'
$vdd.Version = '23.40.36.27'
& $script
$vdd.Problem = 0
& $script
$vdd.Problem = 52
Expect-Failure 'device problem 52'
Write-Output 'PASS: absent, unbound disabled, missing version, bound disabled, active and rejected driver cases'
