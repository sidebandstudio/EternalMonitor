"""Drive the reference PC using SSH and the console-session PowerShell runner."""
import base64
import json
import os
import pathlib
import re
import subprocess
import sys

ROOT = r"D:\AgentWork\em-v030"
REPO = r"D:\AgentWork\Eternal-Monitor"
EVIDENCE = pathlib.Path(os.environ.get("EM_EVIDENCE_DIR", "/Users/aldo/Desktop/EternalMonitor-Handoff/evidence"))


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def ps(command):
    encoded = base64.b64encode(("$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; " + command).encode("utf-16-le")).decode()
    subprocess.run(["ssh", "-o", "BatchMode=yes", "windows",
                    "powershell -NoProfile -OutputFormat Text -ExecutionPolicy Bypass -EncodedCommand " + encoded], check=True)


def script(name, arguments=""):
    return "& " + quote(ROOT + "\\scripts\\" + name + ".ps1") + " " + arguments


def session(command, detach=False, idle=False, timeout=600):
    options = "-Command " + quote(command) + " -TimeoutSec " + str(timeout)
    if detach:
        options += " -Detach"
    if idle:
        options += " -RequireIdle"
    ps(script("Invoke-InSession", options))


def pull(remote, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["scp", "windows:" + remote.replace("\\", "/"), str(destination)], check=True)
    print(destination.resolve(), file=sys.stderr)


def main(args):
    if not args:
        raise ValueError("Usage: remote.sh idle|sync|build|run-host|stop-host|shot|log|probe-log|pattern|probe|tone|firewall")
    action, *args = args
    print("Windows: " + action + " " + " ".join(args), file=sys.stderr, flush=True)
    if action == "idle" and not args:
        session("Write-Output 'Console input-idle check passed'", idle=True, timeout=30)
    elif action == "sync" and len(args) == 1:
        ps("New-Item -ItemType Directory -Force " + quote(ROOT + r"\scripts") + " | Out-Null")
        sources = sorted(pathlib.Path(__file__).parent.glob("*.ps1"))
        subprocess.run(["scp", *map(str, sources), "windows:D:/AgentWork/em-v030/scripts/"], check=True)
        ps(script("Sync-Repo", "-Branch " + quote(args[0])))
    elif action == "build" and all(a == "--release" for a in args):
        ps(script("Build-Host", "-Test -Lint" + (" -Release" if args else "")))
    elif action == "run-host":
        environment = {"APPDATA": ROOT + r"\state", "ETERNAL_HEADLESS": "1", "ETERNAL_FPS": "60"}
        for arg in args:
            key, sep, value = arg.partition("=")
            if not sep or not re.fullmatch(r"ETERNAL_[A-Z0-9_]+", key):
                raise ValueError("Host arguments must be ETERNAL_NAME=value")
            environment[key] = value
        pidfile = ROOT + r"\host.pid.json"
        ps("if (Test-Path " + quote(pidfile) + ") { throw 'A host run is already tracked; stop it before starting another' }")
        command = "; ".join("$env:" + k + "=" + quote(v) for k, v in environment.items())
        bitrate = float(os.environ.get("EM_BITRATE_MBPS", "15"))
        if not 4 <= bitrate <= 50:
            raise ValueError("EM_BITRATE_MBPS must be between 4 and 50")
        settings_dir = ROOT + r"\state\EternalMonitor"
        # This isolated fixture profile is separate from the installed product.
        settings = json.dumps(dict(bitrate_mbps=bitrate, target_fps=60, start_on_boot=False,
                                   require_pairing=os.environ.get("EM_REQUIRE_PAIRING", "0") == "1",
                                   stream_audio=os.environ.get("EM_AUDIO", "0") == "1"))
        command += "; New-Item -ItemType Directory -Force " + quote(settings_dir) + " | Out-Null"
        command += "; [System.IO.File]::WriteAllText(" + quote(settings_dir + r"\settings.json")
        command += "," + quote(settings) + ",(New-Object System.Text.UTF8Encoding($false)))"
        command += "; $env:PATH=" + quote(r"D:\AgentWork\sdk\ffmpeg-7.1.1-full_build-shared\bin;") + "+$env:PATH"
        command += "; $p=Start-Process -PassThru -NoNewWindow -FilePath " + quote(REPO + r"\target\release\eternal-host.exe")
        command += " -ArgumentList '19876' -RedirectStandardOutput " + quote(ROOT + r"\host.log")
        command += " -RedirectStandardError " + quote(ROOT + r"\host.stderr.log")
        command += "; @{id=$p.Id;start=$p.StartTime.ToUniversalTime().Ticks.ToString();path=$p.Path} | ConvertTo-Json | Set-Content " + quote(pidfile)
        command += "; $p.WaitForExit(); if ($p.ExitCode -ne 0) { throw ('Host exited with ' + $p.ExitCode) }"
        session(command, detach=True, idle=True, timeout=7200)
    elif action == "rss" and not args:
        ps("$record=Get-Content " + quote(ROOT + r"\host.pid.json") + " -Raw | ConvertFrom-Json; "
           "$p=Get-Process -Id $record.id; "
           "if ($p.Path -ne $record.path -or $p.StartTime.ToUniversalTime().Ticks.ToString() -ne $record.start) { "
           "throw 'The tracked host process identity changed' }; "
           "@{pid=$p.Id;rss_kib=[math]::Ceiling($p.WorkingSet64/1024)} | ConvertTo-Json -Compress")
    elif action == "stop-host" and not args:
        ps("$file=" + quote(ROOT + r"\host.pid.json") + "; if (Test-Path $file) { "
           "$record=Get-Content $file -Raw | ConvertFrom-Json; "
           "$p=Get-Process -Id $record.id -ErrorAction SilentlyContinue; "
           "if ($p -and $p.Path -eq $record.path -and $p.StartTime.ToUniversalTime().Ticks.ToString() -eq $record.start) { "
           "taskkill /PID $p.Id /T /F | Out-Host; if ($LASTEXITCODE -ne 0) { throw 'Host stop failed' } }; "
           "Remove-Item $file }; "
           "Get-Process eternal-host -ErrorAction SilentlyContinue | Select-Object Id,SessionId,Path; "
           "Write-Host 'Tracked host stopped'")
    elif action == "shot" and len(args) == 1:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", args[0]):
            raise ValueError("Screenshot names use letters, digits, underscores and dashes")
        remote = ROOT + "\\shots\\" + args[0] + ".png"
        session(script("Take-Screenshot", "-Path " + quote(remote)))
        pull(remote, EVIDENCE / "windows" / (args[0] + ".png"))
    elif action == "log" and (not args or (len(args) == 2 and args[0] == "-n" and args[1].isdigit())):
        ps("Get-Content " + quote(ROOT + r"\host.log") + (" -Tail " + args[1] if args else ""))
    elif action == "probe-log" and not args:
        pull(ROOT + r"\input-probe.log", EVIDENCE / "windows" / "input-probe.log")
    elif action in ("pattern", "probe") and args in (["start"], ["stop"]):
        flag = ROOT + "\\" + action + ".stop"
        if args == ["stop"]:
            ps("New-Item -ItemType File -Force " + quote(flag) + " | Out-Null")
        else:
            session(script("Pattern-Window" if action == "pattern" else "Input-Probe", "-Seconds 7200"),
                    detach=True, idle=True, timeout=7260)
    elif action == "tone" and len(args) == 1 and args[0].isdigit():
        session(script("Play-Tone", "-Seconds " + args[0]), timeout=int(args[0]) + 30)
    elif action == "firewall" and len(args) in (1, 2):
        if len(args) == 2 and args[1] != "--remove":
            raise ValueError("Usage: remote.sh firewall <path> [--remove]")
        ps(script("Set-Firewall", "-Program " + quote(args[0]) + (" -Remove" if len(args) == 2 else "")))
    else:
        raise ValueError("Invalid arguments for " + action)


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except subprocess.CalledProcessError as error:
        print(f"Windows command failed with exit {error.returncode}", file=sys.stderr)
        sys.exit(1)
    except ValueError as error:
        print(error, file=sys.stderr)
        sys.exit(1)
