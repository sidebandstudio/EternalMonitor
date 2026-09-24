$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (!(Test-Path "$root\input-probe.log")) { return }
$record = Get-Content "$root\input-probe.log" -First 1 | ConvertFrom-Json
if ($record.event -ne 'Ready') { throw 'No probe identity in its log' }
$p = Get-Process -Id $record.pid -ErrorAction SilentlyContinue
if (!$p) { return }
if (!$record.path -or !$record.start -or $p.Path -ne $record.path -or
    $p.StartTime.ToUniversalTime().Ticks.ToString() -ne $record.start) {
    throw 'The tracked probe identity changed; refusing to close it'
}
New-Item -ItemType File -Force "$root\probe.stop" | Out-Null
if (!$p.WaitForExit(10000)) {
    Stop-Process -Id $p.Id -Force
    throw 'Probe did not close after its stop flag; stopped the verified process'
}
Write-Output 'Tracked input probe exited'
