param([int]$Seconds = 600, [switch]$FullScreen)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing, System.Web.Extensions
$source = @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Web.Script.Serialization;
using System.Windows.Forms;
public class EMInputProbe : Form {
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint map);
    [DllImport("kernel32.dll")] static extern uint SetThreadExecutionState(uint flags);
    [DllImport("winmm.dll")] static extern uint timeBeginPeriod(uint ms);
    [DllImport("winmm.dll")] static extern uint timeEndPeriod(uint ms);
    readonly StreamWriter log = new StreamWriter(@"D:\AgentWork\em-v030\input-probe.log", false);
    readonly JavaScriptSerializer json = new JavaScriptSerializer();
    readonly Timer timer = new Timer();
    readonly Stopwatch clock = Stopwatch.StartNew();
    long frame = -1;
    public EMInputProbe(int seconds, bool fullScreen) {
        Text = "EternalMonitor input probe";
        StartPosition = FormStartPosition.Manual;
        Bounds = new Rectangle(100,100,1720,880);
        if (fullScreen) { FormBorderStyle = FormBorderStyle.None; Bounds = Screen.PrimaryScreen.Bounds; }
        TopMost = true; KeyPreview = true; DoubleBuffered = true;
        BackColor = Color.FromArgb(32,36,42);
        ForeColor = Color.FromArgb(255,122,26);
        log.AutoFlush = true;
        MouseMove += delegate(object s, MouseEventArgs e) { Mouse("MouseMove", e); };
        MouseDown += delegate(object s, MouseEventArgs e) { Mouse("MouseDown", e); };
        MouseUp += delegate(object s, MouseEventArgs e) { Mouse("MouseUp", e); };
        MouseWheel += delegate(object s, MouseEventArgs e) { Mouse("MouseWheel", e); };
        KeyDown += delegate(object s, KeyEventArgs e) { Key("KeyDown", e); };
        KeyUp += delegate(object s, KeyEventArgs e) { Key("KeyUp", e); };
        KeyPress += delegate(object s, KeyPressEventArgs e) { Write("KeyPress", new Dictionary<string,object> { {"char",e.KeyChar.ToString()}, {"utf16",(int)e.KeyChar} }); };
        Shown += delegate {
            Activate(); Focus();
            Rectangle r = RectangleToScreen(ClientRectangle);
            Write("Ready", new Dictionary<string,object> { {"pid",Process.GetCurrentProcess().Id}, {"x",r.X}, {"y",r.Y}, {"width",r.Width}, {"height",r.Height} });
        };
        Deactivate += delegate { Write("Deactivated", new Dictionary<string,object>()); };
        timeBeginPeriod(1);
        if (SetThreadExecutionState(0x80000003) == 0) throw new InvalidOperationException("Could not keep the probe awake");
        timer.Interval = 4;
        timer.Tick += delegate {
            if (clock.Elapsed.TotalSeconds >= seconds || File.Exists(@"D:\AgentWork\em-v030\probe.stop")) { Close(); return; }
            long next = (long)(clock.Elapsed.TotalSeconds * 60);
            if (frame != next) { frame = next; Invalidate(); }
        };
        timer.Start();
    }
    void Write(string kind, Dictionary<string,object> fields) {
        fields["event"] = kind;
        fields["elapsed_ms"] = clock.ElapsedMilliseconds;
        log.WriteLine(json.Serialize(fields));
    }
    void Mouse(string kind, MouseEventArgs e) {
        Point p = PointToScreen(e.Location);
        Write(kind, new Dictionary<string,object> { {"x",p.X}, {"y",p.Y}, {"button",e.Button.ToString()}, {"delta",e.Delta} });
    }
    void Key(string kind, KeyEventArgs e) {
        Write(kind, new Dictionary<string,object> { {"key",e.KeyCode.ToString()}, {"keycode",(int)e.KeyCode}, {"scan",MapVirtualKey((uint)e.KeyCode,0)} });
        if (e.KeyCode == Keys.Escape) Close();
    }
    protected override bool IsInputKey(Keys keyData) { return true; }
    protected override void OnPaint(PaintEventArgs e) {
        base.OnPaint(e);
        int width = ClientSize.Width, height = ClientSize.Height;
        Color[] colors = { Color.FromArgb(210,40,50), Color.FromArgb(35,180,80),
                           Color.FromArgb(40,70,210), Color.FromArgb(180,180,180) };
        for (int i = 0; i < 4; i++) {
            using (var brush = new SolidBrush(colors[i]))
                e.Graphics.FillRectangle(brush, (i % 2) * width/2, (i / 2) * height/2, (width+1)/2, (height+1)/2);
        }
        using (var amber = new SolidBrush(ForeColor))
            e.Graphics.FillRectangle(amber, (int)(frame * 6 % Math.Max(width,1)), 60, Math.Max(width/40,2), height-60);
        using (var pen = new Pen(ForeColor, 3)) {
            int w = ClientSize.Width, h = ClientSize.Height;
            foreach (var p in new[] { new Point(w/2,h/2),new Point(20,20),new Point(w-20,20),new Point(20,h-20),new Point(w-20,h-20) }) {
                e.Graphics.DrawLine(pen,p.X-12,p.Y,p.X+12,p.Y);
                e.Graphics.DrawLine(pen,p.X,p.Y-12,p.X,p.Y+12);
            }
        }
        e.Graphics.DrawString("Input stays inside this window. Escape closes the probe.", Font, Brushes.White, 40,40);
    }
    protected override void OnFormClosed(FormClosedEventArgs e) {
        timer.Stop(); timer.Dispose(); timeEndPeriod(1); SetThreadExecutionState(0x80000000);
        Write("Closed", new Dictionary<string,object>());
        log.Dispose(); base.OnFormClosed(e);
    }
}
'@
Add-Type -TypeDefinition $source -ReferencedAssemblies System.Windows.Forms,System.Drawing,System.Web.Extensions
Remove-Item 'D:\AgentWork\em-v030\probe.stop' -ErrorAction SilentlyContinue
[Windows.Forms.Application]::EnableVisualStyles()
$window = New-Object EMInputProbe $Seconds,([bool]$FullScreen)
try { [Windows.Forms.Application]::Run($window) } finally { $window.Dispose() }
