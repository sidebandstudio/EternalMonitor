param([Parameter(Mandatory=$true)][string]$Program, [switch]$Remove)
$ErrorActionPreference = 'Stop'
$Program = [IO.Path]::GetFullPath($Program)
$sha = [Security.Cryptography.SHA256]::Create()
try { $hash = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Program.ToLowerInvariant())))).Replace('-','').Substring(0,12) }
finally { $sha.Dispose() }
$name = "EternalMonitor dev $hash"
Get-NetFirewallRule -DisplayName "$name *" -ErrorAction SilentlyContinue | Remove-NetFirewallRule
if (!$Remove) {
    Get-NetFirewallApplicationFilter -Program $Program -ErrorAction SilentlyContinue |
        Get-NetFirewallRule | Where-Object { $_.Action -eq 'Block' } | Remove-NetFirewallRule
    foreach ($protocol in @('UDP','TCP')) {
        New-NetFirewallRule -DisplayName "$name $protocol" -Direction Inbound -Action Allow -Program $Program -Protocol $protocol -Profile Private,Public | Out-Null
    }
}
Write-Host "Firewall $name program=$Program removed=$Remove"
