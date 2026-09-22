# Pairing

The Windows host requires pairing by default. Enter the six-digit code from its Stream tab on the iPad, or scan the host QR code. A successful code entry rotates the displayed code. USB connections pair automatically through the trusted Apple device tunnel.

The host generates a nonzero 128-bit token with the operating system random source and stores it in the host settings. The QR URL includes that token. The iPad saves it in the Keychain under the host name and address, accessible while the device is unlocked and restricted to that device. Reconnecting sends the saved token; a rejected token opens the code sheet again.

"New code" replaces the displayed code without forgetting existing iPads. "Regenerate token" forgets every paired iPad and ends the current authorized session. The iPad's "Forget paired hosts" removes its saved tokens; its next connection requires a code. Turning "Require pairing" back on also ends any session established while pairing was disabled.

Five failed authorization attempts from one IP within 60 seconds block further code attempts from that IP for 60 seconds. The countdown does not restart for blocked retries. Existing token-authenticated connections and USB connections remain available. The host bounds address tracking and never evicts an active ban to admit another address.

An exact retransmission of an accepted HELLO receives the original ACK, so a lost ACK does not invalidate a successful code entry. Changed credentials or handshake fields require fresh authorization. Failed and busy ACKs never contain the host token.

Pairing authorizes a connection. Media and control packets are not encrypted in v0.3.0; use a trusted LAN or an encrypted network tunnel. Anyone with the QR code, saved host settings, or a captured token can authenticate until the token is regenerated. Keep these private.

## Verification

Rust tests cover code rotation, token rejection and revocation, duplicate ACKs, USB trust, busy sessions, bounded per-IP limits and expiry. A real UDP and software-encoder test decodes video after code pairing and reconnects with the returned token. Swift tests cover credential storage, code validation, cooldowns, QR parsing and stale ACK rejection. `scripts/e2e_pairing.sh` drives wrong-code entry, successful pairing, app restart with the real Keychain, and forgetting the saved host through XCUITest. Simulator builds use ad-hoc signing to provide the Keychain app identity.
