# Collect evidence on a disposable Windows CI runner before terminating Setup.
param(
    [Parameter(Mandatory)][string]$Phase,
    [Parameter(Mandatory)][string]$LogDirectory,
    [Parameter(Mandatory)][string]$SetupLog,
    [Parameter(Mandatory)][datetime]$StartedAt
)

$ErrorActionPreference = 'Stop'
$prefix = Join-Path $LogDirectory "$Phase-timeout"
$notes = [Collections.Generic.List[string]]::new()
$notes.Add("Captured $([datetime]::UtcNow.ToString('o')). Runner session $([Diagnostics.Process]::GetCurrentProcess().SessionId).")
$all = @()
$roots = @()
$selected = @()
try {
    $all = @(Get-CimInstance Win32_Process -OperationTimeoutSec 10)
    $logArgument = [regex]::Escape(('/LOG="{0}"' -f $SetupLog))
    $roots = @($all | Where-Object CommandLine -Match $logArgument)
    $tree = [Collections.Generic.HashSet[uint32]]::new()
    foreach ($root in $roots) { [void]$tree.Add($root.ProcessId) }
    do {
        $changed = $false
        foreach ($item in $all) {
            if ($tree.Contains($item.ParentProcessId) -and $tree.Add($item.ProcessId)) { $changed = $true }
        }
    } while ($changed)
    $helpers = 'pnputil.exe', 'drvinst.exe', 'rundll32.exe', 'consent.exe', 'DeviceInstall.exe',
               'DeviceCensus.exe', 'nefconw.exe', 'vdd-setup-x64.exe'
    $selected = @($all | Where-Object { $tree.Contains($_.ProcessId) -or $_.Name -in $helpers })
    $processDetails = @($selected | Sort-Object CreationDate | Select-Object ProcessId, ParentProcessId,
        SessionId, CreationDate, Name, ExecutablePath, CommandLine,
        @{Name='InSetupTree'; Expression={$tree.Contains($_.ProcessId)}})
    ConvertTo-Json -InputObject $processDetails -Depth 3 |
        Set-Content -LiteralPath "$prefix-processes.json"
    $notes.Add("Matched $($roots.Count) Setup command lines and $($tree.Count) processes in their trees.")
} catch { $notes.Add("Process enumeration failed. $($_.Exception.Message)") }

try {
    Get-Content -LiteralPath (Join-Path $env:SystemRoot 'INF\setupapi.dev.log') -Tail 200 -Encoding ansi |
        Set-Content -LiteralPath "$prefix-setupapi.dev.log"
} catch { $notes.Add("SetupAPI log could not be read. $($_.Exception.Message)") }

try {
    # Inno's default /LOG file is named "Setup Log ... .txt". The pinned
    # vendor wrapper has default logging disabled, so a log may be absent.
    $locations = @($env:TEMP, (Join-Path $env:SystemRoot 'Temp'))
    foreach ($process in $selected) {
        if ($process.Name -like '*.tmp' -and $process.ExecutablePath) {
            $locations += Split-Path -Parent $process.ExecutablePath
        }
    }
    $vendorLogs = @($locations | Sort-Object -Unique | ForEach-Object {
        Get-ChildItem -LiteralPath $_ -File -ErrorAction SilentlyContinue | Where-Object {
            $_.LastWriteTimeUtc -ge $StartedAt -and
            $_.Name -match '^(Setup Log .*\.txt|.*(?:vdd|setup).*\.log)$'
        }
    })
    $index = 0
    foreach ($file in $vendorLogs) {
        $index++
        $notes.Add("Collected possible vendor log $index from $($file.FullName).")
        # Normalize text for the workflow's artifact secret scan.
        Get-Content -LiteralPath $file.FullName -Raw | Set-Content -LiteralPath "$prefix-vendor-$index.log"
    }
    if (!$vendorLogs.Count) { $notes.Add('No possible vendor log found in the installer process and system temp directories.') }
} catch { $notes.Add("Vendor log collection failed. $($_.Exception.Message)") }

try {
    if (!('SetupTimeout.Windows' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace SetupTimeout {
    public static class Windows {
        [UnmanagedFunctionPointer(CallingConvention.Winapi, CharSet = CharSet.Unicode)]
        private delegate bool DesktopCallback(string name, IntPtr data);
        private delegate bool WindowCallback(IntPtr window, IntPtr data);

        [DllImport("user32.dll")] private static extern IntPtr GetProcessWindowStation();
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool EnumDesktops(IntPtr station, DesktopCallback callback, IntPtr data);
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr OpenDesktop(string name, uint flags, bool inherit, uint access);
        [DllImport("user32.dll")] private static extern bool CloseDesktop(IntPtr desktop);
        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool EnumDesktopWindows(IntPtr desktop, WindowCallback callback, IntPtr data);
        [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetWindowText(IntPtr window, StringBuilder text, int capacity);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetClassName(IntPtr window, StringBuilder text, int capacity);
        [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);

        public record Window(string Desktop, uint ProcessId, string Title, string Class, bool Visible);

        public static List<Window> List(List<string> notes) {
            var windows = new List<Window>();
            var desktops = new List<string>();
            if (!EnumDesktops(GetProcessWindowStation(), (name, data) => {
                desktops.Add(name);
                return true;
            }, IntPtr.Zero))
                notes.Add("EnumDesktops failed with Win32 error " + Marshal.GetLastWin32Error() +
                          ". Trying the current desktop.");

            void Collect(IntPtr desktop, string name) {
                int before = windows.Count;
                if (!EnumDesktopWindows(desktop, (window, data) => {
                    GetWindowThreadProcessId(window, out uint process);
                    var title = new StringBuilder(1024);
                    var type = new StringBuilder(256);
                    GetWindowText(window, title, title.Capacity);
                    GetClassName(window, type, type.Capacity);
                    windows.Add(new Window(name, process, title.ToString(), type.ToString(), IsWindowVisible(window)));
                    return true;
                }, IntPtr.Zero))
                    notes.Add("EnumDesktopWindows on " + name + " failed with Win32 error " + Marshal.GetLastWin32Error());
                else
                    notes.Add(name + " yielded " + (windows.Count - before) + " top-level windows.");
            }

            if (desktops.Count == 0)
                Collect(IntPtr.Zero, "current");
            foreach (string name in desktops) {
                IntPtr desktop = OpenDesktop(name, 0, false, 0x0001); // DESKTOP_READOBJECTS
                if (desktop == IntPtr.Zero) {
                    notes.Add("OpenDesktop on " + name + " failed with Win32 error " + Marshal.GetLastWin32Error());
                    continue;
                }
                try { Collect(desktop, name); }
                finally { CloseDesktop(desktop); }
            }
            return windows;
        }
    }
}
'@
    }
    $byPid = @{}
    foreach ($item in $all) { $byPid[$item.ProcessId] = $item.Name }
    $windows = @([SetupTimeout.Windows]::List($notes) | Select-Object Desktop, ProcessId,
        @{Name='ProcessName'; Expression={$byPid[$_.ProcessId]}}, Title, Class, Visible)
    ConvertTo-Json -InputObject $windows -Depth 3 | Set-Content -LiteralPath "$prefix-windows.json"
    $notes.Add('Window enumeration is limited to accessible desktops on the collecting process window station and session.')
    if (!$windows.Count) { $notes.Add('No top-level windows could be enumerated.') }
} catch { $notes.Add("Window enumeration failed. $($_.Exception.Message)") }

$notes | Set-Content -LiteralPath "$prefix-notes.log"
$notes | ForEach-Object { Write-Host $_ }
$roots
