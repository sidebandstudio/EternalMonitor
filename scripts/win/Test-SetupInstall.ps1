# Exercise the built installer on a fresh, elevated windows-2022 CI runner.
param(
    [Parameter(Mandatory)][string]$Setup,
    [Parameter(Mandatory)][string]$LogDirectory
)

$ErrorActionPreference = 'Stop'
$taskNames = 'EternalMonitor VDD Enable', 'EternalMonitor VDD Disable'
$uninstallRoot = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall'
$installDir = Join-Path $env:ProgramFiles 'EternalMonitor'

# Keep in sync with SETUP_FLAGS in updater/src/lib.rs. Setup skips the app
# launch in silent mode and this test does not run the updater's relaunch.
$setupFlags = @('/SP-', '/SILENT', '/SUPPRESSMSGBOXES', '/NORESTART',
                '/CLOSEAPPLICATIONS', '/NORESTARTAPPLICATIONS')

function Invoke-InstallerPhase([string]$Phase, [string]$Exe, [string[]]$Flags) {
    $log = Join-Path $LogDirectory "$Phase.log"
    $commandLine = ($Flags -join ' ') + " /LOG=`"$log`""
    $startedAt = [datetime]::UtcNow
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $limit = if ($Phase -eq 'fresh-install') { 240 } else { 600 }
    # On Windows -Wait tracks descendants. Inno's uninstaller launcher can
    # exit before its temporary copy has finished removing the directory.
    $job = Start-ThreadJob -ScriptBlock {
        $ErrorActionPreference = 'Stop'
        (Start-Process -FilePath $using:Exe -ArgumentList $using:commandLine `
            -WorkingDirectory $using:LogDirectory -PassThru -Wait).ExitCode
    }
    $retryJob = $null
    try {
        if ($Phase -eq 'upgrade') {
            $retryJob = Start-ThreadJob -FilePath (Join-Path $PSScriptRoot 'Block-SetupSmokeMarker.ps1') -ArgumentList $log
        }
        if (!(Wait-Job $job -Timeout $limit)) {
            try {
                $capture = @{Phase=$Phase; LogDirectory=$LogDirectory; SetupLog=$log; StartedAt=$startedAt}
                $processes = @(& (Join-Path $PSScriptRoot 'Get-SetupTimeout.ps1') @capture)
            } catch {
                Write-Warning "Timeout diagnostics failed. $($_.Exception.Message)"
            }
            if (!$processes) {
                $logArgument = [regex]::Escape(('/LOG="{0}"' -f $log))
                $processes = Get-CimInstance Win32_Process -OperationTimeoutSec 10 |
                    Where-Object CommandLine -Match $logArgument
            }
            foreach ($item in $processes) {
                $process = Get-Process -Id $item.ProcessId -ErrorAction SilentlyContinue
                if ($process -and !$process.HasExited) {
                    try { $process.Kill($true) }
                    catch [InvalidOperationException] {
                        if (!$process.HasExited) { throw }
                    }
                }
            }
            throw "$Phase timed out after $limit seconds. See $log."
        }
        $code = Receive-Job $job
        Write-Host ('{0} exit code {1} after {2:F1} seconds.' -f $Phase, $code, $timer.Elapsed.TotalSeconds)
        if ($null -eq $code -or $code -ne 0) {
            throw "$Phase expected exit code 0, got '$code'. Setup code 20 means driver failure and 21 means unverified task registration. See $log."
        }
        if ($retryJob) {
            if (!(Wait-Job $retryJob -Timeout 2)) { throw 'Upgrade did not reach the deliberately blocked marker retry.' }
            Receive-Job $retryJob
        }
        Get-Content -LiteralPath $log -Raw
    } finally {
        if ($retryJob) { Remove-Job $retryJob -Force }
        Remove-Job $job -Force
    }
}

function Get-AppUninstallEntry {
    Get-ItemProperty -Path "$uninstallRoot\*" | Where-Object DisplayName -eq 'EternalMonitor'
}

function Get-DriverRegistration {
    # Setup checks the native uninstall key before the 32-bit view.
    $entry = Get-ItemProperty -LiteralPath "$uninstallRoot\VirtualDisplayDriver_is1" -ErrorAction SilentlyContinue
    if (!$entry -or !$entry.PSObject.Properties['InstallLocation']) {
        $entry = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1' -ErrorAction SilentlyContinue
    }
    if ($entry.InstallLocation -and $entry.DisplayVersion) { $entry }
}

function Assert-VddTasks([string]$Phase) {
    foreach ($name in $taskNames) {
        $task = Get-ScheduledTask -TaskPath '\' -TaskName $name
        $user = $task.Principal.UserId
        $disallow = $task.Settings.DisallowStartIfOnBatteries
        $stop = $task.Settings.StopIfGoingOnBatteries
        Write-Host "$Phase task '$name' runs as $user. DisallowStartIfOnBatteries=$disallow, StopIfGoingOnBatteries=$stop."
        if ($user -notin @('SYSTEM', 'NT AUTHORITY\SYSTEM', 'S-1-5-18') -or
            $false -ne $disallow -or $false -ne $stop) {
            throw "$Phase task '$name' must run as SYSTEM and be allowed to start and keep running on battery."
        }
    }
}

function Assert-TaskRegistrationLog([string]$Phase, [string]$Log) {
    # The registrar stops old instances before it writes the marker. Setup
    # logs the call with -ResultMarker and separately logs its marker check.
    # The registrar does not log individual Stop calls on idle tasks.
    if ($Log -notmatch '(?m)Parameters: [^\r\n]*\\vdd-tasks-setup\.ps1" -ResultMarker [^\r\n]*vdd-tasks-verified' -or
        $Log -notmatch 'Both virtual display tasks were verified\.') {
        throw "$Phase log is missing the stop/register script invocation or Setup's successful marker check."
    }
    Write-Host "$Phase ran the stop/register script and Setup verified the marker."
}

function Assert-DriverOutcome([string]$Phase, [string]$Log, [bool]$PreserveDriver, [bool]$RecordedOwnership) {
    # Server 2022 build 20348 is below Setup's Windows 11 threshold of 22000.
    # NeedsVddInstall still runs the vendor setup if the package is absent.
    # FailVddStep logs repair/binding failures and continues on this build.
    # A successful install does not require a working virtual screen here.
    if ($PreserveDriver) {
        if ($Log -notmatch 'Preserving existing Virtual Display Driver package') {
            throw "$Phase did not log preservation of the registered driver."
        }
        Write-Host "$Phase preserved the registered driver package."
    } else {
        if ($Log -notmatch '(?m)^\S+ \S+ +Filename: [^\r\n]*\\vdd-setup-x64\.exe\r?$') {
            throw "$Phase did not attempt the bundled driver installer."
        }
        Write-Host "$Phase attempted the bundled driver installer."
    }

    $fallback = [regex]::Matches($Log, 'Virtual Display Driver step failed on Windows 10, Setup goes on: [^\r\n]+')
    foreach ($message in $fallback) { Write-Host "$Phase driver: $($message.Value)" }
    # RecordVddOwnership still requires the vendor's uninstall registration
    # and uninstaller, even on Windows 10. Inno can log an AfterInstall
    # exception and return 0, so require the ownership file for a fresh run.
    if (!$PreserveDriver -and !$RecordedOwnership) {
        throw "$Phase did not record ownership of the bundled driver. See Setup's log for the registration or uninstaller failure."
    }
    if (!$fallback.Count) { Write-Host "$Phase logged no Windows 10 driver fallback." }
}

$Setup = (Resolve-Path -LiteralPath $Setup).Path
$LogDirectory = (New-Item -ItemType Directory -Path $LogDirectory -Force).FullName
try {
    $build = [Environment]::OSVersion.Version.Build
    if ($build -ne 20348) { throw "Expected Windows Server 2022 build 20348, got $build." }
    Write-Host "Windows Server 2022 build $build follows Setup's Windows 10 path."

    if ((Test-Path -LiteralPath $installDir) -or (Get-AppUninstallEntry) -or (Get-DriverRegistration) -or
        (Get-ScheduledTask -TaskPath '\' | Where-Object TaskName -in $taskNames)) {
        throw 'Setup smoke requires a clean runner with no EternalMonitor files, tasks or registered VDD package.'
    }

    $freshLog = Invoke-InstallerPhase 'fresh-install' $Setup $setupFlags
    foreach ($file in 'EternalMonitor-host.exe', 'EternalMonitor-updater.exe', 'unins000.exe') {
        if (!(Test-Path -LiteralPath (Join-Path $installDir $file) -PathType Leaf)) {
            throw "Fresh install is missing $file under $installDir."
        }
    }
    if (!(Get-ChildItem -LiteralPath $installDir -Filter 'avcodec-*.dll' -File)) {
        throw "Fresh install is missing the FFmpeg codec DLL under $installDir."
    }
    $uninstaller = Join-Path $installDir 'unins000.exe'
    $entry = @(Get-AppUninstallEntry)
    if ($entry.Count -ne 1 -or $entry[0].UninstallString -ne ('"{0}"' -f $uninstaller)) {
        throw 'Fresh install is missing the expected machine-wide EternalMonitor uninstall entry.'
    }
    Write-Host 'Fresh install has the host, updater, FFmpeg codec DLL and registered uninstaller.'
    Assert-VddTasks 'fresh-install'
    Assert-TaskRegistrationLog 'fresh-install' $freshLog
    $ownershipPath = Join-Path $installDir 'driver\installed-by-eternalmonitor.txt'
    $freshOwnership = Test-Path -LiteralPath $ownershipPath -PathType Leaf

    $preserveOnUpgrade = [bool](Get-DriverRegistration)
    $upgradeLog = Invoke-InstallerPhase 'upgrade' $Setup $setupFlags
    Assert-VddTasks 'upgrade'
    Assert-TaskRegistrationLog 'upgrade' $upgradeLog
    if ([regex]::Matches($upgradeLog, 'Virtual display task registration was not verified\. Retrying\.').Count -ne 1 -or
        $upgradeLog -notmatch 'Virtual display task registration retry exited with code 0\b') {
        throw 'Upgrade did not retry the deliberately blocked marker write exactly once and succeed.'
    }
    Write-Host 'Upgrade recovered from the first marker write failure on one retry.'
    $upgradeOwnership = Test-Path -LiteralPath $ownershipPath -PathType Leaf

    $null = Invoke-InstallerPhase 'uninstall' $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
    if ((Test-Path -LiteralPath $installDir) -or (Get-AppUninstallEntry)) {
        throw 'Uninstall left the install directory or the uninstall entry behind.'
    }
    $remaining = Get-ScheduledTask -TaskPath '\' | Where-Object TaskName -in $taskNames
    if ($remaining) { throw "Uninstall left scheduled tasks behind: $($remaining.TaskName -join ', ')." }
    Write-Host 'Uninstall removed the install directory, uninstall entry and both scheduled tasks.'

    # Check these after the lifecycle so a driver packaging error does not
    # keep us from seeing what Setup did on upgrade and uninstall.
    Assert-DriverOutcome 'fresh-install' $freshLog $false $freshOwnership
    Assert-DriverOutcome 'upgrade' $upgradeLog $preserveOnUpgrade $upgradeOwnership
} finally {
    Get-ChildItem -LiteralPath $LogDirectory -Filter '*.log' -File | ForEach-Object {
        Write-Host "Last 80 lines of $($_.Name)"
        Get-Content -LiteralPath $_.FullName -Tail 80
    }
}
