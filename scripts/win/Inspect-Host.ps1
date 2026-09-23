param(
    [ValidateSet('Stream','Settings','QR')][string]$View = 'Stream',
    [Parameter(Mandatory=$true)][string]$Path,
    [switch]$Connected
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EMHostWindow {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hgt, uint flags);
}
'@
$record = Get-Content 'D:\AgentWork\em-v030\host.pid.json' -Raw | ConvertFrom-Json
$p = Get-Process -Id $record.id
if ($p.Path -ne $record.path -or $p.StartTime.ToUniversalTime().Ticks.ToString() -ne $record.start) {
    throw 'The tracked host identity changed'
}
$deadline = (Get-Date).AddSeconds(15)
do {
    $p.Refresh()
    if ($p.MainWindowHandle -ne [IntPtr]::Zero) { break }
    if ((Get-Date) -gt $deadline) { throw 'The tracked host has no GUI window' }
    Start-Sleep -Milliseconds 100
} while ($true)
$window = [System.Windows.Automation.AutomationElement]::FromHandle($p.MainWindowHandle)
if ($window.Current.ProcessId -ne $p.Id) { throw 'Unexpected GUI process' }
# Raise only our tracked window over our test pattern. No synthetic keyboard or
# pointer input is used, and the session runner checks actual input idle first.
if (![EMHostWindow]::SetWindowPos($p.MainWindowHandle,[IntPtr](-1),30,10,1100,1000,0x40)) { throw 'Could not raise the tracked host' }
[void][EMHostWindow]::SetForegroundWindow($p.MainWindowHandle)
function Invoke-HostControl([string]$Name) {
    $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty,$Name)
    $deadline = (Get-Date).AddSeconds(5)
    do {
        $control = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$condition)
        if ($control) { break }
        if ((Get-Date) -gt $deadline) { throw "Host control is absent: $Name" }
        Start-Sleep -Milliseconds 100
    } while ($true)
    $pattern = $null
    if ($control.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) { $pattern.Invoke() }
    elseif ($control.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern,[ref]$pattern)) { $pattern.Select() }
    else { throw "Host control cannot be invoked through UI Automation: $Name" }
}
Invoke-HostControl $(if ($View -eq 'Settings') { 'Settings' } else { 'Stream' })
if ($View -eq 'QR') { Invoke-HostControl 'QR code' }
Start-Sleep -Milliseconds 300
$nodes = $window.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition)
$names = @($nodes | ForEach-Object { $_.Current.Name } | Where-Object { $_ })
if ($View -eq 'Stream') {
    foreach ($label in @('PAIRING','USB CONNECTION','PC AUDIO')) {
        if (!($names -contains $label)) { throw "Host Stream card absent: $label" }
    }
    if ($Connected) {
        foreach ($label in @('CONNECTED IPAD','Decode rate','Repaired fragments')) {
            if (!($names -contains $label)) { throw "Connected client evidence absent: $label" }
        }
        if ($names -contains 'Waiting for an iPad') { throw 'Host GUI has no connected client' }
    }
}
if ($View -eq 'Settings' -and !($names -contains 'Encoder input')) { throw 'Settings content did not appear' }
if ($View -eq 'QR' -and !($names -contains 'QR Code')) { throw 'QR modal did not appear' }
$rect = $window.Current.BoundingRectangle
if ($rect.Width -lt 100 -or $rect.Height -lt 100) { throw 'Invalid host window bounds' }
New-Item -ItemType Directory -Force (Split-Path $Path -Parent) | Out-Null
$bitmap = New-Object Drawing.Bitmap ([int]$rect.Width),([int]$rect.Height)
$graphics = [Drawing.Graphics]::FromImage($bitmap)
try {
    $graphics.CopyFromScreen([int]$rect.X,[int]$rect.Y,0,0,$bitmap.Size)
    $bitmap.Save($Path,[Drawing.Imaging.ImageFormat]::Png)
} finally { $graphics.Dispose(); $bitmap.Dispose() }
@{pid=$p.Id; view=$View; labels=$names; screenshot=$Path} | ConvertTo-Json -Depth 4
