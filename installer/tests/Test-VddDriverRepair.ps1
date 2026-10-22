# Pure script checks for the Setup step that repairs a Virtual Display Driver EternalMonitor
# installed. PnP, the registry, pnputil and the vendor's nefconw are replaced in this process,
# and the script runs from a scratch copy; nothing is installed or removed.
param([string]$RepairPath)
$ErrorActionPreference = 'Stop'
$source = if ($RepairPath) { $RepairPath } else { Join-Path $PSScriptRoot '..\scripts\vdd-driver-repair.ps1' }
$iss = Get-Content -Raw (Join-Path $PSScriptRoot '..\EternalMonitor.iss')
$root = Join-Path $env:TEMP ('em-driver-repair-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force "$root\scripts", "$root\driver", "$root\vendor" | Out-Null
$script = "$root\scripts\vdd-driver-repair.ps1"
Copy-Item $source $script
$vendor = "$root\vendor"
$nefcon = "$vendor\nefconw.exe"
$inf = "$vendor\MttVDD.inf"
$marker = "$root\driver\installed-by-eternalmonitor.txt"

function Reset-Fixture {
    foreach ($file in $nefcon, $inf) { [IO.File]::WriteAllText($file, 'test fixture; never executed') }
    [IO.File]::WriteAllText($marker, "$vendor\unins000.exe`r`n25.05.03")
    $global:EMRepair = @{
        Registration = [pscustomobject]@{InstallLocation=$vendor; DisplayVersion='25.05.03'}
        Calls = @(); Next = 1
        # The driver package in the Windows driver store, which binds to new devices.
        InStore = $true; StageExit = 0
        # Whether a device the package binds to then starts.
        NewStarts = $true
        # pnputil reports removing a device that stays (259 or a pending restart).
        RemoveSticks = $false
        Devices = @([pscustomobject]@{InstanceId='PCI\GPU';HardwareIds=@('PCI\GPU');Inf='oem1.inf';Problem=0})
    }
}
function Add-Vdd([string]$Inf, [int]$Problem) {
    $id = 'ROOT\DISPLAY\{0:D4}' -f ($global:EMRepair.Next++)
    $global:EMRepair.Devices += [pscustomobject]@{InstanceId=$id;HardwareIds=@('Root\MttVDD');Inf=$(if ($Inf) { $Inf } else { $null });Problem=$Problem}
    $id
}
function Get-Vdd { @($global:EMRepair.Devices | Where-Object { $_.HardwareIds -contains 'Root\MttVDD' }) }
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
# Binds the staged package to every driverless device, as Windows does.
function Bind-Driverless {
    foreach ($device in Get-Vdd | Where-Object { !$_.Inf }) {
        $device.Inf = 'oem40.inf'
        $device.Problem = if ($global:EMRepair.NewStarts) { 0 } else { 43 }
    }
}
function global:pnputil.exe {
    $verb = $args[0]
    $global:EMRepair.Calls += "pnputil $verb $(if ($verb -eq '/remove-device') { $args[1] })".Trim()
    switch ($verb) {
        '/add-driver' {
            if ($args[1] -ne $inf -or $args[2] -ne '/install') { throw "Bad add-driver: $args" }
            $global:LASTEXITCODE = $global:EMRepair.StageExit
            if ($global:EMRepair.StageExit -in @(0, 3010)) { $global:EMRepair.InStore = $true; Bind-Driverless }
        }
        '/remove-device' {
            $id = $args[1]
            if ($global:EMRepair.RemoveSticks) { $global:LASTEXITCODE = 259; return }
            $global:EMRepair.Devices = @($global:EMRepair.Devices | Where-Object { $_.InstanceId -ne $id })
            $global:LASTEXITCODE = 0
        }
        default { throw "Unexpected pnputil $verb" }
    }
}
# nefconw 1.10.0. --remove-device-node looks up a compatible driver before it removes
# anything, so it fails while a device has no driver and the store has no package.
function Start-Process {
    [CmdletBinding()] param([string]$FilePath,[string]$ArgumentList,[switch]$Wait,[switch]$PassThru,[string]$WindowStyle)
    if ($FilePath -ne $nefcon -or !$Wait -or !$PassThru) { throw "Unexpected process $FilePath" }
    $verb = $ArgumentList.Split(' ')[0]
    $global:EMRepair.Calls += "nefconw $verb"
    $exit = 0
    switch ($verb) {
        '--create-device-node' {
            if ($ArgumentList -notmatch '--hardware-id Root\\MttVDD --class-name Display --class-guid 4D36E968') { throw "Bad create: $ArgumentList" }
            Add-Vdd $null 28 | Out-Null
        }
        '--remove-device-node' {
            if (!$global:EMRepair.InStore -and (Get-Vdd | Where-Object { !$_.Inf })) { $exit = 1 }
            else { $global:EMRepair.Devices = @($global:EMRepair.Devices | Where-Object { $_.HardwareIds -notcontains 'Root\MttVDD' }) }
        }
        '--install-driver' { $global:EMRepair.InStore = $true; Bind-Driverless }
        default { throw "Unexpected nefconw verb $verb" }
    }
    [pscustomobject]@{ExitCode=$exit}
}
function Invoke-Repair {
    $global:LASTEXITCODE = 0
    try { & $script 2>$null | Out-Null } catch { $global:LASTEXITCODE = "threw: $($_.Exception.Message)" }
    $LASTEXITCODE
}
$failures = @()
function Expect([string]$Case, $Code, [string]$Calls, [scriptblock]$State = { $true }) {
    $actual = Invoke-Repair
    $ran = $global:EMRepair.Calls -join ', '
    if ("$actual" -ne "$Code" -or $ran -ne $Calls -or !(& $State)) {
        $script:failures += "$Case`: exit $actual (want $Code), ran [$ran] (want [$Calls]), devices $((Get-Vdd | ForEach-Object { "$($_.InstanceId):$($_.Inf):$($_.Problem)" }) -join ' ')"
    }
}
function Working { @(Get-Vdd | Where-Object { $_.Inf -and $_.Problem -in @(0, 22) }) }
$stage = 'pnputil /add-driver'
try {
    # Between streams the device is off with code 22; running is fine too.
    foreach ($problem in 22, 0) {
        Reset-Fixture; Add-Vdd 'oem33.inf' $problem | Out-Null
        Expect "working device, code $problem" 0 ''
    }
    # A working device next to a broken one: nothing is touched.
    Reset-Fixture; $good = Add-Vdd 'oem33.inf' 22; Add-Vdd 'oem33.inf' 43 | Out-Null
    Expect 'working and broken devices' 0 '' { @(Get-Vdd).Count -eq 2 }

    # A driver another app installed is never changed.
    Reset-Fixture; Remove-Item $marker; Add-Vdd 'oem33.inf' 43 | Out-Null
    Expect 'no ownership marker' 10 '' { @(Get-Vdd).Count -eq 1 }
    Reset-Fixture; [IO.File]::WriteAllText($marker, "$vendor\unins000.exe`r`n24.12.24")
    Expect 'vendor install changed since Setup recorded it' 10 ''
    Reset-Fixture; $global:EMRepair.Registration = $null
    Expect 'no vendor registration' 10 ''
    Reset-Fixture; Remove-Item $nefcon
    Expect 'vendor files missing' 11 ''

    # Driverless device and the package gone from the driver store: staging the package
    # binds it, and nothing is removed. nefconw's remove fails in this state.
    Reset-Fixture; $global:EMRepair.InStore = $false; $kept = Add-Vdd $null 28
    Expect 'driverless device, package missing' 0 $stage { (Working).InstanceId -eq $kept }
    # Removed in Device Manager, with or without its package.
    foreach ($inStore in $true, $false) {
        Reset-Fixture; $global:EMRepair.InStore = $inStore
        Expect "missing device, package in store=$inStore" 0 "$stage, nefconw --create-device-node, $stage" { @(Working).Count -eq 1 }
    }
    # A device with a driver error is replaced, and removed only once the new one works.
    Reset-Fixture; $old = Add-Vdd 'oem33.inf' 43
    Expect 'driver error' 0 "$stage, nefconw --create-device-node, $stage, pnputil /remove-device $old" {
        @(Get-Vdd).Count -eq 1 -and (Working).InstanceId -ne $old }
    # The replacement fails too: only the device this run added is removed.
    Reset-Fixture; $old = Add-Vdd 'oem33.inf' 43; $global:EMRepair.NewStarts = $false
    Expect 'replacement fails' 1 "$stage, nefconw --create-device-node, $stage, pnputil /remove-device ROOT\DISPLAY\0002" {
        @(Get-Vdd).Count -eq 1 -and @(Get-Vdd)[0].InstanceId -eq $old -and @(Get-Vdd)[0].Problem -eq 43 }
    # pnputil reports removing a broken device that stays: not a success.
    Reset-Fixture; $old = Add-Vdd 'oem33.inf' 43; $global:EMRepair.RemoveSticks = $true
    Expect 'broken device not removed' 1 "$stage, nefconw --create-device-node, $stage, pnputil /remove-device $old" {
        @(Get-Vdd).Count -eq 2 -and @(Working).Count -eq 1 }
    # The same when undoing a replacement that did not start.
    Reset-Fixture; $old = Add-Vdd 'oem33.inf' 43; $global:EMRepair.NewStarts = $false; $global:EMRepair.RemoveSticks = $true
    Expect 'failed replacement not removed' 1 "$stage, nefconw --create-device-node, $stage, pnputil /remove-device ROOT\DISPLAY\0002"
    # Windows rejects the package: nothing else happens.
    Reset-Fixture; $old = Add-Vdd 'oem33.inf' 52; $global:EMRepair.StageExit = 5
    Expect 'package rejected' 1 $stage { @(Get-Vdd).Count -eq 1 -and @(Get-Vdd)[0].Problem -eq 52 }
    # A restart-required result still counts once the device works.
    Reset-Fixture; $global:EMRepair.StageExit = 3010
    Expect 'restart required' 0 "$stage, nefconw --create-device-node, $stage"
    # A rerun after a repair changes nothing.
    $global:EMRepair.Calls = @(); $global:EMRepair.StageExit = 0
    Expect 'rerun' 0 ''

    # Setup ships the step, runs it before the tasks disable the device, also when the
    # existing driver is preserved, and names each outcome.
    if ($iss -notmatch 'Source: "scripts\\vdd-driver-repair\.ps1"') { $failures += 'Setup does not install the repair script' }
    $tasks = ($iss -split "`n") | Where-Object { $_ -match 'vdd-tasks-setup\.ps1' -and $_ -match '^Filename:' }
    if ($tasks -notmatch 'BeforeInstall: RepairVddDriver' -or $tasks -match 'Check: NeedsVddInstall') {
        $failures += 'Setup does not repair a preserved driver before registering the tasks'
    }
    $code = [regex]::Match($iss, '(?s)procedure RepairVddDriver;.*?\nend;').Value
    if ($code -notmatch 'vdd-driver-repair\.ps1' -or $code -notmatch "10: Message := 'Another app installed" -or
        $code -notmatch '11: Message' -or $code -notmatch 'VddInstallFailed := True' -or $code -notmatch 'RaiseException') {
        $failures += 'Setup does not report each repair failure'
    }
    if ($failures) { throw "Repair checks failed:`n  " + ($failures -join "`n  ") }
    Write-Output 'PASS: working, working beside broken, not owned, changed, unregistered, files missing, driverless with package gone, missing device, driver error, failed replacement, removal not done, rejected package, restart, rerun, Setup wiring'
} finally {
    Remove-Item $root -Recurse -Force
    Remove-Item function:global:pnputil.exe -ErrorAction SilentlyContinue
    Remove-Variable EMRepair -Scope Global -ErrorAction SilentlyContinue
    $global:LASTEXITCODE = 0
}
