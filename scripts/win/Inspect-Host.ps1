param(
    [ValidateSet('Stream','Settings','QR')][string]$View = 'Stream',
    [Parameter(Mandatory=$true)][string]$Path,
    [switch]$Connected,
    [switch]$TestAutostart
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
$pidCondition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty,$p.Id)
do {
    $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children,$pidCondition)
    $window = @($windows | Where-Object { $_.Current.Name -eq 'EternalMonitor // SIGNAL' }) | Select-Object -First 1
    if ($window) { break }
    if ((Get-Date) -gt $deadline) { throw 'The tracked host has no EternalMonitor GUI window' }
    Start-Sleep -Milliseconds 100
} while ($true)
$handle = [IntPtr]$window.Current.NativeWindowHandle
if ($window.Current.ProcessId -ne $p.Id -or $handle -eq [IntPtr]::Zero) { throw 'Unexpected GUI process' }
# The Rust process can own a console as well as its actual GUI window.
# Raise only the named GUI belonging to the verified tracked process.
if (![EMHostWindow]::SetWindowPos($handle,[IntPtr](-1),30,10,1100,1000,0x40)) { throw 'Could not raise the tracked host' }
[void][EMHostWindow]::SetForegroundWindow($handle)
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
Invoke-HostControl $(if ($View -eq 'Settings') { 'SETTINGS' } else { 'STREAM' })
if ($View -eq 'QR') { Invoke-HostControl 'QR CODE' }
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
$autostart = $null
if ($TestAutostart) {
    if ($View -ne 'Settings') { throw 'Autostart must be tested from Settings' }
    $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty,'Start on Windows startup')
    $checkbox = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$condition)
    $toggle = $null
    if (!$checkbox -or !$checkbox.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern,[ref]$toggle)) {
        throw 'Startup checkbox has no UI Automation toggle'
    }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run',$true)
    if (!$key) { throw 'Windows Run registry key is absent' }
    $originalPresent = $key.GetValueNames() -contains 'EternalMonitor'
    $originalValue = $key.GetValue('EternalMonitor',$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $originalKind = if ($originalPresent) { $key.GetValueKind('EternalMonitor') } else { $null }
    function Set-StartupFlag([bool]$Enabled) {
        $current = $toggle.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On
        if ($current -ne $Enabled) { $toggle.Toggle() }
        $deadline = (Get-Date).AddSeconds(5)
        while (($key.GetValueNames() -contains 'EternalMonitor') -ne $Enabled -or
               (($toggle.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On) -ne $Enabled)) {
            if ((Get-Date) -gt $deadline) { throw 'Startup checkbox did not update the registry' }
            Start-Sleep -Milliseconds 50
        }
    }
    try {
        Set-StartupFlag $false
        Set-StartupFlag $true
        if ($key.GetValue('EternalMonitor') -ne ('"' + $p.Path + '"')) {
            throw 'Startup command must contain the quoted, current host executable'
        }
        Set-StartupFlag $false
        $autostart = @{ enabled_path_verified=$true; disabled_value_absent=$true }
    } finally {
        try { Set-StartupFlag $originalPresent } finally {
            try {
                if ($originalPresent) { $key.SetValue('EternalMonitor',$originalValue,$originalKind) }
                else { $key.DeleteValue('EternalMonitor',$false) }
            } finally { $key.Dispose() }
        }
    }
    $autostart.restored = $true
}
$rect = $window.Current.BoundingRectangle
if ($rect.Width -lt 100 -or $rect.Height -lt 100) { throw 'Invalid host window bounds' }
New-Item -ItemType Directory -Force (Split-Path $Path -Parent) | Out-Null
$bitmap = New-Object Drawing.Bitmap ([int]$rect.Width),([int]$rect.Height)
$graphics = [Drawing.Graphics]::FromImage($bitmap)
try {
    $graphics.CopyFromScreen([int]$rect.X,[int]$rect.Y,0,0,$bitmap.Size)
    $bitmap.Save($Path,[Drawing.Imaging.ImageFormat]::Png)
} finally { $graphics.Dispose(); $bitmap.Dispose() }
@{pid=$p.Id; view=$View; labels=$names; screenshot=$Path; autostart=$autostart} | ConvertTo-Json -Depth 4
