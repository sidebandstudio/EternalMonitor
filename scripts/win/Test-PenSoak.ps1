# Long native pen soak: pen_probe's `soak` mode draws strokes into this
# focused probe window and logs every frame Windows refuses. The probe counts
# the pointer messages it receives, so restarted contacts show as extra downs.
# -BusyMs blocks the probe's message loop that long every three seconds, as a
# drawing app does while it renders a heavy brush stroke.
param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$EvidenceDirectory,
    [int]$Seconds = 600,
    [ValidateSet('paced','batch','burst','same','load')][string]$Pattern = 'batch',
    [switch]$Stamp,
    # More pen_probe soak options, such as 'spacing_us=500 recover=retry'.
    [string]$Options = '',
    [int]$BusyMs = 0,
    [int]$ScreenIndex = 0
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $EvidenceDirectory | Out-Null
Add-Type -AssemblyName System.Windows.Forms, System.Drawing, System.Web.Extensions
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing,System.Web.Extensions -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Web.Script.Serialization;
using System.Windows.Forms;
public sealed class EMPenSoak : Form {
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("kernel32.dll")] static extern uint SetThreadExecutionState(uint flags);
    [StructLayout(LayoutKind.Sequential)] struct PointerInfo {
        public uint type, id, frame, flags;
        public IntPtr source, target;
        public Point pixel, himetric, rawPixel, rawHimetric;
        public uint time, history;
        public int input;
        public uint keys;
        public ulong performance;
        public uint change;
    }
    [DllImport("user32.dll")] static extern bool GetPointerInfo(uint id, out PointerInfo info);
    readonly Timer timer = new Timer();
    readonly Stopwatch clock = Stopwatch.StartNew();
    readonly string executable, directory, arguments;
    readonly int seconds, busyMs;
    readonly System.Text.StringBuilder childOutput = new System.Text.StringBuilder();
    Process child;
    Rectangle bounds;
    long nextBusy = 3000;
    // Messages by kind, and the frames they carry: Windows coalesces updates
    // a slow window has not read, keeping them in each message's history.
    public long Downs, Ups, Updates, Frames, Cancels, MouseDowns, Busy, InfoFailures;
    public bool Finished;
    public EMPenSoak(string exe, string dir, int screen, int secs, string pattern, bool stamp, string options, int busy) {
        executable = exe; directory = dir; seconds = secs; busyMs = busy;
        arguments = secs + " " + pattern + (stamp ? " stamp" : "") + (options.Length > 0 ? " " + options : "");
        Text = "EternalMonitor input probe";
        StartPosition = FormStartPosition.Manual;
        var area = Screen.AllScreens[screen].WorkingArea;
        Bounds = new Rectangle(area.X + 100, area.Y + 100, Math.Min(800, area.Width - 200), Math.Min(600, area.Height - 200));
        BackColor = Color.White;
        TopMost = true;
        SetThreadExecutionState(0x80000003);
        Shown += delegate { Activate(); Focus(); SetForegroundWindow(Handle); bounds = RectangleToScreen(ClientRectangle); };
        timer.Interval = 50;
        timer.Tick += delegate {
            if (child == null) {
                if (clock.ElapsedMilliseconds < 1000) return;
                if (GetForegroundWindow() != Handle) {
                    Activate(); Focus(); SetForegroundWindow(Handle);
                    if (clock.Elapsed.TotalSeconds > 20) Close();
                    return;
                }
                child = new Process();
                child.StartInfo = new ProcessStartInfo(executable,
                    String.Format("soak {0} {1} {2} {3} {4} {5}", Process.GetCurrentProcess().Id, bounds.X, bounds.Y, bounds.Width, bounds.Height, arguments));
                child.StartInfo.UseShellExecute = false;
                child.StartInfo.CreateNoWindow = true;
                child.StartInfo.RedirectStandardOutput = true;
                child.StartInfo.RedirectStandardError = true;
                DataReceivedEventHandler keep = delegate(object sender, DataReceivedEventArgs args) {
                    if (args.Data != null) lock (childOutput) childOutput.AppendLine(args.Data);
                };
                child.OutputDataReceived += keep;
                child.ErrorDataReceived += keep;
                child.Start();
                child.BeginOutputReadLine();
                child.BeginErrorReadLine();
                return;
            }
            if (busyMs > 0 && clock.ElapsedMilliseconds >= nextBusy) {
                nextBusy = clock.ElapsedMilliseconds + 3000;
                Busy++;
                System.Threading.Thread.Sleep(busyMs);
            }
            if (child.HasExited) {
                child.WaitForExit();
                string output;
                lock (childOutput) output = childOutput.ToString();
                File.WriteAllText(Path.Combine(directory, "injector.log"), output);
                Finished = child.ExitCode == 0;
                File.WriteAllText(Path.Combine(directory, "result.json"), new JavaScriptSerializer().Serialize(new {
                    finished = Finished, exitCode = child.ExitCode, downs = Downs, ups = Ups, updates = Updates,
                    frames = Frames, cancels = Cancels, mouseDowns = MouseDowns, infoFailures = InfoFailures, busy = Busy, busyMs = busyMs,
                    x = bounds.X, y = bounds.Y, width = bounds.Width, height = bounds.Height
                }));
                Close();
            }
            if (clock.Elapsed.TotalSeconds > seconds + 60) Close();
        };
        timer.Start();
    }
    protected override void WndProc(ref Message message) {
        if (message.Msg == 0x201) MouseDowns++;
        if (message.Msg >= 0x245 && message.Msg <= 0x247) {
            if (message.Msg == 0x245) Updates++;
            if (message.Msg == 0x246) Downs++;
            if (message.Msg == 0x247) Ups++;
            PointerInfo info;
            if (GetPointerInfo((uint)(message.WParam.ToInt64() & 0xffff), out info)) {
                Frames += Math.Max(1u, info.history);
                if ((info.flags & 0x8000) != 0) Cancels++;
            } else {
                InfoFailures++;
            }
            message.Result = IntPtr.Zero;
            return;
        }
        base.WndProc(ref message);
    }
    protected override void OnFormClosed(FormClosedEventArgs e) {
        timer.Stop();
        if (child != null) { if (!child.HasExited) child.Kill(); child.Dispose(); }
        SetThreadExecutionState(0x80000000);
        base.OnFormClosed(e);
    }
    public static bool Run(string exe, string dir, int screen, int secs, string pattern, bool stamp, string options, int busy) {
        SetProcessDpiAwarenessContext(new IntPtr(-4));
        using (var probe = new EMPenSoak(exe, dir, screen, secs, pattern, stamp, options, busy)) { Application.Run(probe); return probe.Finished; }
    }
}
'@
if (![EMPenSoak]::Run($Executable, $EvidenceDirectory, $ScreenIndex, $Seconds, $Pattern, $Stamp.IsPresent, $Options, $BusyMs)) {
    throw "Pen soak did not finish. See $EvidenceDirectory"
}
Get-Content (Join-Path $EvidenceDirectory 'result.json')
Select-String -Path (Join-Path $EvidenceDirectory 'injector.log') -Pattern 'SOAK summary' | Select-Object -Last 1 | ForEach-Object { $_.Line }
