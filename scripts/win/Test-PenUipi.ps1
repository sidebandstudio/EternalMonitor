# The receiving side of a UIPI check: run this at a higher level (an
# interactive task with RunLevel Highest) and pen_probe's `uipi` mode
# without elevation. The probe window writes probe.json with its process
# and bounds, counts the pointer, mouse and key messages that reach it, and
# closes when done.flag appears in the evidence directory.
param(
    [Parameter(Mandatory=$true)][string]$EvidenceDirectory,
    [int]$TimeoutSec = 120,
    [int]$ScreenIndex = 0
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $EvidenceDirectory | Out-Null
Remove-Item (Join-Path $EvidenceDirectory '*') -Force -ErrorAction SilentlyContinue
Add-Type -AssemblyName System.Windows.Forms, System.Drawing, System.Web.Extensions
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing,System.Web.Extensions -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Web.Script.Serialization;
using System.Windows.Forms;
public sealed class EMPenUipi : Form {
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("kernel32.dll")] static extern uint SetThreadExecutionState(uint flags);
    readonly Timer timer = new Timer();
    readonly Stopwatch clock = Stopwatch.StartNew();
    readonly string directory;
    readonly int timeout;
    bool published;
    public int Pointer, MouseDowns, Keys;
    public EMPenUipi(string dir, int screen, int secs) {
        directory = dir; timeout = secs;
        Text = "EternalMonitor input probe";
        StartPosition = FormStartPosition.Manual;
        var area = Screen.AllScreens[screen].WorkingArea;
        Bounds = new Rectangle(area.X + 100, area.Y + 100, Math.Min(800, area.Width - 200), Math.Min(600, area.Height - 200));
        BackColor = Color.White;
        TopMost = true;
        KeyPreview = true;
        SetThreadExecutionState(0x80000003);
        Shown += delegate { Activate(); Focus(); SetForegroundWindow(Handle); };
        timer.Interval = 50;
        timer.Tick += delegate {
            if (!published && clock.ElapsedMilliseconds > 1000) {
                if (GetForegroundWindow() != Handle) {
                    Activate(); Focus(); SetForegroundWindow(Handle);
                } else {
                    Rectangle bounds = RectangleToScreen(ClientRectangle);
                    File.WriteAllText(Path.Combine(directory, "probe.json"), String.Format("{0} {1} {2} {3} {4}",
                        Process.GetCurrentProcess().Id, bounds.X, bounds.Y, bounds.Width, bounds.Height));
                    published = true;
                }
            }
            if (File.Exists(Path.Combine(directory, "done.flag")) || clock.Elapsed.TotalSeconds > timeout) {
                File.WriteAllText(Path.Combine(directory, "result.json"), new JavaScriptSerializer().Serialize(new {
                    published, pointer = Pointer, mouseDowns = MouseDowns, keys = Keys,
                    foregroundAtEnd = GetForegroundWindow() == Handle
                }));
                Close();
            }
        };
        timer.Start();
    }
    protected override void WndProc(ref Message message) {
        if (message.Msg >= 0x245 && message.Msg <= 0x247) Pointer++;
        if (message.Msg == 0x201) MouseDowns++;
        if (message.Msg == 0x100) Keys++;
        base.WndProc(ref message);
    }
    protected override void OnFormClosed(FormClosedEventArgs e) {
        timer.Stop();
        SetThreadExecutionState(0x80000000);
        base.OnFormClosed(e);
    }
    public static void Run(string dir, int screen, int secs) {
        SetProcessDpiAwarenessContext(new IntPtr(-4));
        using (var probe = new EMPenUipi(dir, screen, secs)) { Application.Run(probe); }
    }
}
'@
[EMPenUipi]::Run($EvidenceDirectory, $ScreenIndex, $TimeoutSec)
Get-Content (Join-Path $EvidenceDirectory 'result.json')
