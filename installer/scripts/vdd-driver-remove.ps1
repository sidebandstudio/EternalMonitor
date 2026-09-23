# Always invoked by the uninstaller, including upgrades from older uninstall logs.
# Remove only the exact driver package this application installed.
$ErrorActionPreference = 'Stop'
$marker = Join-Path $PSScriptRoot '..\driver\installed-by-eternalmonitor.txt'
if (!(Test-Path $marker)) {
    Write-Output 'Preserving preexisting Virtual Display Driver.'
    return
}
$registration = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1' -ErrorAction SilentlyContinue |
    Where-Object { $_.InstallLocation -and $_.DisplayVersion } | Select-Object -First 1
if (!$registration) { return }
$uninstaller = Join-Path $registration.InstallLocation 'unins000.exe'
$expected = $uninstaller + "`r`n" + $registration.DisplayVersion
if ([IO.File]::ReadAllText($marker) -ne $expected) {
    Write-Output 'Preserving Virtual Display Driver changed outside EternalMonitor.'
    return
}
if (!(Test-Path $uninstaller)) { throw 'The owned Virtual Display Driver uninstaller is missing.' }
$process = Start-Process -PassThru -Wait -FilePath $uninstaller -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART'
if ($process.ExitCode -notin @(0, 3010)) { throw "Virtual Display Driver uninstall failed with exit $($process.ExitCode)." }
