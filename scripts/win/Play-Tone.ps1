param([int]$Seconds = 30, [int]$Hz = 1000)
$ErrorActionPreference = 'Stop'
if ($Seconds -lt 1 -or $Seconds -gt 600 -or $Hz -lt 20 -or $Hz -gt 20000) { throw 'Invalid tone duration or frequency' }
Add-Type -TypeDefinition @'
using System;
using System.IO;
public static class EMTone {
    public static void Write(string path, int seconds, int hz) {
        int frames = 48000 * seconds, bytes = frames * 4;
        using (var w = new BinaryWriter(File.Create(path))) {
            w.Write(System.Text.Encoding.ASCII.GetBytes("RIFF")); w.Write(36+bytes);
            w.Write(System.Text.Encoding.ASCII.GetBytes("WAVEfmt ")); w.Write(16);
            w.Write((short)1); w.Write((short)2); w.Write(48000); w.Write(192000);
            w.Write((short)4); w.Write((short)16);
            w.Write(System.Text.Encoding.ASCII.GetBytes("data")); w.Write(bytes);
            for (int i=0; i<frames; i++) {
                short sample = (short)(32767 * 0.2511886 * Math.Sin(2*Math.PI*hz*i/48000));
                w.Write(sample); w.Write(sample);
            }
        }
    }
}
'@
$path = 'D:\AgentWork\em-v030\tone.wav'
[EMTone]::Write($path, $Seconds, $Hz)
$player = New-Object System.Media.SoundPlayer $path
try { $player.PlaySync() } finally { $player.Dispose() }
