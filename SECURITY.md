# Security policy

## Supported versions

Security fixes go into the latest 0.3 build, currently `v0.3.0`. The 0.1
releases are no longer maintained; please update both apps.

## Reporting a vulnerability

Please report privately rather than in a public issue:

- Use GitHub's [private vulnerability reporting](https://github.com/whoisaldo/EternalMonitor/security/advisories/new), or
- email [aliyounes@eternalreverse.com](mailto:aliyounes@eternalreverse.com).

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

Reports are welcome for anything that lets someone:

- connect or inject input without pairing;
- read or reuse a pairing token;
- run code through the host, the installer or its scheduled tasks;
- crash the host or the iPad app with crafted network traffic.
