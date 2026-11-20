# Force a single marker write failure on upgrade without changing Setup or
# the task registrar. An empty directory at the marker path blocks the write.
param([Parameter(Mandatory)][string]$SetupLog)
$ErrorActionPreference = 'Stop'
$marker = $null
$watcher = [IO.FileSystemWatcher]::new((Split-Path -Parent $SetupLog), (Split-Path -Leaf $SetupLog))
$watcher.NotifyFilter = 'FileName, LastWrite, Size'
$watcher.EnableRaisingEvents = $true
$deadline = [datetime]::UtcNow.AddMinutes(10)
try {
    while ([datetime]::UtcNow -lt $deadline) {
        if (Test-Path -LiteralPath $SetupLog -PathType Leaf) {
            $log = Get-Content -LiteralPath $SetupLog -Raw
            if (!$marker -and $log -match 'Created protected temporary directory: ([^\r\n]+)\r?\n') {
                $temp = $Matches[1]
                if ((Split-Path -Leaf $temp) -notmatch '^is-[a-z0-9]+\.tmp$') {
                    throw 'Setup logged an unexpected temporary directory name.'
                }
                $candidate = Join-Path $temp 'vdd-tasks-verified'
                if (Test-Path -LiteralPath $candidate) { throw 'The task marker already exists.' }
                New-Item -ItemType Directory -Path $candidate | Out-Null
                $marker = $candidate
                Write-Host 'Blocked the first upgrade marker write.'
            }
            if ($marker -and $log -match 'Virtual display task registration was not verified\. Retrying\.') {
                Remove-Item -LiteralPath $marker
                $marker = $null
                Write-Host 'Cleared the marker path for the Setup retry.'
                return
            }
        }
        # Also recheck periodically in case a write arrived between this wait
        # and reading the log, or the watcher coalesced multiple writes.
        $null = $watcher.WaitForChanged([IO.WatcherChangeTypes]::All, 100)
    }
    throw 'Setup did not reach the marker retry.'
} finally {
    $watcher.Dispose()
    if ($marker -and (Test-Path -LiteralPath $marker -PathType Container)) {
        Remove-Item -LiteralPath $marker
    }
}
