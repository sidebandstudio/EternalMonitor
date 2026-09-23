# Reference PC tests

These scripts use `ssh windows` from devbox1. Builds, toolchains, temporary
files, and logs stay under `D:\AgentWork`. `remote.sh sync <branch>` copies
the scripts and updates the clean PC checkout. It refuses to overwrite
uncommitted work.

```sh
scripts/win/remote.sh sync v030/p0-harness
scripts/win/remote.sh build --release
scripts/win/remote.sh firewall 'D:\AgentWork\Eternal-Monitor\target\release\eternal-host.exe'
scripts/e2e_matrix.sh --real
```

The session runner refuses an RDP or missing console session. Pattern,
probe, and host runs require two minutes idle because host startup can
change the virtual display. Screenshots can run while the console is active.
Detached tasks expire after their timeout and unregister on exit. Job output
and exit status are in `D:\AgentWork\em-v030\jobs`. Host settings are isolated
in `D:\AgentWork\em-v030\state`, preserving the installed app's preferences.

`remote.sh stop-host` checks the recorded PID, creation time, and executable
path before stopping the host it started. `pattern stop` and `probe stop`
close the test windows using their flag files. Check task output if a window
does not appear; an SSH process cannot capture the interactive desktop.

`shot <name>` saves private desktop screenshots in the handoff evidence
folder, outside the repository. `EM_EVIDENCE_DIR` can select another private
folder. Never commit desktop screenshots or input-probe logs.

Record current AC power timeouts before changing them and restore those same
values after the campaign. Remove temporary firewall rules with
`remote.sh firewall <path> --remove`, close test windows, stop the host, and
verify that the virtual display is disabled when finished.
