$ErrorActionPreference = 'Stop'
$global:EMVddStateDevices = @(
    [pscustomobject]@{InstanceId='PCI\GPU';Status='OK';Problem=0;Ids=@('PCI\GPU')},
    [pscustomobject]@{InstanceId='ROOT\DISPLAY\0002';Status='Error';Problem=22;Ids=@('Root\MttVDD')}
)
$global:EMVddStateInf = 'oem33.inf'
$global:EMVddStateVersion = '23.40.36.27'
function Get-PnpDevice {
    [CmdletBinding()] param([string]$Class,[switch]$PresentOnly,[string]$InstanceId)
    if ($InstanceId) { $global:EMVddStateDevices | Where-Object {$_.InstanceId -eq $InstanceId} }
    elseif ($Class -eq 'Display' -and $PresentOnly) { $global:EMVddStateDevices }
    else { throw 'Unexpected PnP query' }
}
function Get-PnpDeviceProperty {
    [CmdletBinding()] param([string]$InstanceId,[string]$KeyName)
    $data = switch ($KeyName) {
        'DEVPKEY_Device_HardwareIds' { ($global:EMVddStateDevices | Where-Object {$_.InstanceId -eq $InstanceId}).Ids }
        # Rebooted disabled devices have no live problem-code property.
        'DEVPKEY_Device_ProblemCode' { $null }
        'DEVPKEY_Device_DriverInfPath' { $global:EMVddStateInf }
        'DEVPKEY_Device_DriverVersion' { $global:EMVddStateVersion }
        default { throw 'Unexpected PnP property' }
    }
    [pscustomobject]@{Data=$data}
}
function Test-Path { param([string]$Path) $false }
$script = Join-Path $PSScriptRoot 'Vdd-State.ps1'
$state = & $script | ConvertFrom-Json
if ($state.instance -ne 'ROOT\DISPLAY\0002' -or !$state.disabled -or $state.driver_inf -ne 'oem33.inf') {
    throw 'State did not identify the bound, disabled driver at its actual instance ID'
}
foreach ($property in @('EMVddStateInf','EMVddStateVersion')) {
    $original = Get-Variable -Name $property -Scope Global -ValueOnly
    Set-Variable -Name $property -Value $null -Scope Global
    $failed = $false
    try { & $script | Out-Null } catch { $failed = $_.Exception.Message -match 'no bound driver' }
    Set-Variable -Name $property -Value $original -Scope Global
    if (!$failed) { throw "Unbound driver passed with missing $property" }
}
Write-Output 'PASS: actual hardware ID selected; disabled unbound devices rejected'
