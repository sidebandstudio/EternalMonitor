# Audio codec behavior

The host uses FFmpeg 7's `libopus` encoder with 48 kHz stereo Float32 input,
960 samples per packet (20 ms), 128 kbps, constrained VBR, and
`application=lowdelay`. swresample handles the Windows endpoint's mix rate
and channel layout before a bounded remainder assembles the Opus frames.

FFmpeg 7 does not expose libopus's `OPUS_SET_DTX` as an AVOption. Passing
`dtx=1` to its options dictionary would not enable it. Also, Opus restricted
low-delay mode uses CELT, whose packets do not carry SILK's in-band FEC.
The wrapper accepts `fec=1` and `packet_loss=10`, but those settings must not
be described as demonstrated loss protection in this mode.

For digital silence (a complete stereo frame of exact zeros), the host
reopens the encoder once at the transition. This clears its tonal history
and produces real three-byte Opus silence packets. The first packet after
the reopen carries `DISCONTINUITY`; the decoder resets its prediction while
retaining the packet's position in the playout timeline. No synthetic Opus
bytes, speech VAD, or undocumented FFmpeg internals are used. This is
silence compression, rather than libopus DTX: the host still sends 50 small
packets per second while the input remains silent.

The codec test decodes 4.2 seconds of a -12 dBFS 1 kHz signal, including two
100 ms silence gaps. It verifies packet sequence/time, exact 960-sample
decodes, sub-10-byte silence packets, the tone level before and after each
gap, and a peak below 0.35 (no transition burst). A separate test covers
full-scale noise, alternating samples, DC, and silence against the
smallest supported 576-byte datagram. Both run on macOS and Windows.

References: [FFmpeg 7 libopus wrapper](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavcodec/libopusenc.c),
[Opus encoder controls](https://opus-codec.org/docs/opus_api-1.5/group__opus__encoderctls.html),
[WASAPI loopback recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording),
[WASAPI capture buffers and timestamps](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer).
