# Run manually on the reference PC with an active console session.
param([string]$Runner=(Join-Path $PSScriptRoot 'Invoke-InSession.ps1'))
$ErrorActionPreference='Stop'
$output=(& $Runner -Command 'Start-Sleep -Seconds 2; Write-Output "Detached cleanup fixture finished"' -RunLevel Limited -Detach -TimeoutSec 30) -join "`n"
$match=[regex]::Match($output,'EM_JOB=([0-9]{8}-[0-9]{6}-[a-f0-9]{8})')
if(!$match.Success){throw 'Fixture job identity missing'}
$id=$match.Groups[1].Value
$name="EM-$id"
$job=Join-Path (Split-Path -Parent $PSScriptRoot) "jobs\$id"
try {
    $deadline=(Get-Date).AddSeconds(12)
    while(!(Test-Path "$job\exit.txt")) {
        if((Get-Date) -gt $deadline){throw 'Fixture did not finish'}
        Start-Sleep -Milliseconds 200
    }
    if((Get-Content "$job\exit.txt" -Raw).Trim() -ne '0'){throw 'Fixture command failed'}
    while(Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
        if((Get-Date) -gt $deadline){throw 'Completed Limited task could not unregister itself'}
        Start-Sleep -Milliseconds 200
    }
    Write-Output 'PASS: completed Limited detached task removed its registration'
} finally {
    $remaining=Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    if($remaining) {
        if($remaining.Actions.Arguments -ne "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$job\run.ps1`""){throw 'Cleanup identity mismatch'}
        Unregister-ScheduledTask -TaskName $name -Confirm:$false
    }
}
