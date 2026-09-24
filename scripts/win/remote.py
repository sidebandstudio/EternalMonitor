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
SSH_OPTIONS = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
               "-o", "ServerAliveInterval=10", "-o", "ServerAliveCountMax=3"]


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def ps(command):
    encoded = base64.b64encode(("$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; "
                               "$OutputEncoding=[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); " + command).encode("utf-16-le")).decode()
    subprocess.run(["ssh", *SSH_OPTIONS, "windows",
                    "powershell -NoProfile -OutputFormat Text -ExecutionPolicy Bypass -EncodedCommand " + encoded], check=True)


def script(name, arguments=""):
    return "& " + quote(ROOT + "\\scripts\\" + name + ".ps1") + " " + arguments


def session(command, detach=False, idle=False, timeout=600, run_level="Highest"):
    if run_level not in ("Highest", "Limited"):
        raise ValueError("Run level must be Highest or Limited")
    options = "-Command " + quote(command) + " -TimeoutSec " + str(timeout)
    options += " -RunLevel " + run_level
    if detach:
        options += " -Detach"
    # Set only for a test window the user explicitly says is free. Keep the
    # idle guard for unattended runs and never persist this on the PC.
    if idle and os.environ.get("EM_PC_AVAILABLE") != "1":
        options += " -RequireIdle"
    ps(script("Invoke-InSession", options))


def pull(remote, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["scp", *SSH_OPTIONS, "windows:" + remote.replace("\\", "/"), str(destination)], check=True)
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
        subprocess.run(["scp", *SSH_OPTIONS, *map(str, sources), "windows:D:/AgentWork/em-v030/scripts/"], check=True)
        ps(script("Sync-Repo", "-Branch " + quote(args[0])))
    elif action == "build" and all(a == "--release" for a in args):
        ps(script("Build-Host", "-Test -Lint" + (" -Release" if args else "")))
    elif action == "run-host":
        environment = {"APPDATA": ROOT + r"\state", "ETERNAL_HEADLESS": "1", "ETERNAL_FPS": "60"}
        for arg in args:
            key, sep, value = arg.partition("=")
            if not sep or not (re.fullmatch(r"ETERNAL_[A-Z0-9_]+", key) or key == "RUST_LOG"):
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
        settings = json.dumps(dict(bitrate_mbps=bitrate, target_fps=int(environment["ETERNAL_FPS"]), start_on_boot=False,
                                   require_pairing=os.environ.get("EM_REQUIRE_PAIRING", "0") == "1",
                                   stream_audio=os.environ.get("EM_AUDIO", "0") == "1",
                                   capture_display=os.environ.get("EM_CAPTURE_DISPLAY") or None))
        command += "; New-Item -ItemType Directory -Force " + quote(settings_dir) + " | Out-Null"
        command += "; [System.IO.File]::WriteAllText(" + quote(settings_dir + r"\settings.json")
        command += "," + quote(settings) + ",(New-Object System.Text.UTF8Encoding($false)))"
        command += "; $env:PATH=" + quote(r"D:\AgentWork\sdk\ffmpeg-7.1.1-full_build-shared\bin;") + "+$env:PATH"
        executable = REPO + r"\target\release\eternal-host.exe"
        if os.environ.get("EM_INSTALLED_HOST") == "1":
            executable = os.environ.get("EM_INSTALLED_HOST_PATH", ROOT + r"\installed\EternalMonitor-host.exe")
        window_option = "-WindowStyle Hidden" if environment["ETERNAL_HEADLESS"] == "1" else "-NoNewWindow"
        # Jobs keep TEMP on D: to spare C:. An installed host has the user's
        # TEMP and its own folder as working directory, and its PowerShell
        # VDD children inherit both, so launch the host the same way.
        command += "; $userTemp=[Environment]::GetEnvironmentVariable('TEMP','User')"
        command += "; if ($userTemp) { $env:TEMP=$userTemp; $env:TMP=$userTemp }"
        command += "; $p=Start-Process -PassThru " + window_option + " -FilePath " + quote(executable)
        command += " -WorkingDirectory " + quote(executable.rsplit("\\", 1)[0])
        command += " -ArgumentList '19876' -RedirectStandardOutput " + quote(ROOT + r"\host.log")
        command += " -RedirectStandardError " + quote(ROOT + r"\host.stderr.log")
        command += "; @{id=$p.Id;start=$p.StartTime.ToUniversalTime().Ticks.ToString();path=$p.Path} | ConvertTo-Json | Set-Content " + quote(pidfile)
        command += "; $p.WaitForExit(); if ($p.ExitCode -ne 0) { throw ('Host exited with ' + $p.ExitCode) }"
        session(command, detach=True, idle=True, timeout=7200,
                run_level=os.environ.get("EM_RUN_LEVEL", "Highest"))
        ps("$deadline=(Get-Date).AddSeconds(60); while (!(Test-Path " + quote(pidfile) + ")) { "
           "if ((Get-Date) -gt $deadline) { throw 'Host did not start; inspect the interactive job' }; Start-Sleep -Milliseconds 100 }; "
           "$record=Get-Content " + quote(pidfile) + " -Raw | ConvertFrom-Json; "
           "$p=Get-Process -Id $record.id; if ($p.Path -ne $record.path -or "
           "$p.StartTime.ToUniversalTime().Ticks.ToString() -ne $record.start) { throw 'Host identity changed at startup' }; "
           # An autoconnecting app sends HELLO for about 10 s. A stalled disk
           # has held host startup for 25 s, so return only once it listens.
           "$deadline=(Get-Date).AddSeconds(60); while (!(Select-String -Path " + quote(ROOT + r"\host.log") +
           " -Pattern 'UDP transport ready' -Quiet)) { "
           "if ($p.HasExited) { throw 'Host exited during startup' }; "
           "if ((Get-Date) -gt $deadline) { throw 'Host did not start listening within 60 s' }; Start-Sleep -Milliseconds 100 }")
    elif action == "host-info" and not args:
        path = quote(ROOT + r"\host.pid.json")
        ps("if (Test-Path " + path + ") { Get-Content " + path + " -Raw } else { Write-Output 'null' }")
    elif action == "vdd-state" and not args:
        ps(script("Vdd-State"))
    elif action == "vdd-disable" and not args:
        ps(script("Vdd-State", "-Disable"))
    elif action == "rss" and not args:
        ps("$record=Get-Content " + quote(ROOT + r"\host.pid.json") + " -Raw | ConvertFrom-Json; "
           "$p=Get-Process -Id $record.id; "
           "if ($p.Path -ne $record.path -or $p.StartTime.ToUniversalTime().Ticks.ToString() -ne $record.start) { "
           "throw 'The tracked host process identity changed' }; "
           "@{pid=$p.Id;rss_kib=[math]::Ceiling($p.WorkingSet64/1024)} | ConvertTo-Json -Compress")
    elif action == "stop-host" and args in ([], ["--kill"]):
        if args:
            ps(script("Stop-Host", "-Force"))
        else:
            # Console-control delivery must originate in the host's desktop
            # session. Session 0 is reserved for the explicit kill path.
            session(script("Stop-Host"), timeout=30)
    elif action == "shot" and len(args) == 1:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", args[0]):
            raise ValueError("Screenshot names use letters, digits, underscores and dashes")
        remote = ROOT + "\\shots\\" + args[0] + ".png"
        session(script("Take-Screenshot", "-Path " + quote(remote)))
        pull(remote, EVIDENCE / "windows" / (args[0] + ".png"))
    elif action == "log" and (not args or (len(args) == 2 and args[0] == "-n" and args[1].isdigit())):
        ps("Get-Content " + quote(ROOT + r"\host.log") + " -Encoding UTF8" + (" -Tail " + args[1] if args else ""))
    elif action == "stderr" and not args:
        ps("Get-Content " + quote(ROOT + r"\host.stderr.log") + " -Encoding UTF8")
    elif action == "gui" and len(args) in (2, 3) and args[0] in ("Stream", "Settings", "QR"):
        if len(args) == 3 and args[2] != '--connected':
            raise ValueError('The optional GUI flag is --connected')
        if not re.fullmatch(r"[A-Za-z0-9_-]+", args[1]):
            raise ValueError("Screenshot names use letters, digits, underscores and dashes")
        remote_path = ROOT + "\\shots\\" + args[1] + ".png"
        session(script("Inspect-Host", "-View " + quote(args[0]) + " -Path " + quote(remote_path) +
                       (" -Connected" if len(args) == 3 else "")), idle=True)
        pull(remote_path, EVIDENCE / "windows" / (args[1] + ".png"))
    elif action == "autostart" and len(args) == 1:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", args[0]):
            raise ValueError('Autostart evidence name is invalid')
        remote_path = ROOT + "\\shots\\" + args[0] + ".png"
        session(script('Inspect-Host', '-View Settings -TestAutostart -Path ' + quote(remote_path)), idle=True)
        pull(remote_path, EVIDENCE / 'windows' / (args[0] + '.png'))
    elif action == "diagnostic" and len(args) == 2 and args[0] in ("h264", "hevc"):
        if not re.fullmatch(r"[A-Za-z0-9_-]+", args[1]):
            raise ValueError("Diagnostic row name is invalid")
        remote_path = ROOT + r"\state\EternalMonitor\diagnostics\amf-first-120-packets." + args[0]
        ps("& " + quote(r"D:\AgentWork\sdk\ffmpeg-7.1.1-full_build-shared\bin\ffmpeg.exe") +
           " -hide_banner -v error -xerror -f " + args[0] + " -i " + quote(remote_path) +
           " -f null NUL; if ($LASTEXITCODE -ne 0) { throw 'AMF diagnostic decode failed' }; "
           "if ((Get-Item " + quote(remote_path) + ").Length -eq 0) { throw 'AMF capture is empty' }; "
           "Write-Output 'Captured AMF packets decoded without errors'")
        pull(remote_path, EVIDENCE / "real" / args[1] / ("amf-first-120-packets." + args[0]))
    elif action == "probe-log" and not args:
        pull(ROOT + r"\input-probe.log", EVIDENCE / "windows" / "input-probe.log")
    elif action == "probe-info" and not args:
        ps("$line=Get-Content " + quote(ROOT + r"\input-probe.log") + " -First 1; "
           "$info=$line | ConvertFrom-Json; if ($info.event -ne 'Ready') { throw 'Probe is not ready' }; "
           "Get-Process -Id $info.pid -ErrorAction Stop | Out-Null; Write-Output $line")
    elif action == "probe-arm" and not args:
        # The host's startup console and VDD task can take focus. Arm only
        # after capture opens; no test input runs until the probe confirms focus.
        log = quote(ROOT + r"\input-probe.log")
        ps("$deadline=(Get-Date).AddSeconds(30); while (!(Select-String -Path " +
           quote(ROOT + r"\host.log") + " -Pattern 'Desktop duplication active' -Quiet)) { "
           "if ((Get-Date) -gt $deadline) { throw 'Capture did not open before input arming' }; Start-Sleep -Milliseconds 100 }; "
           "New-Item -ItemType File -Force " + quote(ROOT + r"\probe.arm") + " | Out-Null; "
           "$deadline=(Get-Date).AddSeconds(10); while (!(Select-String -Path " + log +
           " -Pattern '\"event\":\"Armed\"' -Quiet)) { "
           "if ((Get-Date) -gt $deadline) { throw 'Input probe could not acquire foreground focus' }; Start-Sleep -Milliseconds 100 }; "
           "Write-Output 'Input probe armed with foreground focus'")
    elif action in ("pattern", "probe") and args in (["start"], ["stop"], ["start", "virtual"], ["start", "fullscreen"]):
        if len(args) == 2 and (action, args[1]) not in (("pattern", "virtual"), ("probe", "fullscreen")):
            raise ValueError("Only pattern supports virtual; only probe supports fullscreen")
        flag = ROOT + "\\" + action + ".stop"
        if args == ["stop"]:
            if action == "probe":
                ps(script("Stop-Probe"))
            else:
                ps("New-Item -ItemType File -Force " + quote(flag) + " | Out-Null")
        else:
            if action == "probe":
                ps("if (Test-Path " + quote(ROOT + r"\input-probe.log") + ") { Remove-Item " + quote(ROOT + r"\input-probe.log") + " }")
            options = "-Seconds 7200"
            if len(args) == 2:
                options += " -VirtualDisplay" if action == "pattern" else " -FullScreen"
            elif action == "pattern" and os.environ.get("EM_CAPTURE_DISPLAY") == "virtual":
                options += " -VirtualDisplay"
            elif action == "pattern" and os.environ.get("EM_CAPTURE_DISPLAY"):
                options += " -DisplayName " + quote(os.environ["EM_CAPTURE_DISPLAY"])
            session(script("Pattern-Window" if action == "pattern" else "Input-Probe", options),
                    detach=True, idle=True, timeout=7260,
                    run_level=os.environ.get("EM_RUN_LEVEL", "Highest"))
            if action == "probe":
                ps("$deadline=(Get-Date).AddSeconds(60); while (!(Test-Path " + quote(ROOT + r"\input-probe.log") + ")) { "
                   "if ((Get-Date) -gt $deadline) { throw 'Probe did not start' }; Start-Sleep -Milliseconds 100 }; "
                   "while (!(Get-Content " + quote(ROOT + r"\input-probe.log") + " -First 1)) { "
                   "if ((Get-Date) -gt $deadline) { throw 'Probe did not become ready' }; Start-Sleep -Milliseconds 100 }")
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
