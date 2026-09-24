# EternalMonitor architecture

The v0.3.0 candidate keeps protocol v2. Implementation details below describe the
candidate branches; release and hardware gate status lives in
[HARDWARE_VERIFICATION.md](HARDWARE_VERIFICATION.md).

## Video and session pipeline

```text
Windows desktop → DXGI capture → latest-frame slot → encoder → bounded channel
                                                       ↓
                              transport: session + links + NACK history + ABR
                                      ↙                            ↘
                               UDP datagrams                 framed USB tunnel
                                      ↘                            ↙
                                  iPad MediaLink → MediaDatagrams
                                       ↓                ↓
                                 FrameAssembler     ControlChannel
                                       ↓                ↕
                                 VideoToolbox       input / reports / clock
                                       ↓
                                 Metal NV12 display
```

Capture, encode and audio have dedicated threads. Capture publishes the latest
raw frame; a slow encoder skips obsolete raw frames. The encoder copies into its
own input buffer and releases capture's reference for reuse. Encoded access
units use a bounded channel: silently dropping an arbitrary encoded P-frame
would corrupt the GOP. The USB sender instead drops whole queued access units
when necessary and requests an IDR, keeping its queue bounded.

DXGI captures the selected desktop rectangle and composites the cursor. Synthetic
capture supplies a deterministic moving pattern and an encoded frame counter on
macOS and in tests. YUV420P is the encoder-input default. The optional Auto/BGRA
path checks the selected codec's advertised formats; software x264 stays YUV.
Windows converts with swscale; the macOS development path uses a tested
Accelerate conversion with a swscale fallback.

The supervisor owns pipeline generations, exit reports, watchdogs, backoff and a
restart-storm brake. The client session outlives an encoder/capture restart. A new
stream epoch resets assembly and requests a fresh keyframe. The iPad keeps one
VideoToolbox session per format, sniffs the bitstream codec, and renders NV12 with
Metal using aspect-fit geometry.

The host offers 30/60/90/120 fps. When HELLO2 includes a preference, the effective
rate is the lower host/client value. The iPad requests 30/60/120 fps. A requested
rate is not a claim about achieved decode or panel refresh rate.

## Wire protocol and transport links

The v2 prefix remains eight bytes: EM magic, version, type, flags, reserved byte
and body length. Video uses the existing 32-byte header with session, epoch,
frame/fragment sequence, keyframe flag and capture timestamp. Repair adds a flag
without changing the prefix. New packet types require negotiated capability bits;
optional control tails are append-only and decoders tolerate trailing bytes.
Rust and Swift share golden vectors and stable packet-type registry tests.

| Message | Purpose |
| --- | --- |
| HELLO2 / HELLO_ACK | Nonce-idempotent session setup, capabilities, device/screen information, preferences and pairing credentials |
| HEARTBEAT / STREAM_CONFIG | Host liveness and current codec, resolution, rate and bitrate |
| RECEIVER_REPORT | Cumulative fragment/loss/repair counts, queue depth, jitter and audio statistics every 500 ms |
| NACK | Missing fragment indexes for a specific session, epoch and frame |
| KEYFRAME_REQUEST | Recovery after repair expiry or decoder error |
| PING / PONG | Min-RTT clock estimate for end-to-end latency |
| INPUT_EVENT | Normalized mouse/touch events, HID keys and UTF-16 text |
| Audio | Sequenced Opus packets with discontinuity and capture timing |
| BYE | User, background or shutdown teardown |

One session owns the active link. Another device receives Busy; a reconnect from
the same device supersedes its prior session. Duplicate accepted nonces replay
the same ACK. All established-session traffic is session-id gated. Reports/input
maintain liveness; expiry removes the client and its managed virtual display.

UDP carries video, audio and control on the same socket. The iPad's established
Network.framework backend remains the default. A selectable BSD backend uses a
nonblocking socket, enlarged receive buffer and dispatch-source burst draining;
both feed the same datagram classifier and assembler. Local burst measurements
did not show a reason to change the default before physical iPad testing.

For USB, the host's `transport/usbmuxd.rs` client connects to Apple's service at
127.0.0.1:27015 on Windows (the development Mac uses its Unix socket). It discovers
a device and requests a tunnel to iPad port 9877. The app listener binds only to
loopback. EMLINK v1 starts with an eight-byte preamble, then a little-endian
16-bit length and each existing datagram; invalid lengths over 1400 bytes fail
before allocation. Host video queues are limited by whole-frame count and bytes.

ConnectionManager switches to an authenticated physical USB link when available,
tears down the old session, and returns to a remembered WiFi target after unplug.
Manual Disconnect pauses automatic USB attachment. Loopback fixtures prove this
state machine and framing, while the cable/trust/device-service path still needs
a physical iPad.

## Loss repair and adaptive bitrate

The host retains exact first-transmission datagrams in a per-session history,
bounded to 96 frames and 12 MiB. NACK replies change only the retransmit flag;
indexes, fragment counts and epochs must match. A fragment is resent at most
once per 5 ms. Repairs bypass first-transmission fault injection and are
serviced while a paced keyframe is waiting.

The iPad holds up to eight frames behind an incomplete one, for an RTT-aware
8–25 ms repair window. It requests each missing fragment as soon as a later
fragment shows the gap and retries once after its RTT plus 5 ms. The window
counts only time in which traffic flows: it stops while media is silent for more
than a frame period, or while no repair arrives for any frame although a request
is overdue, by at most 100 ms per frame. The iPad retires expired frames and
drains completed frames in order. Reported repaired and unrecovered counts
remain separate. Keyframe requests remain the fallback for expiry and decode
errors.

ABR uses the host's 4–50 Mbps ladder under the user's ceiling. Unrecovered loss,
repair pressure, jitter and receive/decode queues can lower bitrate; sustained
clean reports permit an increase. It uses report deltas and resets on epoch
changes. A bitrate change reopens the encoder and forces an IDR. Packet pacing
bounds keyframe bursts without blocking urgent control repairs.

## Audio

WASAPI loopback captures the Windows default render endpoint. Endpoint or format
changes reopen capture; failure disables audio with a visible status instead of
killing video. The portable source produces a known 1 kHz tone and silence gaps.
FFmpeg resamples to 48 kHz stereo and encodes 20 ms Opus packets at 128 kbps. Audio
is sent only when the host setting and client WANTS_AUDIO capability both allow
it. UDP audio is unpaced; USB uses the existing frame wrapper.

The iPad builds libopus from the pinned Swift package and uses one decoder path
on simulator and device. A bounded jitter buffer starts at 60 ms, grows to 120 ms
after repeated underruns and shrinks after sustained clean playback. Missing
packets use Opus concealment; compact quiet packets and DTX gaps do not count as
network loss. AVAudioSourceNode supplies Float32 audio to AVAudioEngine. Muting
leaves video connected.

FFmpeg's low-delay CELT mode does not provide SILK in-band FEC or a working DTX
AVOption. Quiet periods use explicitly reset, compact Opus silence packets.
[Audio codec notes](docs/audio-codec.md) record the decision and fixture tests.

## Pairing and input

Pairing is required by default. A random 128-bit host token authenticates known
iPads; a six-digit code authorizes first contact and rotates after success. Five
failed attempts per source IP in a minute trigger the cooldown; the tracking map
is bounded. QR links include the token. The iPad stores learned credentials in
Keychain, clears rejected stale tokens, and offers Forget paired hosts. USB trusts
physical access. This is access control over an unencrypted local connection;
video, audio, input and pairing credentials are not confidential on the network.

Touch coordinates are normalized within the displayed video, excluding letterbox
bars. The host maps them through the captured output rectangle onto the Windows
virtual desktop. A pure gesture machine handles tap, drag, two-finger scrolling
and hold/right-click. Pencil contact moves the mouse immediately; Windows pen
pressure injection is not implemented.

Keyboard events carry USB HID page 0x07 usages, mapped to Windows scan codes and
extended-key flags. Text carries UTF-16 units for SendInput Unicode events.
Pointer buttons and hover share the same session gate. Edge duplicates carry
one event ID, and disconnect releases held keys/buttons. The on-screen keyboard
has sticky modifiers and navigation keys. Command maps to Ctrl by default, with
an opt-in Win mapping; iPadOS retains its reserved shortcuts.

## Host integration and verification

The installer registers SYSTEM VDD enable/disable tasks and grants ordinary
Users read/execute permission. The host starts a virtual display only for a
connected client, writes the requested mode first in VDD XML, and reports actual
task failures. It also ships TCP/UDP firewall rules. Runtime upgrade, task-ACL and
uninstall checks remain hardware gates.

mDNS advertises `_eternaldisplay._udp` with version/protocol/platform metadata,
refreshes without an unregister gap, and sends goodbye on exit. Manual IP and QR
remain alternatives. Logs rotate through the current session and two prior files.
An optional background update request runs at most once per day with a three-second
timeout. The GUI shows current client/link, codec, repair/loss, audio and fallback
state; 0.36 egui snapshots cover representative views.

Pure Rust/Swift tests, golden vectors, encoded-stream integration tests, native
UI tests, the simulator matrix and RSS/FPS soaks provide complementary checks.
Windows builds compile every platform-specific path; the interactive reference-PC
campaign proves DXGI, hardware encoders, WASAPI, SendInput and installer behavior.
The physical iPad runbook covers device decoding, cable trust, LAN discovery,
latency/input feel and ProMotion. None of those hardware results are inferred
from a compile or simulator pass.
