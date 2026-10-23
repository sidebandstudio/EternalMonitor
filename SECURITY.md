# Security policy

## Supported versions

Security fixes go into the latest release, the one on the
[download page](https://eternalmonitor.dev/download.html). Older releases are
not maintained; please update both apps.

## Reporting a vulnerability

Please report privately rather than in a public issue:

- Use GitHub's [private vulnerability reporting](https://github.com/sidebandstudio/EternalMonitor/security/advisories/new), or
- email [hello@sideband.studio](mailto:hello@sideband.studio).

Include the versions of both apps, what an attacker can do, and the steps to
reproduce it. Leave out real pairing tokens and codes. Please allow time for a fix
before you publish details.

## Security model

Knowing these limits helps you judge whether a finding is new.

- The stream is not encrypted. Video, audio and input cross the local network in
  the clear, so anyone who can capture that traffic can see it. This is documented
  and intended to change later; it does not need a report.
- Pairing controls who may connect. A new iPad on Wi-Fi needs the host's six-digit
  code or its QR token. The host limits wrong guesses, and the iPad keeps the token
  in its Keychain. USB connections trust physical access and need no code.
- The Windows host runs as the signed-in user. It turns the virtual display on and
  off through two scheduled tasks that the installer registers to run as SYSTEM.
- The installer is not code-signed yet. Check its SHA-256 against the release page.
- In-app updates (Windows) trust the network for nothing: the host fetches over
  HTTPS from an allowlist of GitHub hosts, and the installer must match the
  SHA-256 in both eternalmonitor.dev/latest.json and the GitHub release. Against
  other software already running as your Windows user, the updater adds defense
  in depth rather than a guarantee. Its helper loads DLLs only from System32 and
  imports only Windows system libraries. It inherits only its output pipe from
  the host, and Setup and the restarted app inherit no handles from it. It refuses an update folder reached
  through a link and keeps the installer open so it can't be changed. Setup
  starts suspended and runs only if it is the verified file. If the old host
  takes more than 60 seconds to close, Setup does not run, but the helper keeps
  waiting for that process and starts the app once it exits, however late: a
  start while the old host still runs would only show its window and quit,
  leaving nothing running when it finally closed. Microsoft does not
  treat UAC as a security boundary, and such software could ask for elevation
  itself, so same-user races beyond these are out of scope.

Reports are welcome for anything that lets someone:

- connect or inject input without pairing;
- read or reuse a pairing token;
- run code through the host, the installer or its scheduled tasks;
- crash the host or the iPad app with crafted network traffic.
