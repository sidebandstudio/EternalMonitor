param([Parameter(Mandatory=$true)][string]$Path)
$ErrorActionPreference = 'Stop'
if (![Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'Screenshot must run inside the interactive console session'
}
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EMScreenshotDpi {
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
}
'@
New-Item -ItemType Directory -Force (Split-Path $Path -Parent) | Out-Null
# PowerShell is only system-DPI-aware, so a monitor at another scale, such as
# the iPad's virtual display at 125%, was measured and captured too small.
# Per-monitor v2 awareness gives every monitor's full physical pixels.
if ([EMScreenshotDpi]::SetThreadDpiAwarenessContext([IntPtr](-4)) -eq [IntPtr]::Zero) {
    throw 'Could not make the screenshot thread per-monitor DPI aware'
}
$rect = [Windows.Forms.SystemInformation]::VirtualScreen
$bitmap = New-Object Drawing.Bitmap $rect.Width,$rect.Height
$graphics = [Drawing.Graphics]::FromImage($bitmap)
try {
    $graphics.CopyFromScreen($rect.Location, [Drawing.Point]::Empty, $rect.Size)
    $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
    Write-Host "Screenshot: $Path $($rect.Width)x$($rect.Height) session=$((Get-Process -Id $PID).SessionId)"
} finally { $graphics.Dispose(); $bitmap.Dispose() }
