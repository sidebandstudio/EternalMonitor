param([Parameter(Mandatory=$true)][string]$Path)
$ErrorActionPreference = 'Stop'
if (![Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'Screenshot must run inside the interactive console session'
}
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
New-Item -ItemType Directory -Force (Split-Path $Path -Parent) | Out-Null
$rect = [Windows.Forms.SystemInformation]::VirtualScreen
$bitmap = New-Object Drawing.Bitmap $rect.Width,$rect.Height
$graphics = [Drawing.Graphics]::FromImage($bitmap)
try {
    $graphics.CopyFromScreen($rect.Location, [Drawing.Point]::Empty, $rect.Size)
    $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
    Write-Host "Screenshot: $Path $($rect.Width)x$($rect.Height) session=$((Get-Process -Id $PID).SessionId)"
} finally { $graphics.Dispose(); $bitmap.Dispose() }
