# Saves the screens Windows shows before Setup's virtual display driver steps, and puts them back
# after those steps if the PC lost every one of them. The driver's setup starts the virtual display,
# and a hybrid laptop's panel went black then, with only Windows' 800x600 placeholder left. The rule
# is the app's (host/src/display_guard.rs): act only when every real screen from before is gone.
# Re-apply the saved layout; if that fails, "PC screen only", and only if the built-in panel was
# on. Then check again and log. One attempt. A PC that keeps any real screen sees no change.
#
#   display-guard.ps1 -Action save  -Path <file>   before the driver steps
#   display-guard.ps1 -Action check -Path <file>   after them, including the Disable
#
# It never fails or holds up Setup: every error is written to the output, which Setup logs, and the
# script exits 0. installer/tests/Test-DisplayGuard.ps1 dot-sources it to run the rule on fakes.
param([string]$Action, [string]$Path)

# How long a check waits for Windows to bring a real screen back itself, and for a restore to take.
$SettleMs = 3000
$SettleStepMs = 250

$NativeSource = @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace EternalMonitor {
    public class Screen {
        public string Adapter; public uint Target; public bool Internal; public bool Virtual; public string Mode;
    }
    public class Layout {
        public Screen[] Screens; public byte[] Paths; public byte[] Modes; public uint PathCount; public uint ModeCount;
    }
    public static class DisplayGuard {
        const uint QDC_ONLY_ACTIVE_PATHS = 2;
        const int ERROR_INSUFFICIENT_BUFFER = 122;
        const int PATH_SIZE = 72, MODE_SIZE = 64;
        public const uint SDC_TOPOLOGY_INTERNAL = 1, SDC_USE_SUPPLIED_DISPLAY_CONFIG = 0x20,
            SDC_VALIDATE = 0x40, SDC_APPLY = 0x80, SDC_ALLOW_CHANGES = 0x400;

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        public struct TargetName {
            public int Type; public uint Size; public uint AdapterLow; public int AdapterHigh; public uint Id;
            public uint Flags; public int Technology; public ushort Manufacturer; public ushort Product;
            public uint Connector;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string FriendlyName;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DevicePath;
        }

        [DllImport("user32.dll")]
        static extern int GetDisplayConfigBufferSizes(uint flags, out uint paths, out uint modes);
        [DllImport("user32.dll")]
        static extern int QueryDisplayConfig(uint flags, ref uint paths, [Out] byte[] pathArray,
            ref uint modes, [Out] byte[] modeArray, IntPtr topology);
        [DllImport("user32.dll")]
        static extern int SetDisplayConfig(uint paths, byte[] pathArray, uint modes, byte[] modeArray, uint flags);
        [DllImport("user32.dll")]
        static extern int DisplayConfigGetDeviceInfo(ref TargetName name);

        // The active paths and modes, or null when Windows can't say. Read again if they changed
        // between sizing and reading, as Microsoft documents during a topology change.
        public static Layout Read() {
            for (int attempt = 0; attempt < 4; attempt++) {
                uint paths, modes;
                if (GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, out paths, out modes) != 0) return null;
                byte[] pathArray = new byte[paths * PATH_SIZE], modeArray = new byte[modes * MODE_SIZE];
                int status = QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, ref paths, pathArray, ref modes, modeArray, IntPtr.Zero);
                if (status == ERROR_INSUFFICIENT_BUFFER) continue;
                if (status != 0) return null;
                Array.Resize(ref pathArray, (int)(paths * PATH_SIZE));
                Array.Resize(ref modeArray, (int)(modes * MODE_SIZE));
                var screens = new List<Screen>();
                for (int i = 0; i < paths; i++) screens.Add(Describe(pathArray, i * PATH_SIZE, modeArray, modes));
                return new Layout { Screens = screens.ToArray(), Paths = pathArray, Modes = modeArray,
                    PathCount = paths, ModeCount = modes };
            }
            return null;
        }

        // DISPLAYCONFIG_PATH_INFO: the source's mode index at 12, the target's adapter at 20, id at
        // 28, technology at 36 and refresh rate at 48. A source mode's size is at 16 in its entry.
        static Screen Describe(byte[] paths, int at, byte[] modes, uint modeCount) {
            uint low = BitConverter.ToUInt32(paths, at + 20);
            int high = BitConverter.ToInt32(paths, at + 24);
            uint id = BitConverter.ToUInt32(paths, at + 28);
            int technology = BitConverter.ToInt32(paths, at + 36);
            uint numerator = BitConverter.ToUInt32(paths, at + 48), denominator = BitConverter.ToUInt32(paths, at + 52);
            uint index = BitConverter.ToUInt32(paths, at + 12);
            string mode = null;
            if (index < modeCount && BitConverter.ToInt32(modes, (int)index * MODE_SIZE) == 1) {
                int entry = (int)index * MODE_SIZE;
                double hz = denominator == 0 ? 0 : Math.Round((double)numerator / denominator, 2);
                mode = String.Format(System.Globalization.CultureInfo.InvariantCulture, "{0}x{1}@{2}Hz",
                    BitConverter.ToUInt32(modes, entry + 16), BitConverter.ToUInt32(modes, entry + 20), hz);
            }
            var name = new TargetName();
            name.Type = 2; // DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME
            name.Size = (uint)Marshal.SizeOf(typeof(TargetName));
            name.AdapterLow = low; name.AdapterHigh = high; name.Id = id;
            string device = DisplayConfigGetDeviceInfo(ref name) == 0 ? name.DevicePath ?? "" : "";
            return new Screen {
                Adapter = (((ulong)(uint)high << 32) | low).ToString("x"),
                Target = id,
                // Internal, LVDS, embedded DisplayPort and embedded UDI.
                Internal = technology == unchecked((int)0x80000000) || technology == 6 || technology == 11 || technology == 13,
                // The virtual display's monitor reports the driver's EDID ID in its path.
                Virtual = device.ToUpperInvariant().Contains("MTT1337"),
                Mode = mode,
            };
        }

        // Check, then apply, the saved paths and modes. 0, or the failing call's code.
        public static int Reapply(byte[] paths, uint pathCount, byte[] modes, uint modeCount) {
            uint supplied = SDC_USE_SUPPLIED_DISPLAY_CONFIG;
            int status = SetDisplayConfig(pathCount, paths, modeCount, modes, supplied | SDC_VALIDATE);
            if (status != 0) return status;
            return SetDisplayConfig(pathCount, paths, modeCount, modes, supplied | SDC_APPLY | SDC_ALLOW_CHANGES);
        }

        // What Windows+P "PC screen only" does.
        public static int InternalOnly() {
            return SetDisplayConfig(0, null, 0, null, SDC_APPLY | SDC_TOPOLOGY_INTERNAL);
        }
    }
}
'@

function Use-Native {
    # A compiler warning must not cost the check.
    if (!('EternalMonitor.DisplayGuard' -as [type])) { Add-Type -TypeDefinition $NativeSource -IgnoreWarnings }
}

# The Windows calls. Test-DisplayGuard.ps1 replaces these four.
function Read-Screens { Use-Native; [EternalMonitor.DisplayGuard]::Read() }
function Set-SavedLayout($Saved) {
    Use-Native
    [EternalMonitor.DisplayGuard]::Reapply($Saved.Paths, $Saved.PathCount, $Saved.Modes, $Saved.ModeCount)
}
function Set-PcScreenOnly { Use-Native; [EternalMonitor.DisplayGuard]::InternalOnly() }
function Wait-Display([int]$Milliseconds) { Start-Sleep -Milliseconds $Milliseconds }

function Get-RealScreens($Screens) { @($Screens | Where-Object { $_ -and !$_.Virtual }) }

# The screens as one log value, as the app writes it: the count of real screens, each one's kind,
# target and mode, and the count of virtual ones.
function Format-Screens($Layout) {
    if ($null -eq $Layout) { return 'paths=unknown' }
    $real = Get-RealScreens $Layout.Screens
    $shown = @($real | Select-Object -First 8 | ForEach-Object {
        $kind = if ($_.Internal) { 'internal' } else { 'external' }
        $mode = if ($_.Mode) { " $($_.Mode)" } else { '' }
        "$kind tgt=$($_.Adapter)/$($_.Target)$mode"
    })
    $more = if ($real.Count -gt 8) { " | +$($real.Count - 8) more" } else { '' }
    $vdd = @($Layout.Screens | Where-Object { $_ -and $_.Virtual }).Count
    "paths=$($real.Count) [$($shown -join ' | ')$more] vdd=$vdd"
}

# leave, restore, or restore-internal (then "PC screen only" may follow).
function Get-RestorePlan($Before, $After) {
    $had = Get-RealScreens $Before
    if ($had.Count -eq 0 -or (Get-RealScreens $After).Count -gt 0) { return 'leave' }
    if (@($had | Where-Object { $_.Internal }).Count -gt 0) { 'restore-internal' } else { 'restore' }
}

# The screens once a real one is back, or as they are after $SettleMs.
function Wait-Screens($Before) {
    $waited = 0
    while ($true) {
        $layout = Read-Screens
        if ($null -eq $layout -or (Get-RestorePlan $Before $layout.Screens) -eq 'leave' -or $waited -ge $SettleMs) {
            return $layout
        }
        Wait-Display $SettleStepMs
        $waited += $SettleStepMs
    }
}

function ConvertTo-Saved($Layout) {
    [pscustomobject]@{
        Screens = @($Layout.Screens | ForEach-Object {
            [pscustomobject]@{Adapter=$_.Adapter; Target=$_.Target; Internal=$_.Internal; Virtual=$_.Virtual; Mode=$_.Mode}
        })
        Paths = [Convert]::ToBase64String([byte[]]$Layout.Paths)
        Modes = [Convert]::ToBase64String([byte[]]$Layout.Modes)
        PathCount = $Layout.PathCount
        ModeCount = $Layout.ModeCount
    } | ConvertTo-Json -Depth 4 -Compress
}

function ConvertFrom-Saved([string]$Text) {
    $saved = $Text | ConvertFrom-Json
    [pscustomobject]@{
        Screens = @($saved.Screens)
        Paths = [Convert]::FromBase64String($saved.Paths)
        Modes = [Convert]::FromBase64String($saved.Modes)
        PathCount = [uint32]$saved.PathCount
        ModeCount = [uint32]$saved.ModeCount
    }
}

function Invoke-Save([string]$Path) {
    $layout = Read-Screens
    Write-Output "Display guard: screens before the display driver steps: $(Format-Screens $layout)"
    if ($null -ne $layout) { [IO.File]::WriteAllText($Path, (ConvertTo-Saved $layout)) }
}

function Invoke-Check([string]$Path) {
    if (!(Test-Path -LiteralPath $Path)) {
        $after = Read-Screens
        Write-Output "Display guard: screens after the display driver steps: $(Format-Screens $after); nothing saved to compare with"
        return
    }
    $saved = ConvertFrom-Saved ([IO.File]::ReadAllText($Path))
    # One attempt per save: a second check finds nothing to compare with.
    Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    $after = Wait-Screens $saved.Screens
    Write-Output "Display guard: screens after the display driver steps: $(Format-Screens $after)"
    if ($null -eq $after) { return }
    $plan = Get-RestorePlan $saved.Screens $after.Screens
    if ($plan -eq 'leave') { return }
    Write-Output "Display guard: the PC lost every screen it had before the display driver steps; restoring them. Before: $(Format-Screens $saved)"
    $tried = @()
    $now = $after
    $code = Set-SavedLayout $saved
    if ($code -eq 0) {
        $now = Wait-Screens $saved.Screens
        if ((Get-RealScreens $now.Screens).Count -eq 0) { $tried += 'reason=saved_layout_no_screen' }
    } else {
        $tried += "reason=saved_layout_refused code=$code"
    }
    $applied = 'saved_layout'
    if ((Get-RealScreens $now.Screens).Count -eq 0 -and $plan -eq 'restore-internal') {
        $applied = 'internal'
        $code = Set-PcScreenOnly
        if ($code -eq 0) {
            $now = Wait-Screens $saved.Screens
            if ((Get-RealScreens $now.Screens).Count -eq 0) { $tried += 'reason=internal_no_screen' }
        } else {
            $tried += "reason=internal_refused code=$code"
        }
    }
    if ((Get-RealScreens $now.Screens).Count -gt 0) {
        Write-Output "Display guard: screens restored outcome=restored applied=$applied $(Format-Screens $now)"
    } else {
        $reasons = if ($tried) { ($tried -join ' ') + ' ' } else { '' }
        Write-Output "Display guard: could not restore the screens outcome=restore_failed $reasons$(Format-Screens $now)"
    }
}

function Invoke-DisplayGuard([string]$Action, [string]$Path) {
    $ErrorActionPreference = 'Stop'
    try {
        if (!$Path) { throw 'no -Path given' }
        switch ($Action) {
            'save' { Invoke-Save $Path }
            'check' { Invoke-Check $Path }
            default { throw "unknown -Action '$Action'" }
        }
    } catch {
        Write-Output "Display guard skipped: $($_.Exception.Message)"
    }
}

# Dot-sourced without an action, it only defines the functions above.
if ($Action -or $Path) {
    Invoke-DisplayGuard $Action $Path
    exit 0
}
