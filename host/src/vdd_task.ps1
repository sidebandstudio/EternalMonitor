param([string]$Task, [string]$After)
$ErrorActionPreference = 'Stop'
try {
    $service = New-Object -ComObject Schedule.Service
    $service.Connect()
    $registered = $service.GetFolder('\').GetTask($Task)
    # Finish any prior invocation before requesting another one. The host
    # serializes both directions so a late disable cannot undo an enable.
    # Waiting for it must not eat into this run's own time.
    $deadline = (Get-Date).AddSeconds(10)
    while ($registered.State -in @(2,4)) {
        if ((Get-Date) -gt $deadline) { throw "Previous VDD task timed out: $Task" }
        Start-Sleep -Milliseconds 50
        $registered = $service.GetFolder('\').GetTask($Task)
    }
    # The host stopped waiting for an earlier run of $After, which can still
    # finish. A Disable that ran first would be undone when the Enable ends.
    if ($After) {
        $earlier = $service.GetFolder('\').GetTask($After)
        while ($earlier.State -in @(2,4)) {
            if ((Get-Date) -gt $deadline) { throw "Previous VDD task timed out: $After" }
            Start-Sleep -Milliseconds 50
            $earlier = $service.GetFolder('\').GetTask($After)
        }
    }
    if (!$registered.Enabled -or !$registered.Definition.Settings.AllowDemandStart) {
        throw "VDD task cannot be started: $Task"
    }
    $running = $registered.Run($null)
    if (!$running) { throw "VDD task did not start: $Task" }
    $deadline = (Get-Date).AddSeconds(15)
    while ($true) {
        if ((Get-Date) -gt $deadline) { throw "VDD task timed out: $Task" }
        try { $running.Refresh() } catch {
            # SCHED_E_TASK_NOT_RUNNING means the recorded instance completed.
            if ($_.Exception.GetBaseException().HResult -eq -2147216629) { break }
            throw
        }
        if ($running.State -notin @(2,4)) { break }
        Start-Sleep -Milliseconds 50
    }
    $registered = $service.GetFolder('\').GetTask($Task)
    if ($registered.LastTaskResult -ne 0) {
        throw "VDD task failed: $Task, result=$($registered.LastTaskResult)"
    }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
