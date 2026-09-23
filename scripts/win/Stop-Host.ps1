param([switch]$Force)
$ErrorActionPreference = 'Stop'
$file = 'D:\AgentWork\em-v030\host.pid.json'
if (!(Test-Path $file)) { return }
$record = Get-Content $file -Raw | ConvertFrom-Json
$p = Get-Process -Id $record.id -ErrorAction SilentlyContinue
if (!$p) { Remove-Item $file; return }
if ($p.Path -ne $record.path -or $p.StartTime.ToUniversalTime().Ticks.ToString() -ne $record.start) {
    throw 'The tracked host process identity changed; refusing to stop it'
}
$graceful = $false
if (!$Force) {
    if ($p.MainWindowHandle -ne [IntPtr]::Zero) {
        $graceful = $p.CloseMainWindow()
    } else {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EMHostConsole {
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool AttachConsole(uint pid);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetConsoleCtrlHandler(IntPtr handler, bool add);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GenerateConsoleCtrlEvent(uint signal, uint group);
    public static bool Stop(uint pid) {
        FreeConsole();
        if (!AttachConsole(pid)) return false;
        try {
            if (!SetConsoleCtrlHandler(IntPtr.Zero,true)) return false;
            return GenerateConsoleCtrlEvent(0,0);
        } finally { FreeConsole(); }
    }
}
'@
        $graceful = [EMHostConsole]::Stop([uint32]$p.Id)
    }
    if ($graceful) { [void]$p.WaitForExit(10000) }
}
$p.Refresh()
if (!$p.HasExited) {
    & taskkill /PID $p.Id /T /F | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Tracked host cleanup failed' }
    [void]$p.WaitForExit(5000)
    if ($p.HasExited) { Remove-Item $file }
    if (!$Force) { throw 'Graceful host shutdown failed; the tracked process was force-stopped for cleanup' }
} else { Remove-Item $file }
Write-Output $(if ($Force) { 'Tracked host was killed' } else { 'Tracked host exited gracefully' })
