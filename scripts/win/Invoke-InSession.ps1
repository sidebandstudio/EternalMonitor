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
Write-Host $users
$console = [regex]::Match($users, '(?im)^\s*>?\s*(\S+)\s+console\s+(\d+)\s+Active\s+(\S+)')
if (!$console.Success -or $console.Groups[1].Value -ne $env:USERNAME) {
    throw 'An active console session for the current user is required; RDP sessions are not suitable.'
}
if ($RequireIdle) {
    $idle = $console.Groups[3].Value
    $minutes = 0
    if ($idle -match '^(\d+)\+(\d+):(\d+)$') {
        $minutes = [int]$Matches[1]*1440 + [int]$Matches[2]*60 + [int]$Matches[3]
    } elseif ($idle -match '^(\d+):(\d+)$') {
        $minutes = [int]$Matches[1]*60 + [int]$Matches[2]
    } elseif ($idle -match '^\d+$') { $minutes = [int]$idle }
    if ($minutes -lt 2) { throw 'The console has been idle for less than two minutes. Retry when it is free.' }
}
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
$script = $template.Replace('__JOB__',$job).Replace('__TASK__',$task).Replace('__COMMAND__',$Command)
$script | Set-Content -Encoding UTF8 (Join-Path $job 'run.ps1')
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$job\run.ps1`""
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel $RunLevel
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds $TimeoutSec) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName $task
Write-Host "EM_JOB=$id"
Write-Host "EM_PATH=$job"
if ($Detach) { return }
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
