# Pure script checks for the Setup step that repairs a preserved Virtual Display Driver.
# PnP, the registry and the vendor's nefconw are replaced in this process; nothing is installed.
$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot '..\scripts\vdd-driver-repair.ps1'
$iss = Get-Content -Raw (Join-Path $PSScriptRoot '..\EternalMonitor.iss')
$root = Join-Path $env:TEMP ('em-driver-repair-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $root | Out-Null
$nefcon = Join-Path $root 'nefconw.exe'
$inf = Join-Path $root 'MttVDD.inf'
[IO.File]::WriteAllText($nefcon, 'test fixture; never executed')
[IO.File]::WriteAllText($inf, 'test fixture')

function Reset-Fixture {
    $global:EMRepair = @{
        Registration = [pscustomobject]@{InstallLocation=$root; DisplayVersion='25.05.03'}
        Calls = @(); Exit = 0; Accept = $true
        Devices = @([pscustomobject]@{InstanceId='PCI\GPU';HardwareIds=@('PCI\GPU');Inf='oem1.inf';Problem=0})
    }
}
function Add-Vdd([string]$Inf, [int]$Problem) {
    $global:EMRepair.Devices += [pscustomobject]@{InstanceId='ROOT\DISPLAY\0000';HardwareIds=@('Root\MttVDD');Inf=$Inf;Problem=$Problem}
}
function Get-PnpDevice {
    [CmdletBinding()] param([string]$Class,[switch]$PresentOnly)
    if ($Class -ne 'Display' -or !$PresentOnly) { throw 'Must query present display devices' }
    $global:EMRepair.Devices
}
function Get-PnpDeviceProperty {
    [CmdletBinding()] param([string]$InstanceId,[string]$KeyName)
    $device = $global:EMRepair.Devices | Where-Object { $_.InstanceId -eq $InstanceId }
    $value = switch ($KeyName) {
        'DEVPKEY_Device_HardwareIds' { $device.HardwareIds }
        'DEVPKEY_Device_DriverInfPath' { $device.Inf }
        default { throw "Unexpected property $KeyName" }
    }
    [pscustomobject]@{Data=$value}
}
function Get-ItemProperty { [CmdletBinding()] param([string[]]$Path) $global:EMRepair.Registration }
# Plays the vendor tool: removes, creates and binds the device as nefconw would.
function Start-Process {
    [CmdletBinding()] param([string]$FilePath,[string]$ArgumentList,[switch]$Wait,[switch]$PassThru,[string]$WindowStyle)
    if ($FilePath -ne $nefcon -or !$Wait -or !$PassThru) { throw "Unexpected process $FilePath" }
    $verb = $ArgumentList.Split(' ')[0]
    $global:EMRepair.Calls += $verb
    if ($global:EMRepair.Exit -notin @(0, 3010)) { return [pscustomobject]@{ExitCode=$global:EMRepair.Exit} }
    switch ($verb) {
        '--remove-device-node' {
            if ($ArgumentList -notmatch 'Root\\MttVDD' -or $ArgumentList -notmatch '4D36E968-E325-11CE-BFC1-08002BE10318') { throw "Bad remove: $ArgumentList" }
            $global:EMRepair.Devices = @($global:EMRepair.Devices | Where-Object { $_.HardwareIds -notcontains 'Root\MttVDD' })
        }
        '--create-device-node' {
            if ($ArgumentList -notmatch '--hardware-id Root\\MttVDD --class-name Display --class-guid 4D36E968') { throw "Bad create: $ArgumentList" }
            Add-Vdd $null 28
        }
        '--install-driver' {
            if ($ArgumentList -notmatch [regex]::Escape("--inf-path `"$inf`"")) { throw "Bad install: $ArgumentList" }
            if ($global:EMRepair.Accept) {
                $vdd = $global:EMRepair.Devices | Where-Object { $_.HardwareIds -contains 'Root\MttVDD' }
                $vdd.Inf = 'oem40.inf'; $vdd.Problem = 0
            }
        }
        default { throw "Unexpected nefconw verb $verb" }
    }
    [pscustomobject]@{ExitCode=$global:EMRepair.Exit}
}
function Expect-Calls([string]$Expected, [string]$Case) {
    $actual = $global:EMRepair.Calls -join ','
    if ($actual -ne $Expected) { throw "$Case`: expected nefconw [$Expected], ran [$actual]" }
}
function Expect-Failure([string]$Message, [string]$Case) {
    $failure = $null
    try { & $script | Out-Null } catch { $failure = $_.Exception.Message }
    if (!$failure -or $failure -notlike "*$Message*") { throw "$Case`: expected '$Message', got '$failure'" }
}
$repair = '--remove-device-node,--create-device-node,--install-driver'
try {
    # Between streams the device is off with code 22; running is fine too.
    foreach ($problem in 22, 0) {
        Reset-Fixture; Add-Vdd 'oem33.inf' $problem
        & $script | Out-Null
        Expect-Calls '' "healthy device, code $problem"
    }

    # Removed in Device Manager: created again and bound to the driver.
    Reset-Fixture
    & $script | Out-Null
    Expect-Calls '--create-device-node,--install-driver' 'missing device'

    # A driver error, a signature block, or no driver: replaced as the vendor install does.
    foreach ($case in @(@{Inf='oem33.inf';Problem=43}, @{Inf='oem33.inf';Problem=52}, @{Inf=$null;Problem=28}, @{Inf=$null;Problem=0})) {
        Reset-Fixture; Add-Vdd $case.Inf $case.Problem
        & $script | Out-Null
        Expect-Calls $repair "device with inf '$($case.Inf)' and code $($case.Problem)"
    }

    # A restart-required result still counts, once the device is bound.
    Reset-Fixture; $global:EMRepair.Exit = 3010
    & $script | Out-Null
    Expect-Calls '--create-device-node,--install-driver' 'restart required'

    Reset-Fixture; $global:EMRepair.Exit = 5
    Expect-Failure 'nefconw --create-device-node exited 5' 'vendor tool failure'

    Reset-Fixture; $global:EMRepair.Accept = $false
    Expect-Failure 'did not accept' 'driver not bound after repair'

    # Without the vendor's files there is nothing to repair from: say how to reinstall.
    Reset-Fixture; $global:EMRepair.Registration = $null
    Expect-Failure 'Uninstall Virtual Display Driver' 'no registration'
    Expect-Calls '' 'no registration'
    Reset-Fixture; Remove-Item $nefcon
    Expect-Failure 'Uninstall Virtual Display Driver' 'missing vendor tool'
    [IO.File]::WriteAllText($nefcon, 'test fixture; never executed')

    # Setup ships the step and runs it before the tasks disable the device, also when the
    # existing driver is preserved.
    if ($iss -notmatch 'Source: "scripts\\vdd-driver-repair\.ps1"') { throw 'Setup does not install the repair script' }
    $tasks = ($iss -split "`n") | Where-Object { $_ -match 'vdd-tasks-setup\.ps1' -and $_ -match '^Filename:' }
    if ($tasks -notmatch 'BeforeInstall: RepairVddDriver' -or $tasks -match 'Check: NeedsVddInstall') {
        throw 'Setup does not repair a preserved driver before registering the tasks'
    }
    if ($iss -notmatch "(?s)procedure RepairVddDriver;.*vdd-driver-repair\.ps1.*ExitCode <> 0.*RaiseException") {
        throw 'Setup ignores a failed repair'
    }
    Write-Output 'PASS: healthy, missing, driver error, signature, unbound, restart, tool failure, rejected, no vendor files, Setup wiring'
} finally {
    Remove-Item $root -Recurse -Force
    Remove-Variable EMRepair -Scope Global -ErrorAction SilentlyContinue
}
