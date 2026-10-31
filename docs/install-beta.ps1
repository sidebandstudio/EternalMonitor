# EternalMonitor for Windows, Beta channel, installed from a terminal:
#
#   irm https://eternalmonitor.dev/install-beta.ps1 | iex
#
# Runs install.ps1 for the Beta build: the newest nightly from the moving
# "beta" GitHub release, checked against its published SHA-256.
# $env:EM_INSTALL_DRY_RUN = 1 stops after the check, before the installer starts.

& {
    $EternalMonitorChannel = 'beta'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    Invoke-RestMethod -UseBasicParsing -Uri 'https://eternalmonitor.dev/install.ps1' | Invoke-Expression
}
