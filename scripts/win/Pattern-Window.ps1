param([int]$Seconds = 600, [switch]$VirtualDisplay, [string]$DisplayName = '')
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$source = @'
using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;
public class EMTestPattern : Form {
    [DllImport("user32.dll", SetLastError=true)]
    public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("winmm.dll")] static extern uint timeBeginPeriod(uint ms);
    [DllImport("winmm.dll")] static extern uint timeEndPeriod(uint ms);
    [DllImport("kernel32.dll")] static extern uint SetThreadExecutionState(uint flags);
    readonly Timer timer = new Timer();
    readonly Stopwatch clock = Stopwatch.StartNew();
    readonly int seconds;
    readonly bool virtualDisplay;
    long frame = -1;
    public EMTestPattern(int seconds, bool virtualDisplay, string displayName) {
        this.seconds = seconds;
        this.virtualDisplay = virtualDisplay;
        Text = "EternalMonitor test pattern";
        FormBorderStyle = FormBorderStyle.None;
        StartPosition = FormStartPosition.Manual;
        Bounds = Screen.PrimaryScreen.Bounds;
        if (!String.IsNullOrEmpty(displayName)) {
            bool found = false;
            foreach (var screen in Screen.AllScreens) {
                if (String.Equals(screen.DeviceName, displayName, StringComparison.OrdinalIgnoreCase)) {
                    Bounds = screen.Bounds;
                    found = true;
                    break;
                }
            }
            if (!found) throw new InvalidOperationException("Test display is absent: " + displayName);
        }
        TopMost = true;
        DoubleBuffered = true;
        KeyPreview = true;
        KeyDown += delegate(object s, KeyEventArgs e) { if (e.KeyCode == Keys.Escape) Close(); };
        timeBeginPeriod(1);
        if (SetThreadExecutionState(0x80000003) == 0)
            throw new InvalidOperationException("Could not keep the test display awake");
        timer.Interval = 4;
        timer.Tick += delegate {
            if (clock.Elapsed.TotalSeconds >= seconds || File.Exists(@"D:\AgentWork\em-v030\pattern.stop")) { Close(); return; }
            if (this.virtualDisplay) {
                Rectangle target = Screen.PrimaryScreen.Bounds;
                foreach (var screen in Screen.AllScreens) {
                    if (!screen.Primary && screen.Bounds.Width == 2420 && screen.Bounds.Height == 1668) {
                        target = screen.Bounds;
                        break;
                    }
                }
                if (Bounds != target) Bounds = target;
            }
            long next = (long)(clock.Elapsed.TotalSeconds * 60);
            if (next != frame) { frame = next; Invalidate(); }
        };
        timer.Start();
    }
    protected override void OnPaint(PaintEventArgs e) {
        int w = ClientSize.Width, h = ClientSize.Height;
        Color[] colors = { Color.FromArgb(210,40,50), Color.FromArgb(35,180,80),
                           Color.FromArgb(40,70,210), Color.FromArgb(180,180,180) };
        for (int i = 0; i < 4; i++) {
            using (var brush = new SolidBrush(colors[i]))
                e.Graphics.FillRectangle(brush, (i % 2) * w/2, (i / 2) * h/2, (w+1)/2, (h+1)/2);
        }
        using (var amber = new SolidBrush(Color.FromArgb(255,122,26)))
            e.Graphics.FillRectangle(amber, (int)(frame * 6 % Math.Max(w,1)), 24, Math.Max(w/40,2), h-24);
        e.Graphics.FillRectangle(Brushes.Black, 0, 0, w, 24);
        for (int bit = 0; bit < 24; bit++)
            if ((frame & (1L << bit)) != 0)
                e.Graphics.FillRectangle(Brushes.White, 4+bit*20, 4, 16, 16);
    }
    protected override void OnFormClosed(FormClosedEventArgs e) {
        timer.Stop(); timer.Dispose(); timeEndPeriod(1);
        SetThreadExecutionState(0x80000000);
        base.OnFormClosed(e);
    }
}
'@
Add-Type -TypeDefinition $source -ReferencedAssemblies System.Windows.Forms,System.Drawing
Remove-Item 'D:\AgentWork\em-v030\pattern.stop' -ErrorAction SilentlyContinue
# Match capture dimensions in physical pixels, including secondary screens
# with a different scale from the primary monitor. PowerShell is system-aware.
if ([EMTestPattern]::SetThreadDpiAwarenessContext([IntPtr](-4)) -eq [IntPtr]::Zero) {
    throw "Could not enable per-monitor DPI awareness for the test pattern"
}
[Windows.Forms.Application]::EnableVisualStyles()
$window = New-Object EMTestPattern $Seconds,([bool]$VirtualDisplay),$DisplayName
try { [Windows.Forms.Application]::Run($window) } finally { $window.Dispose() }
