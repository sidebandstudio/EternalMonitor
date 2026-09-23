$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP ('em-driver-removal-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force "$root\scripts","$root\driver","$root\vendor" | Out-Null
Copy-Item (Join-Path $PSScriptRoot '..\scripts\vdd-driver-remove.ps1') "$root\scripts\vdd-driver-remove.ps1"
$marker = "$root\driver\installed-by-eternalmonitor.txt"
$exe = "$root\vendor\unins000.exe"
[IO.File]::WriteAllText($exe, 'test fixture; never executed')
$global:EMTestRegistration = [pscustomobject]@{InstallLocation="$root\vendor";DisplayVersion='25.05.03'}
$global:EMTestCalls = 0
$global:EMTestExit = 0
function Get-ItemProperty { [CmdletBinding()] param([string[]]$Path) $global:EMTestRegistration }
function Start-Process {
    [CmdletBinding()] param([string]$FilePath,[string]$ArgumentList,[switch]$PassThru,[switch]$Wait)
    if ($FilePath -ne $exe -or $ArgumentList -notmatch '/NORESTART' -or !$Wait) { throw 'Unexpected uninstall invocation' }
    $global:EMTestCalls++
    [pscustomobject]@{ExitCode=$global:EMTestExit}
}
try {
    & "$root\scripts\vdd-driver-remove.ps1"
    if($global:EMTestCalls -ne 0){throw 'Preexisting driver was removed'}
    [IO.File]::WriteAllText($marker, $exe + "`r`n" + 'older-version')
    & "$root\scripts\vdd-driver-remove.ps1"
    if($global:EMTestCalls -ne 0){throw 'Changed driver was removed'}
    [IO.File]::WriteAllText($marker, $exe + "`r`n" + '25.05.03')
    & "$root\scripts\vdd-driver-remove.ps1"
    if($global:EMTestCalls -ne 1){throw 'Owned driver did not uninstall'}
    $global:EMTestExit=1
    $failed=$false
    try { & "$root\scripts\vdd-driver-remove.ps1" } catch { $failed=$true }
    if(!$failed){throw 'Vendor failure was ignored'}
    Write-Output 'PASS: preexisting, changed, owned driver and uninstall failure cases'
} finally { Remove-Item $root -Recurse -Force }
