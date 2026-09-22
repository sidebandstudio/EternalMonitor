param(
    [Parameter(Mandatory=$true)][string]$Command,
    [int]$TimeoutSec = 600,
    [ValidateSet('Highest','Limited')][string]$RunLevel = 'Highest',
    [switch]$Detach,
    [switch]$RequireIdle
)
$ErrorActionPreference = 'Stop'
$root = 'D:\AgentWork\em-v030'
$users = (& query user 2>&1 | Out-String)
Write-Output $users
$console = [regex]::Match($users, '(?im)^\s*>?\s*(\S+)\s+console\s+(\d+)\s+Active\s+(\S+)')
if (!$console.Success -or $console.Groups[1].Value -ne $env:USERNAME) {
    throw 'An active console session for the current user is required; RDP sessions are not suitable.'
}
# The console's `query user` idle column can remain "none" for hours.
# Validate actual input idle time inside that same interactive session,
# immediately before the requested command is allowed to run.
if ($TimeoutSec -lt 1) { throw 'TimeoutSec must be positive' }
$id = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N').Substring(0,8)
$job = Join-Path $root "jobs\$id"
$task = "EM-$id"
New-Item -ItemType Directory -Force $job | Out-Null
$template = @'
$ErrorActionPreference = 'Stop'
$env:TEMP = 'D:\AgentWork\temp'
$env:TMP = $env:TEMP
Set-Location 'D:\AgentWork\em-v030'
$job = '__JOB__'
$task = '__TASK__'
$code = 0
$PID | Set-Content "$job\pid.txt"
Start-Transcript -Path "$job\transcript.txt" -Force | Out-Null
try {
    $global:LASTEXITCODE = 0
    $sessionId = [System.Diagnostics.Process]::GetCurrentProcess().SessionId
    if ($sessionId -ne __SESSION_ID__) { throw 'Task did not start in the expected console session' }
    if (__REQUIRE_IDLE__) {
        Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class EMInputIdle {
    [StructLayout(LayoutKind.Sequential)] struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetLastInputInfo(ref LASTINPUTINFO info);
    public static double Seconds() {
        var info = new LASTINPUTINFO { cbSize = 8 };
        if (!GetLastInputInfo(ref info)) throw new System.ComponentModel.Win32Exception();
        return unchecked((uint)Environment.TickCount - info.dwTime) / 1000.0;
    }
}
"@
        $idleSeconds = [EMInputIdle]::Seconds()
        "Console preflight: session=$sessionId idle_seconds=$idleSeconds" | Set-Content "$job\preflight.txt"
        if ($idleSeconds -lt 120) { throw 'The console has been idle for less than two minutes. Retry when it is free.' }
    }
    'ready' | Set-Content "$job\ready.txt"
    & {
__COMMAND__
    } *> "$job\out.txt"
    if ($LASTEXITCODE -ne 0) { $code = $LASTEXITCODE }
} catch {
    $_ | Out-String | Add-Content "$job\out.txt"
    $code = 1
} finally {
    Stop-Transcript | Out-Null
    $code | Set-Content "$job\exit.txt"
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
}
exit $code
'@
$requireIdleLiteral = if ($RequireIdle) { '$true' } else { '$false' }
$script = $template.Replace('__JOB__',$job).Replace('__TASK__',$task).Replace('__COMMAND__',$Command).Replace('__SESSION_ID__',$console.Groups[2].Value).Replace('__REQUIRE_IDLE__',$requireIdleLiteral)
$script | Set-Content -Encoding UTF8 (Join-Path $job 'run.ps1')
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$job\run.ps1`""
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel $RunLevel
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds $TimeoutSec) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName $task
Write-Output "EM_JOB=$id"
Write-Output "EM_PATH=$job"
$startupDeadline = (Get-Date).AddSeconds([Math]::Min($TimeoutSec,30))
try {
    while (!(Test-Path "$job\ready.txt") -and !(Test-Path "$job\exit.txt")) {
        if ((Get-Date) -gt $startupDeadline) { throw "Interactive preflight timed out; evidence: $job" }
        Start-Sleep -Milliseconds 200
    }
    if (Test-Path "$job\preflight.txt") { Get-Content "$job\preflight.txt" }
} catch {
    Stop-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
    throw
}
if ($Detach -and (Test-Path "$job\ready.txt")) { return }
$deadline = (Get-Date).AddSeconds($TimeoutSec)
try {
    while (!(Test-Path "$job\exit.txt")) {
        if ((Get-Date) -gt $deadline) {
            Stop-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue
            throw "Interactive job timed out; evidence: $job"
        }
        Start-Sleep -Milliseconds 200
    }
    Get-Content "$job\out.txt"
    $code = [int](Get-Content "$job\exit.txt")
    if ($code -ne 0) { throw "Interactive job failed with exit $code; evidence: $job" }
} finally {
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
}
