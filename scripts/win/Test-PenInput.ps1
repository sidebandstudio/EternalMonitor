param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$EvidenceDirectory,
    [int]$ScreenIndex = 0
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $EvidenceDirectory | Out-Null
Add-Type -AssemblyName System.Windows.Forms, System.Drawing, System.Web.Extensions
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing,System.Web.Extensions -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Web.Script.Serialization;
using System.Windows.Forms;
public sealed class EMPenProbe : Form {
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
    [StructLayout(LayoutKind.Sequential)] struct PenInfo {
        public PointerInfo pointer;
        public uint flags, mask, pressure, rotation;
        public int tiltX, tiltY;
    }
    [DllImport("user32.dll")] static extern bool GetPointerPenInfo(uint id, out PenInfo info);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern uint SendInput(uint count, Input[] inputs, int size);
    [StructLayout(LayoutKind.Sequential)] struct MouseInput { public int x, y; public uint data, flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct Input { public uint type; public MouseInput mouse; }
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("kernel32.dll")] static extern uint SetThreadExecutionState(uint flags);
    readonly List<PenInfo> samples = new List<PenInfo>();
    readonly List<long> sampleTimes = new List<long>();
    readonly Timer timer = new Timer();
    readonly Stopwatch clock = Stopwatch.StartNew();
    readonly string executable, directory;
    readonly StreamWriter log;
    readonly JavaScriptSerializer json = new JavaScriptSerializer();
    Process child;
    bool focusClickSent;
    readonly System.Text.StringBuilder childOutput = new System.Text.StringBuilder();
    Rectangle bounds;
    public bool Passed;
    public EMPenProbe(string exe, string dir, int screen) {
        executable = exe; directory = dir;
        log = new StreamWriter(Path.Combine(dir, "pen-events.jsonl"), false);
        log.AutoFlush = true;
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
            if (child == null && clock.ElapsedMilliseconds > 1000) {
                if (GetForegroundWindow() != Handle) {
                    Activate(); Focus(); SetForegroundWindow(Handle);
                    // Windows can refuse programmatic foreground activation.
                    // A fixture click is allowed only on our own visible form.
                    Point center = PointToScreen(new Point(ClientSize.Width / 2, ClientSize.Height / 2));
                    if (!focusClickSent && WindowFromPoint(center) == Handle) {
                        Cursor.Position = center;
                        if (WindowFromPoint(Cursor.Position) == Handle) {
                            focusClickSent = true;
                            Input[] click = { new Input { mouse = new MouseInput { flags = 2 } }, new Input { mouse = new MouseInput { flags = 4 } } };
                            SendInput(2, click, Marshal.SizeOf(typeof(Input)));
                        }
                    }
                    if (clock.Elapsed.TotalSeconds > 20) Close();
                    return;
                }
                child = new Process();
                child.StartInfo = new ProcessStartInfo(executable,
                    String.Format("{0} {1} {2} {3} {4}", Process.GetCurrentProcess().Id, bounds.X, bounds.Y, bounds.Width, bounds.Height));
                child.StartInfo.UseShellExecute = false;
                child.StartInfo.CreateNoWindow = true;
                child.StartInfo.RedirectStandardOutput = true;
                child.StartInfo.RedirectStandardError = true;
                child.OutputDataReceived += delegate(object sender, DataReceivedEventArgs args) {
                    if (args.Data != null) lock (childOutput) childOutput.AppendLine(args.Data);
                };
                child.ErrorDataReceived += delegate(object sender, DataReceivedEventArgs args) {
                    if (args.Data != null) lock (childOutput) childOutput.AppendLine(args.Data);
                };
                child.Start();
                child.BeginOutputReadLine();
                child.BeginErrorReadLine();
            }
            if (child != null && child.HasExited && clock.ElapsedMilliseconds > 2500) {
                child.WaitForExit();
                string output;
                lock (childOutput) output = childOutput.ToString();
                File.WriteAllText(Path.Combine(directory, "injector.log"), output);
                bool low = false, high = false, left = false, right = false, cancelFlagReported = false;
                bool positions = true;
                int downs = 0, ups = 0;
                long heldFrom = 0, heldUntil = 0;
                for (int i = 0; i < samples.Count; i++) {
                    var sample = samples[i];
                    low |= sample.pressure > 0 && sample.pressure <= 103;
                    high |= sample.pressure == 1024;
                    left |= sample.tiltX == -40 && sample.tiltY == 30;
                    right |= sample.tiltX == 40 && sample.tiltY == 30;
                    if ((sample.pointer.flags & 0x10000) != 0) {
                        downs++;
                        if (downs == 2) heldFrom = sampleTimes[i];
                    }
                    if ((sample.pointer.flags & 0x40000) != 0) {
                        ups++;
                        if (downs == 2) heldUntil = sampleTimes[i];
                    }
                    cancelFlagReported |= (sample.pointer.flags & 0x48000) == 0x48000;
                    positions &= bounds.Contains(sample.pointer.pixel);
                    if (sample.pressure > 0) {
                        int step = (int)Math.Round(sample.pressure / 102.4);
                        int expectedX = bounds.X + (10000 + step * 3000) * (bounds.Width - 1) / 65535;
                        int expectedY = bounds.Y + 32768 * (bounds.Height - 1) / 65535;
                        positions &= sample.pointer.pixel.X == expectedX && sample.pointer.pixel.Y == expectedY;
                    }
                }
                // Windows may normalize CANCELED to an ordinary POINTERUP.
                // Require the observable result: exactly two strokes, a held
                // second contact, and a zero-pressure release on relay.reset().
                bool releasedOnReset = downs == 2 && ups == 2 && samples.Count > 0
                    && (samples[samples.Count - 1].pointer.flags & 0x40004) == 0x40000
                    && samples[samples.Count - 1].pressure == 0;
                bool held = heldFrom > 0 && heldUntil - heldFrom >= 1450;
                Passed = child.ExitCode == 0 && samples.Count >= 10 && low && high && left && right && releasedOnReset && held && positions
                    && !output.Contains("rejected") && !output.Contains("unavailable");
                File.WriteAllText(Path.Combine(directory, "result.json"), json.Serialize(new {
                    passed = Passed, count = samples.Count, low, high, left, right, downs, ups, releasedOnReset, held, cancelFlagReported, positions,
                    x = bounds.X, y = bounds.Y, width = bounds.Width, height = bounds.Height
                }));
                Close();
            }
            if (clock.Elapsed.TotalSeconds > 20) { Close(); }
        };
        timer.Start();
    }
    protected override void WndProc(ref Message message) {
        if (message.Msg >= 0x245 && message.Msg <= 0x247) {
            PenInfo info;
            if (GetPointerPenInfo((uint)(message.WParam.ToInt64() & 0xffff), out info)) {
                samples.Add(info);
                sampleTimes.Add(clock.ElapsedMilliseconds);
                log.WriteLine(json.Serialize(new { message = message.Msg, type = info.pointer.type,
                    elapsedMs = clock.ElapsedMilliseconds,
                    flags = info.pointer.flags, pressure = info.pressure, tiltX = info.tiltX, tiltY = info.tiltY,
                    x = info.pointer.pixel.X, y = info.pointer.pixel.Y }));
                message.Result = IntPtr.Zero;
                return;
            }
        }
        base.WndProc(ref message);
    }
    protected override void OnFormClosed(FormClosedEventArgs e) {
        timer.Stop(); log.Dispose();
        if (child != null) { if (!child.HasExited) child.Kill(); child.Dispose(); }
        SetThreadExecutionState(0x80000000);
        base.OnFormClosed(e);
    }
    public static bool Run(string exe, string dir, int screen) {
        SetProcessDpiAwarenessContext(new IntPtr(-4));
        using (var probe = new EMPenProbe(exe, dir, screen)) { Application.Run(probe); return probe.Passed; }
    }
}
'@
if (![EMPenProbe]::Run($Executable, $EvidenceDirectory, $ScreenIndex)) {
    throw "Native pen verification failed. See $EvidenceDirectory"
}
Get-Content (Join-Path $EvidenceDirectory 'result.json')
