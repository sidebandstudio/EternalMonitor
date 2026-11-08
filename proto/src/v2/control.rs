//! v2 control datagrams: everything that is not media.
//!
//! Layout — a 16-byte control header, then one message body:
//!
//! ```text
//! [0..8]   common prefix   (packet_type = the message discriminator)
//! [8..12]  session_id u32  0 only in HELLO2 (no session assigned yet)
//! [12..16] msg_seq    u32  per-sender monotonic counter across all control
//!                          types, starting at 1; receivers apply latest-wins
//!                          per type and may drop stale ones
//! ```
//!
//! Reliability is per-message-semantics (RTCP model): HELLO2 retries until
//! acked, BYE fires several times, everything else is periodic and supersedes
//! itself. There is no generic retransmit layer.
//!
//! Evolution rule: bodies are append-only. Decoders MUST ignore trailing bytes
//! they do not understand, so fields can be appended without a version bump.

use super::{CommonPrefix, PacketType, WireError};

/// Size of the control header (common prefix included).
pub const CONTROL_HEADER_SIZE: usize = 16;
/// Longest device/host name accepted on the wire, in UTF-8 bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Codec identifiers used in [`StreamConfig`] and [`Hello2::decoder_caps`].
pub const CODEC_H264: u8 = 0;
pub const CODEC_HEVC: u8 = 1;

/// [`Hello2::decoder_caps`] bits.
pub const CAP_DECODE_H264: u16 = 1 << 0;
pub const CAP_DECODE_HEVC: u16 = 1 << 1;
pub const CAP_DECODE_HEVC_10BIT: u16 = 1 << 2;

/// [`Hello2::feature_caps`] bits.
pub const FEATURE_WANTS_INPUT: u16 = 1 << 0;
pub const FEATURE_WANTS_AUDIO: u16 = 1 << 1;
pub const FEATURE_SUPPORTS_NACK: u16 = 1 << 2;
/// The iPad understands [`HelloStatus::Stopped`], [`HelloStatus::Restarting`]
/// and [`HelloStatus::Failed`] (0.3.5 and later). Older iPads drop an ACK
/// whose status they don't know, so the host sends them only statuses 0-5.
pub const FEATURE_STANDBY_STATUS: u16 = 1 << 3;
/// The iPad app checks for its own updates (0.3.2 and later), so a HELLO2
/// without it comes from 0.3.1 or older. Older hosts ignore it.
pub const FEATURE_CHECKS_UPDATES: u16 = 1 << 4;
/// The iPad draws the pointer itself from POINTER_SHAPE and POINTER_STATE.
/// A host with [`HOSTCAP_POINTER`] then leaves the pointer out of the video.
/// Bit 3 is taken by another change in review.
pub const FEATURE_DRAWS_POINTER: u16 = 1 << 5;
/// The iPad may ask for smaller media datagrams in a
/// [`KeyframeRequest::max_media_dgram`], and wants [`Heartbeat::frames_sent`]
/// to tell a PC that sends nothing from video lost on the way (0.3.5 and
/// later). Bit 5 is [`FEATURE_DRAWS_POINTER`].
pub const FEATURE_MEDIA_SIZE: u16 = 1 << 6;
pub const HOSTCAP_NACK: u16 = 1 << 0;
pub const HOSTCAP_AUDIO: u16 = 1 << 1;
pub const HOSTCAP_USB: u16 = 1 << 2;
pub const HOSTCAP_KEYBOARD: u16 = 1 << 3;
/// Native Windows pen injection, including input_ver 2 tilt and hover.
pub const HOSTCAP_PEN: u16 = 1 << 4;
/// Media `frame_seq` counts the access units sent in a stream epoch without
/// gaps, so a forward gap is a frame lost in transit. Without it, numbers
/// may skip frames that were never sent. Older iPads ignore the bit.
/// Every 0.3.4 and later host sets it in every reply that accepts a
/// session, and iPads read an accepted reply without it as a PC to update,
/// so it stays set there.
pub const HOSTCAP_CONTIGUOUS_FRAME_SEQ: u16 = 1 << 5;
/// The host sends POINTER_SHAPE and POINTER_STATE, and leaves the pointer
/// out of the video, to a client with [`FEATURE_DRAWS_POINTER`].
pub const HOSTCAP_POINTER: u16 = 1 << 6;
/// The host honours [`KeyframeRequest::max_media_dgram`] for the rest of the
/// session and puts [`Heartbeat::frames_sent`] in its heartbeats. Set only
/// for an iPad whose HELLO2 has [`FEATURE_MEDIA_SIZE`], so an older iPad
/// gets the same bytes as before. Bit 6 is [`HOSTCAP_POINTER`].
pub const HOSTCAP_MEDIA_SIZE: u16 = 1 << 7;
/// The smallest media datagram an iPad may ask for: the IPv4 minimum
/// reassembly size, also the smallest packet size the PC's own setting takes.
pub const MIN_MEDIA_DGRAM: u16 = 576;

/// [`StreamConfig::flags`] bits.
pub const STREAM_FLAG_SOFTWARE_ENCODER: u8 = 1 << 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlHeader {
    pub packet_type: PacketType,
    pub session_id: u32,
    pub msg_seq: u32,
}

/// Current stream parameters, embedded in HELLO_ACK and HEARTBEAT and sent
/// standalone as STREAM_CONFIG on change. 16 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamConfig {
    pub stream_epoch: u32,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub codec: u8,
    pub flags: u8,
    pub bitrate_bps: u32,
}

pub const STREAM_CONFIG_SIZE: usize = 16;

impl StreamConfig {
    fn encode_into(self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.stream_epoch.to_le_bytes());
        buf[4..6].copy_from_slice(&self.width.to_le_bytes());
        buf[6..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8..10].copy_from_slice(&self.fps.to_le_bytes());
        buf[10] = self.codec;
        buf[11] = self.flags;
        buf[12..16].copy_from_slice(&self.bitrate_bps.to_le_bytes());
    }

    fn decode(buf: &[u8]) -> Result<Self, WireError> {
        if buf.len() < STREAM_CONFIG_SIZE {
            return Err(WireError::Truncated);
        }
        Ok(Self {
            stream_epoch: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            width: u16::from_le_bytes([buf[4], buf[5]]),
            height: u16::from_le_bytes([buf[6], buf[7]]),
            fps: u16::from_le_bytes([buf[8], buf[9]]),
            codec: buf[10],
            flags: buf[11],
            bitrate_bps: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
        })
    }
}

/// Client → host session request. session_id = 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello2 {
    pub proto_min: u8,
    pub proto_max: u8,
    /// Fresh random value per connect attempt; the host echoes it in HELLO_ACK
    /// and treats a duplicate HELLO2 with the same nonce as a retransmit.
    pub client_nonce: u32,
    /// Kept for parity with v1 semantics; normally equals the UDP source port.
    pub listen_port: u16,
    pub decoder_caps: u16,
    pub feature_caps: u16,
    pub screen_px_w: u16,
    pub screen_px_h: u16,
    pub screen_pt_w: u16,
    pub screen_pt_h: u16,
    pub refresh_hz: u8,
    pub device_name: String,
    pub device_id: u64,
    pub preferred_fps: u8,
    pub auth_token: [u8; 16],
    pub pairing_code: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HelloStatus {
    Ok = 0,
    Busy = 1,
    VersionUnsupported = 2,
    Error = 3,
    Unauthorized = 4,
    RateLimited = 5,
    /// The PC answered but isn't streaming: someone clicked Stop on it.
    Stopped = 6,
    /// The PC is restarting its stream; ask again shortly.
    Restarting = 7,
    /// The PC stopped streaming after repeated errors, until someone clicks
    /// Start. [`HelloAck::reason`] carries a [`FailedReason`] token.
    Failed = 8,
}

impl HelloStatus {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Ok,
            1 => Self::Busy,
            2 => Self::VersionUnsupported,
            3 => Self::Error,
            4 => Self::Unauthorized,
            5 => Self::RateLimited,
            6 => Self::Stopped,
            7 => Self::Restarting,
            8 => Self::Failed,
            _ => return None,
        })
    }
}

/// Host → client HELLO2 response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloAck {
    pub status: HelloStatus,
    pub accepted_version: u8,
    pub client_nonce: u32,
    /// Nonzero iff status == Ok.
    pub session_id: u32,
    pub heartbeat_interval_ms: u16,
    pub report_interval_ms: u16,
    pub liveness_timeout_ms: u16,
    pub stream_config: StreamConfig,
    pub host_name: String,
    pub auth_token: [u8; 16],
    pub host_caps: u16,
    /// Why the PC isn't streaming, for [`HelloStatus::Failed`]: a
    /// [`FailedReason`] token, empty otherwise. Appended after `host_caps`
    /// (0.3.5), at most [`MAX_REASON_LEN`] bytes; older parsers ignore it.
    pub reason: String,
}

/// Longest [`HelloAck::reason`] on the wire, in bytes.
pub const MAX_REASON_LEN: usize = 32;

/// The longest HELLO_ACK datagram: the header, the fixed fields and stream
/// config, the longest host name, the token and host caps, and the longest
/// reason.
pub const MAX_HELLO_ACK_LEN: usize =
    CONTROL_HEADER_SIZE + 16 + STREAM_CONFIG_SIZE + 1 + MAX_NAME_LEN + 16 + 2 + 1 + MAX_REASON_LEN;

/// A HELLO2 that sets [`FEATURE_STANDBY_STATUS`] is padded with zeros to at
/// least this size, so any answer to it is no larger than the question. A
/// PC answers while it isn't streaming only within that bound. Older
/// parsers ignore the padding, as any trailing body bytes.
pub const STANDBY_HELLO2_LEN: usize = MAX_HELLO_ACK_LEN;

/// Why a PC stopped streaming after repeated errors, as [`HelloAck::reason`]
/// carries it. A token, not text: the iPad says it in its own words, and an
/// iPad that doesn't know a token shows the general advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedReason {
    Capture,
    Encoder,
    Transport,
    SecureDesktop,
    DisplayChanging,
    NoDisplay,
    UnsupportedOnThisGpu,
    TooManyCaptureApps,
    SessionDisconnected,
    DriverNotResponding,
}

impl FailedReason {
    pub const ALL: [FailedReason; 10] = [
        Self::Capture,
        Self::Encoder,
        Self::Transport,
        Self::SecureDesktop,
        Self::DisplayChanging,
        Self::NoDisplay,
        Self::UnsupportedOnThisGpu,
        Self::TooManyCaptureApps,
        Self::SessionDisconnected,
        Self::DriverNotResponding,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Encoder => "encoder",
            Self::Transport => "transport",
            Self::SecureDesktop => "secure_desktop",
            Self::DisplayChanging => "display_changing",
            Self::NoDisplay => "no_display",
            Self::UnsupportedOnThisGpu => "unsupported_gpu",
            Self::TooManyCaptureApps => "too_many_capture_apps",
            Self::SessionDisconnected => "session_disconnected",
            Self::DriverNotResponding => "driver_not_responding",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.token() == token)
    }
}

/// [`Heartbeat::notice`] values. A notice is appended to the heartbeat only
/// when it is not [`NOTICE_NONE`]: iPads older than the field ignore the byte,
/// and an iPad ignores a value it does not know.
pub const NOTICE_NONE: u8 = 0;
/// The PC is locked or shows a permission prompt, so Windows lets nothing
/// capture its screen and the picture stops until the user returns.
pub const NOTICE_PC_LOCKED: u8 = 1;
/// No display is attached to the PC's desktop (a closed lid, screens off or
/// unplugged), so there is nothing to capture until one comes back.
pub const NOTICE_NO_DISPLAY: u8 = 2;

/// Host → client, every heartbeat interval while a session is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    pub host_time_us: u64,
    pub stream_config: StreamConfig,
    /// What the iPad should say over the picture, for as long as it lasts.
    pub notice: u8,
    /// Video frames sent to this session so far, for an iPad that set
    /// [`FEATURE_MEDIA_SIZE`]; `None` for others. Appended after `notice`,
    /// which is then always written.
    pub frames_sent: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ByeReason {
    UserDisconnect = 0,
    AppBackground = 1,
    HostShuttingDown = 2,
    Error = 3,
    Superseded = 4,
}

impl ByeReason {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::UserDisconnect,
            1 => Self::AppBackground,
            2 => Self::HostShuttingDown,
            3 => Self::Error,
            4 => Self::Superseded,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyframeReason {
    GapLoss = 0,
    DecodeError = 1,
    Startup = 2,
    Resume = 3,
}

impl KeyframeReason {
    fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::GapLoss,
            1 => Self::DecodeError,
            2 => Self::Startup,
            3 => Self::Resume,
            _ => return None,
        })
    }
}

/// Client → host request for an IDR (PLI). Idempotent; rate-limited host-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyframeRequest {
    pub stream_epoch: u32,
    pub last_complete_seq: u32,
    pub reason: KeyframeReason,
    /// The largest media datagram the client asks for from now on in this
    /// session, at least [`MIN_MEDIA_DGRAM`]; 0 asks nothing. Appended
    /// (0.3.5): sent only to a host with [`HOSTCAP_MEDIA_SIZE`], and older
    /// hosts ignore it as any trailing body bytes.
    pub max_media_dgram: u16,
}

/// Requests missing fragments from one frame. Indices are unique and sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nack {
    pub stream_epoch: u32,
    pub frame_seq: u32,
    pub frag_count: u16,
    pub missing: Vec<u16>,
}

impl Nack {
    pub fn validate(&self) -> Result<(), WireError> {
        if self.missing.is_empty() || self.missing.len() > 64 {
            return Err(WireError::InvalidField("missing_len"));
        }
        if self.missing.iter().any(|&index| index >= self.frag_count)
            || self.missing.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(WireError::InvalidField("missing"));
        }
        Ok(())
    }
}

/// Client → host stats, every report interval. Cumulative per epoch — the host
/// diffs consecutive reports. Doubles as the client's liveness signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReceiverReport {
    pub stream_epoch: u32,
    pub highest_seq: u32,
    pub frames_complete: u32,
    pub frames_dropped: u32,
    pub frags_received: u32,
    pub frags_lost: u32,
    /// RFC 3550-style interarrival jitter, microseconds.
    pub jitter_us: u32,
    pub decode_fps_x10: u16,
    pub assembler_depth: u8,
    pub decode_depth: u8,
    /// 0 until clock sync has converged.
    pub e2e_latency_ms_x10: u16,
    pub rtt_ms_x10: u16,
    pub frags_repaired: u32,
    pub nacks_sent: u32,
    pub audio_packets_lost: u32,
    pub audio_buffer_ms: u16,
    /// The last pointer shape the client holds complete, 0 for none. The
    /// host sends the current shape again when this differs from it.
    pub pointer_shape_id: u32,
}

pub const RECEIVER_REPORT_SIZE: usize = 36;

/// Client → host clock probe. `t1_us` is the client clock at send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub t1_us: u64,
}

/// Host → client probe reply: echoes t1, adds host receive/send times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub t1_us: u64,
    pub t2_us: u64,
    pub t3_us: u64,
}

/// Touch/pencil/mouse/key event, client → host. See the input module for the
/// coordinate contract (normalized across the displayed video content rect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub input_ver: u8,
    pub kind: u8,
    pub phase: u8,
    pub buttons: u8,
    /// Per-session monotonic; began/ended events are sent twice for loss
    /// tolerance, so the host must dedupe on this id.
    pub event_id: u32,
    pub x_norm: u16,
    pub y_norm: u16,
    pub pressure_x1000: u16,
    pub scroll_dx: i16,
    pub scroll_dy: i16,
    pub keycode: u16,
    pub modifiers: u8,
    pub client_time_us: u64,
    /// input_ver 2 appends signed degrees, positive right / toward the user.
    pub tilt_x: i16,
    pub tilt_y: i16,
}

/// [`Identify`] and [`Identity`] layout version.
pub const IDENTIFY_VERSION: u8 = 1;
/// What an [`Identity`] proof signs after the nonce.
pub const IDENTIFY_CONTEXT: &[u8] = b"EternalMonitor identify v1";

/// At most this many addresses in an [`Identity`].
pub const IDENTITY_MAX_ADDRESSES: usize = 4;

/// Client → host, session_id 0: before reconnecting to a PC it found at a new
/// address, the iPad checks that it is the same PC, at that address. The host
/// answers with an [`Identity`] whose proof only the PC holding the pairing
/// token can make. Reserved bytes keep the request as long as the answer.
/// Hosts before 0.3.5 don't know the type and drop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identify {
    pub version: u8,
    pub nonce: [u8; 16],
}

/// Host → client, session_id 0: the port the host receives the stream on and
/// its home network IPv4 addresses (at most [`IDENTITY_MAX_ADDRESSES`]), and
/// `proof`, HMAC-SHA256 keyed with the pairing token over
/// [`identity_message`]. The iPad goes on only when the address it asked at
/// is one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub version: u8,
    pub nonce: [u8; 16],
    pub port: u16,
    pub addresses: Vec<[u8; 4]>,
    pub proof: [u8; 32],
}

/// Both bodies: version, nonce, port, address count, the address slots
/// (unused ones zero) and the proof.
pub const IDENTIFY_BODY_SIZE: usize = 1 + 16 + 2 + 1 + 4 * IDENTITY_MAX_ADDRESSES + 32;

/// What an [`Identity`] proof signs: `nonce ‖ IDENTIFY_CONTEXT ‖ port (LE) ‖
/// count ‖ addresses`. Only the first [`IDENTITY_MAX_ADDRESSES`] count.
pub fn identity_message(nonce: &[u8; 16], port: u16, addresses: &[[u8; 4]]) -> Vec<u8> {
    let addresses = &addresses[..addresses.len().min(IDENTITY_MAX_ADDRESSES)];
    let mut message = Vec::with_capacity(16 + IDENTIFY_CONTEXT.len() + 3 + 4 * addresses.len());
    message.extend_from_slice(nonce);
    message.extend_from_slice(IDENTIFY_CONTEXT);
    message.extend_from_slice(&port.to_le_bytes());
    message.push(addresses.len() as u8);
    for address in addresses {
        message.extend_from_slice(address);
    }
    message
}

pub const INPUT_EVENT_SIZE: usize = 30;
pub const PEN_INPUT_EVENT_SIZE: usize = INPUT_EVENT_SIZE + 4;

/// The pointer messages' body layout version.
pub const POINTER_VERSION: u8 = 1;
/// [`PointerShapeChunk::flags`]: the image is followed by an invert mask.
pub const POINTER_SHAPE_INVERT: u8 = 1 << 0;
/// [`PointerState::flags`]: the pointer is on the captured screen and shown.
pub const POINTER_VISIBLE: u8 = 1 << 0;
/// Widest and tallest pointer image sent, in pixels.
pub const POINTER_MAX_SIDE: u16 = 512;
/// The fixed fields of a POINTER_SHAPE body, before its bytes.
pub const POINTER_SHAPE_FIXED_SIZE: usize = 26;
/// Image bytes in one POINTER_SHAPE datagram.
pub const POINTER_CHUNK_MAX: usize =
    super::MAX_DGRAM_SIZE - CONTROL_HEADER_SIZE - POINTER_SHAPE_FIXED_SIZE;
pub const POINTER_STATE_SIZE: usize = 24;

/// Bytes of a pointer image: `height` rows of `width` premultiplied BGRA
/// pixels, then, with [`POINTER_SHAPE_INVERT`], `height` rows of
/// `ceil(width / 8)` mask bytes, most significant bit first, where a set bit
/// inverts the picture under that pixel.
pub fn pointer_shape_len(width: u16, height: u16, flags: u8) -> usize {
    let (w, h) = (usize::from(width), usize::from(height));
    let mask = if flags & POINTER_SHAPE_INVERT != 0 {
        w.div_ceil(8) * h
    } else {
        0
    };
    w * h * 4 + mask
}

/// Host → client: one chunk of the pointer image with id `shape_id`. The
/// client keeps chunks until it holds all `total_len` bytes, then draws that
/// image at the position in [`PointerState`]. A new id replaces the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerShapeChunk {
    /// Nonzero; changes whenever the image does.
    pub shape_id: u32,
    /// The Windows pointer type it came from (1 monochrome, 2 color,
    /// 4 masked color). For diagnostics; the bytes are already converted.
    pub source_type: u8,
    pub flags: u8,
    pub width: u16,
    pub height: u16,
    /// The click point inside the image.
    pub hot_x: u16,
    pub hot_y: u16,
    pub total_len: u32,
    pub offset: u32,
    pub data: Vec<u8>,
}

/// Splits one pointer image into the chunks that carry it.
#[allow(clippy::too_many_arguments)]
pub fn pointer_shape_chunks(
    shape_id: u32,
    source_type: u8,
    flags: u8,
    (width, height): (u16, u16),
    (hot_x, hot_y): (u16, u16),
    bytes: &[u8],
) -> Vec<PointerShapeChunk> {
    debug_assert_eq!(bytes.len(), pointer_shape_len(width, height, flags));
    bytes
        .chunks(POINTER_CHUNK_MAX)
        .enumerate()
        .map(|(index, data)| PointerShapeChunk {
            shape_id,
            source_type,
            flags,
            width,
            height,
            hot_x,
            hot_y,
            total_len: bytes.len() as u32,
            offset: (index * POINTER_CHUNK_MAX) as u32,
            data: data.to_vec(),
        })
        .collect()
}

/// Host → client: where the pointer is, latest wins by `msg_seq`. `x`/`y`
/// place the image's top-left corner in video pixels, and may be negative
/// or past the edge when the pointer is partly off the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointerState {
    pub flags: u8,
    /// The image to draw; 0 while the host has none.
    pub shape_id: u32,
    pub x: i32,
    pub y: i32,
    /// Host clock when the host read this position.
    pub host_time_us: u64,
}

/// One parsed control message.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    Hello2(Hello2),
    HelloAck(HelloAck),
    Heartbeat(Heartbeat),
    Bye(ByeReason),
    KeyframeRequest(KeyframeRequest),
    Nack(Nack),
    ReceiverReport(ReceiverReport),
    Ping(Ping),
    Pong(Pong),
    StreamConfig(StreamConfig),
    InputEvent(InputEvent),
    PointerShape(PointerShapeChunk),
    PointerState(PointerState),
    Identify(Identify),
    Identity(Identity),
}

impl ControlMessage {
    fn packet_type(&self) -> PacketType {
        match self {
            Self::Hello2(_) => PacketType::Hello2,
            Self::HelloAck(_) => PacketType::HelloAck,
            Self::Heartbeat(_) => PacketType::Heartbeat,
            Self::Bye(_) => PacketType::Bye,
            Self::KeyframeRequest(_) => PacketType::KeyframeRequest,
            Self::Nack(_) => PacketType::Nack,
            Self::ReceiverReport(_) => PacketType::ReceiverReport,
            Self::Ping(_) => PacketType::Ping,
            Self::Pong(_) => PacketType::Pong,
            Self::StreamConfig(_) => PacketType::StreamConfig,
            Self::InputEvent(_) => PacketType::InputEvent,
            Self::PointerShape(_) => PacketType::PointerShape,
            Self::PointerState(_) => PacketType::PointerState,
            Self::Identify(_) => PacketType::Identify,
            Self::Identity(_) => PacketType::Identity,
        }
    }
}

/// Serializes one control datagram (header + message body).
pub fn encode_control(session_id: u32, msg_seq: u32, message: &ControlMessage) -> Vec<u8> {
    let mut body = Vec::with_capacity(64);
    match message {
        ControlMessage::Hello2(h) => {
            let name = truncated_name(&h.device_name);
            body.extend_from_slice(&[h.proto_min, h.proto_max]);
            body.extend_from_slice(&h.client_nonce.to_le_bytes());
            body.extend_from_slice(&h.listen_port.to_le_bytes());
            body.extend_from_slice(&h.decoder_caps.to_le_bytes());
            body.extend_from_slice(&h.feature_caps.to_le_bytes());
            body.extend_from_slice(&h.screen_px_w.to_le_bytes());
            body.extend_from_slice(&h.screen_px_h.to_le_bytes());
            body.extend_from_slice(&h.screen_pt_w.to_le_bytes());
            body.extend_from_slice(&h.screen_pt_h.to_le_bytes());
            body.push(h.refresh_hz);
            body.push(name.len() as u8);
            body.extend_from_slice(name.as_bytes());
            if h.device_id != 0
                || h.preferred_fps != 0
                || h.auth_token != [0; 16]
                || h.pairing_code != 0
            {
                body.extend_from_slice(&h.device_id.to_le_bytes());
                body.push(h.preferred_fps);
                body.extend_from_slice(&h.auth_token);
                body.extend_from_slice(&h.pairing_code.to_le_bytes());
            }
            if h.feature_caps & FEATURE_STANDBY_STATUS != 0 {
                body.resize(body.len().max(STANDBY_HELLO2_LEN - CONTROL_HEADER_SIZE), 0);
            }
        }
        ControlMessage::HelloAck(a) => {
            let name = truncated_name(&a.host_name);
            body.push(a.status as u8);
            body.push(a.accepted_version);
            body.extend_from_slice(&a.client_nonce.to_le_bytes());
            body.extend_from_slice(&a.session_id.to_le_bytes());
            body.extend_from_slice(&a.heartbeat_interval_ms.to_le_bytes());
            body.extend_from_slice(&a.report_interval_ms.to_le_bytes());
            body.extend_from_slice(&a.liveness_timeout_ms.to_le_bytes());
            let mut cfg = [0u8; STREAM_CONFIG_SIZE];
            a.stream_config.encode_into(&mut cfg);
            body.extend_from_slice(&cfg);
            body.push(name.len() as u8);
            body.extend_from_slice(name.as_bytes());
            let reason = truncated(&a.reason, MAX_REASON_LEN);
            if a.host_caps != 0 || a.auth_token != [0; 16] || !reason.is_empty() {
                body.extend_from_slice(&a.auth_token);
                body.extend_from_slice(&a.host_caps.to_le_bytes());
            }
            if !reason.is_empty() {
                body.push(reason.len() as u8);
                body.extend_from_slice(reason.as_bytes());
            }
        }
        ControlMessage::Heartbeat(hb) => {
            body.extend_from_slice(&hb.host_time_us.to_le_bytes());
            let mut cfg = [0u8; STREAM_CONFIG_SIZE];
            hb.stream_config.encode_into(&mut cfg);
            body.extend_from_slice(&cfg);
            if hb.notice != NOTICE_NONE || hb.frames_sent.is_some() {
                body.push(hb.notice);
            }
            if let Some(frames) = hb.frames_sent {
                body.extend_from_slice(&frames.to_le_bytes());
            }
        }
        ControlMessage::Bye(reason) => body.push(*reason as u8),
        ControlMessage::KeyframeRequest(k) => {
            body.extend_from_slice(&k.stream_epoch.to_le_bytes());
            body.extend_from_slice(&k.last_complete_seq.to_le_bytes());
            body.push(k.reason as u8);
            if k.max_media_dgram != 0 {
                body.extend_from_slice(&k.max_media_dgram.to_le_bytes());
            }
        }
        ControlMessage::Nack(n) => {
            n.validate().expect("invalid outgoing NACK");
            body.extend_from_slice(&n.stream_epoch.to_le_bytes());
            body.extend_from_slice(&n.frame_seq.to_le_bytes());
            body.extend_from_slice(&n.frag_count.to_le_bytes());
            body.push(n.missing.len() as u8);
            for index in &n.missing {
                body.extend_from_slice(&index.to_le_bytes());
            }
        }
        ControlMessage::ReceiverReport(r) => {
            body.extend_from_slice(&r.stream_epoch.to_le_bytes());
            body.extend_from_slice(&r.highest_seq.to_le_bytes());
            body.extend_from_slice(&r.frames_complete.to_le_bytes());
            body.extend_from_slice(&r.frames_dropped.to_le_bytes());
            body.extend_from_slice(&r.frags_received.to_le_bytes());
            body.extend_from_slice(&r.frags_lost.to_le_bytes());
            body.extend_from_slice(&r.jitter_us.to_le_bytes());
            body.extend_from_slice(&r.decode_fps_x10.to_le_bytes());
            body.push(r.assembler_depth);
            body.push(r.decode_depth);
            body.extend_from_slice(&r.e2e_latency_ms_x10.to_le_bytes());
            body.extend_from_slice(&r.rtt_ms_x10.to_le_bytes());
            if r.frags_repaired != 0
                || r.nacks_sent != 0
                || r.audio_packets_lost != 0
                || r.audio_buffer_ms != 0
                || r.pointer_shape_id != 0
            {
                body.extend_from_slice(&r.frags_repaired.to_le_bytes());
                body.extend_from_slice(&r.nacks_sent.to_le_bytes());
                body.extend_from_slice(&r.audio_packets_lost.to_le_bytes());
                body.extend_from_slice(&r.audio_buffer_ms.to_le_bytes());
            }
            if r.pointer_shape_id != 0 {
                body.extend_from_slice(&r.pointer_shape_id.to_le_bytes());
            }
        }
        ControlMessage::Ping(p) => body.extend_from_slice(&p.t1_us.to_le_bytes()),
        ControlMessage::Pong(p) => {
            body.extend_from_slice(&p.t1_us.to_le_bytes());
            body.extend_from_slice(&p.t2_us.to_le_bytes());
            body.extend_from_slice(&p.t3_us.to_le_bytes());
        }
        ControlMessage::StreamConfig(cfg) => {
            let mut block = [0u8; STREAM_CONFIG_SIZE];
            cfg.encode_into(&mut block);
            body.extend_from_slice(&block);
        }
        ControlMessage::Identify(i) => {
            body.push(i.version);
            body.extend_from_slice(&i.nonce);
            body.extend_from_slice(&[0; IDENTIFY_BODY_SIZE - 17]);
        }
        ControlMessage::Identity(i) => {
            let count = i.addresses.len().min(IDENTITY_MAX_ADDRESSES);
            body.push(i.version);
            body.extend_from_slice(&i.nonce);
            body.extend_from_slice(&i.port.to_le_bytes());
            body.push(count as u8);
            for slot in 0..IDENTITY_MAX_ADDRESSES {
                body.extend_from_slice(i.addresses.get(slot).unwrap_or(&[0; 4]));
            }
            body.extend_from_slice(&i.proof);
        }
        ControlMessage::InputEvent(e) => {
            body.push(e.input_ver);
            body.push(e.kind);
            body.push(e.phase);
            body.push(e.buttons);
            body.extend_from_slice(&e.event_id.to_le_bytes());
            body.extend_from_slice(&e.x_norm.to_le_bytes());
            body.extend_from_slice(&e.y_norm.to_le_bytes());
            body.extend_from_slice(&e.pressure_x1000.to_le_bytes());
            body.extend_from_slice(&e.scroll_dx.to_le_bytes());
            body.extend_from_slice(&e.scroll_dy.to_le_bytes());
            body.extend_from_slice(&e.keycode.to_le_bytes());
            body.push(e.modifiers);
            body.push(0); // reserved
            body.extend_from_slice(&e.client_time_us.to_le_bytes());
            if e.input_ver == 2 {
                body.extend_from_slice(&e.tilt_x.to_le_bytes());
                body.extend_from_slice(&e.tilt_y.to_le_bytes());
            }
        }
        ControlMessage::PointerShape(c) => {
            body.extend_from_slice(&[POINTER_VERSION, c.source_type, c.flags, 0]);
            body.extend_from_slice(&c.shape_id.to_le_bytes());
            body.extend_from_slice(&c.width.to_le_bytes());
            body.extend_from_slice(&c.height.to_le_bytes());
            body.extend_from_slice(&c.hot_x.to_le_bytes());
            body.extend_from_slice(&c.hot_y.to_le_bytes());
            body.extend_from_slice(&c.total_len.to_le_bytes());
            body.extend_from_slice(&c.offset.to_le_bytes());
            body.extend_from_slice(&(c.data.len() as u16).to_le_bytes());
            body.extend_from_slice(&c.data);
        }
        ControlMessage::PointerState(p) => {
            body.extend_from_slice(&[POINTER_VERSION, p.flags, 0, 0]);
            body.extend_from_slice(&p.shape_id.to_le_bytes());
            body.extend_from_slice(&p.x.to_le_bytes());
            body.extend_from_slice(&p.y.to_le_bytes());
            body.extend_from_slice(&p.host_time_us.to_le_bytes());
        }
    }

    let mut datagram = vec![0u8; CONTROL_HEADER_SIZE + body.len()];
    CommonPrefix {
        packet_type: message.packet_type(),
        flags: 0,
        payload_len: body.len() as u16,
    }
    .encode_into(&mut datagram);
    datagram[8..12].copy_from_slice(&session_id.to_le_bytes());
    datagram[12..16].copy_from_slice(&msg_seq.to_le_bytes());
    datagram[CONTROL_HEADER_SIZE..].copy_from_slice(&body);
    datagram
}

/// Parses one control datagram. Enforces the length invariant
/// (`payload_len == datagram_len - 16`) and every field-level invariant, but
/// ignores trailing body bytes beyond the fields it knows (append-only
/// evolution).
pub fn parse_control(datagram: &[u8]) -> Result<(ControlHeader, ControlMessage), WireError> {
    let prefix = CommonPrefix::decode(datagram)?;
    if datagram.len() < CONTROL_HEADER_SIZE {
        return Err(WireError::Truncated);
    }
    if usize::from(prefix.payload_len) != datagram.len() - CONTROL_HEADER_SIZE {
        return Err(WireError::LengthMismatch);
    }

    let header = ControlHeader {
        packet_type: prefix.packet_type,
        session_id: u32::from_le_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        msg_seq: u32::from_le_bytes([datagram[12], datagram[13], datagram[14], datagram[15]]),
    };
    let body = &datagram[CONTROL_HEADER_SIZE..];
    let mut r = Reader::new(body);

    let message = match prefix.packet_type {
        PacketType::Hello2 => {
            let proto_min = r.u8()?;
            let proto_max = r.u8()?;
            let client_nonce = r.u32()?;
            let listen_port = r.u16()?;
            let decoder_caps = r.u16()?;
            let feature_caps = r.u16()?;
            let screen_px_w = r.u16()?;
            let screen_px_h = r.u16()?;
            let screen_pt_w = r.u16()?;
            let screen_pt_h = r.u16()?;
            let refresh_hz = r.u8()?;
            let device_name = r.name()?;
            let (device_id, preferred_fps, auth_token, pairing_code) = if r.remaining() >= 29 {
                (r.u64()?, r.u8()?, r.take(16)?.try_into().unwrap(), r.u32()?)
            } else {
                (0, 0, [0; 16], 0)
            };
            if proto_min > proto_max {
                return Err(WireError::InvalidField("proto_range"));
            }
            ControlMessage::Hello2(Hello2 {
                proto_min,
                proto_max,
                client_nonce,
                listen_port,
                decoder_caps,
                feature_caps,
                screen_px_w,
                screen_px_h,
                screen_pt_w,
                screen_pt_h,
                refresh_hz,
                device_name,
                device_id,
                preferred_fps,
                auth_token,
                pairing_code,
            })
        }
        PacketType::HelloAck => {
            let status = HelloStatus::from_u8(r.u8()?).ok_or(WireError::InvalidField("status"))?;
            let accepted_version = r.u8()?;
            let client_nonce = r.u32()?;
            let session_id = r.u32()?;
            let heartbeat_interval_ms = r.u16()?;
            let report_interval_ms = r.u16()?;
            let liveness_timeout_ms = r.u16()?;
            let stream_config = StreamConfig::decode(r.take(STREAM_CONFIG_SIZE)?)?;
            let host_name = r.name()?;
            let (auth_token, host_caps) = if r.remaining() >= 18 {
                (r.take(16)?.try_into().unwrap(), r.u16()?)
            } else {
                ([0; 16], 0)
            };
            let reason = if r.remaining() > 0 {
                let len = usize::from(r.u8()?);
                if len > MAX_REASON_LEN {
                    return Err(WireError::InvalidField("reason_len"));
                }
                String::from_utf8(r.take(len)?.to_vec())
                    .map_err(|_| WireError::InvalidField("reason_utf8"))?
            } else {
                String::new()
            };
            if status == HelloStatus::Ok && session_id == 0 {
                return Err(WireError::InvalidField("session_id"));
            }
            ControlMessage::HelloAck(HelloAck {
                status,
                accepted_version,
                client_nonce,
                session_id,
                heartbeat_interval_ms,
                report_interval_ms,
                liveness_timeout_ms,
                stream_config,
                host_name,
                auth_token,
                host_caps,
                reason,
            })
        }
        PacketType::Heartbeat => ControlMessage::Heartbeat(Heartbeat {
            host_time_us: r.u64()?,
            stream_config: StreamConfig::decode(r.take(STREAM_CONFIG_SIZE)?)?,
            notice: if r.remaining() >= 1 {
                r.u8()?
            } else {
                NOTICE_NONE
            },
            frames_sent: if r.remaining() >= 4 {
                Some(r.u32()?)
            } else {
                None
            },
        }),
        PacketType::Bye => ControlMessage::Bye(
            ByeReason::from_u8(r.u8()?).ok_or(WireError::InvalidField("bye_reason"))?,
        ),
        PacketType::KeyframeRequest => ControlMessage::KeyframeRequest(KeyframeRequest {
            stream_epoch: r.u32()?,
            last_complete_seq: r.u32()?,
            reason: KeyframeReason::from_u8(r.u8()?)
                .ok_or(WireError::InvalidField("keyframe_reason"))?,
            max_media_dgram: if r.remaining() >= 2 { r.u16()? } else { 0 },
        }),
        PacketType::Nack => {
            let stream_epoch = r.u32()?;
            let frame_seq = r.u32()?;
            let frag_count = r.u16()?;
            let missing_len = usize::from(r.u8()?);
            if !(1..=64).contains(&missing_len) {
                return Err(WireError::InvalidField("missing_len"));
            }
            let mut missing = Vec::with_capacity(missing_len);
            for _ in 0..missing_len {
                missing.push(r.u16()?);
            }
            let nack = Nack {
                stream_epoch,
                frame_seq,
                frag_count,
                missing,
            };
            nack.validate()?;
            ControlMessage::Nack(nack)
        }
        PacketType::ReceiverReport => {
            let mut report = ReceiverReport {
                stream_epoch: r.u32()?,
                highest_seq: r.u32()?,
                frames_complete: r.u32()?,
                frames_dropped: r.u32()?,
                frags_received: r.u32()?,
                frags_lost: r.u32()?,
                jitter_us: r.u32()?,
                decode_fps_x10: r.u16()?,
                assembler_depth: r.u8()?,
                decode_depth: r.u8()?,
                e2e_latency_ms_x10: r.u16()?,
                rtt_ms_x10: r.u16()?,
                ..Default::default()
            };
            if r.remaining() >= 14 {
                report.frags_repaired = r.u32()?;
                report.nacks_sent = r.u32()?;
                report.audio_packets_lost = r.u32()?;
                report.audio_buffer_ms = r.u16()?;
            }
            if r.remaining() >= 4 {
                report.pointer_shape_id = r.u32()?;
            }
            ControlMessage::ReceiverReport(report)
        }
        PacketType::Ping => ControlMessage::Ping(Ping { t1_us: r.u64()? }),
        PacketType::Pong => ControlMessage::Pong(Pong {
            t1_us: r.u64()?,
            t2_us: r.u64()?,
            t3_us: r.u64()?,
        }),
        PacketType::StreamConfig => {
            ControlMessage::StreamConfig(StreamConfig::decode(r.take(STREAM_CONFIG_SIZE)?)?)
        }
        PacketType::InputEvent => {
            let input_ver = r.u8()?;
            ControlMessage::InputEvent(InputEvent {
                input_ver,
                kind: r.u8()?,
                phase: r.u8()?,
                buttons: r.u8()?,
                event_id: r.u32()?,
                x_norm: r.u16()?,
                y_norm: r.u16()?,
                pressure_x1000: r.u16()?,
                scroll_dx: r.i16()?,
                scroll_dy: r.i16()?,
                keycode: r.u16()?,
                modifiers: {
                    let m = r.u8()?;
                    let _reserved = r.u8()?;
                    m
                },
                client_time_us: r.u64()?,
                tilt_x: if input_ver == 2 { r.i16()? } else { 0 },
                tilt_y: if input_ver == 2 { r.i16()? } else { 0 },
            })
        }
        PacketType::PointerShape => {
            pointer_version(r.u8()?)?;
            let source_type = r.u8()?;
            let flags = r.u8()?;
            let _reserved = r.u8()?;
            let chunk = PointerShapeChunk {
                shape_id: r.u32()?,
                source_type,
                flags,
                width: r.u16()?,
                height: r.u16()?,
                hot_x: r.u16()?,
                hot_y: r.u16()?,
                total_len: r.u32()?,
                offset: r.u32()?,
                data: {
                    let len = usize::from(r.u16()?);
                    r.take(len)?.to_vec()
                },
            };
            if chunk.shape_id == 0 {
                return Err(WireError::InvalidField("shape_id"));
            }
            if !(1..=POINTER_MAX_SIDE).contains(&chunk.width)
                || !(1..=POINTER_MAX_SIDE).contains(&chunk.height)
            {
                return Err(WireError::InvalidField("pointer_size"));
            }
            if chunk.total_len as usize != pointer_shape_len(chunk.width, chunk.height, flags) {
                return Err(WireError::InvalidField("total_len"));
            }
            if chunk.data.is_empty()
                || chunk.offset as usize + chunk.data.len() > chunk.total_len as usize
            {
                return Err(WireError::InvalidField("offset"));
            }
            ControlMessage::PointerShape(chunk)
        }
        PacketType::PointerState => {
            pointer_version(r.u8()?)?;
            let flags = r.u8()?;
            let _reserved = r.u16()?;
            ControlMessage::PointerState(PointerState {
                flags,
                shape_id: r.u32()?,
                x: r.i32()?,
                y: r.i32()?,
                host_time_us: r.u64()?,
            })
        }
        PacketType::Identify => {
            let version = r.u8()?;
            let nonce = r.take(16)?.try_into().unwrap();
            r.take(IDENTIFY_BODY_SIZE - 17)?;
            ControlMessage::Identify(Identify { version, nonce })
        }
        PacketType::Identity => {
            let version = r.u8()?;
            let nonce = r.take(16)?.try_into().unwrap();
            let port = r.u16()?;
            let count = usize::from(r.u8()?);
            if count > IDENTITY_MAX_ADDRESSES {
                return Err(WireError::InvalidField("identity_addresses"));
            }
            let slots = r.take(4 * IDENTITY_MAX_ADDRESSES)?;
            let addresses = slots.as_chunks::<4>().0[..count].to_vec();
            ControlMessage::Identity(Identity {
                version,
                nonce,
                port,
                addresses,
                proof: r.take(32)?.try_into().unwrap(),
            })
        }
        PacketType::Media | PacketType::MediaFec | PacketType::Audio | PacketType::Error => {
            return Err(WireError::InvalidField("packet_type"));
        }
    };

    Ok((header, message))
}

/// A pointer body of another layout version is not one this parser knows.
fn pointer_version(version: u8) -> Result<(), WireError> {
    if version == POINTER_VERSION {
        Ok(())
    } else {
        Err(WireError::InvalidField("pointer_version"))
    }
}

fn truncated_name(name: &str) -> &str {
    truncated(name, MAX_NAME_LEN)
}

fn truncated(name: &str, max: usize) -> &str {
    if name.len() <= max {
        return name;
    }
    // Truncate on a char boundary at or below the cap.
    let mut end = max;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// Bounds-checked little-endian body reader. Every read either returns the
/// value or `Truncated` — indexing can never panic.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.at
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let slice = self
            .data
            .get(self.at..self.at + len)
            .ok_or(WireError::Truncated)?;
        self.at += len;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Result<i16, WireError> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, WireError> {
        Ok(self.u32()? as i32)
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Length-prefixed UTF-8 string with the [`MAX_NAME_LEN`] cap.
    fn name(&mut self) -> Result<String, WireError> {
        let len = usize::from(self.u8()?);
        if len > MAX_NAME_LEN {
            return Err(WireError::InvalidField("name_len"));
        }
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::InvalidField("name_utf8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nack_bytes() -> Vec<u8> {
        encode_control(
            7,
            1,
            &ControlMessage::Nack(Nack {
                stream_epoch: 9,
                frame_seq: 42,
                frag_count: 8,
                missing: vec![0, 2, 7],
            }),
        )
    }

    #[test]
    fn nack_round_trip_and_sixty_four_index_limit() {
        let bytes = nack_bytes();
        let (header, message) = parse_control(&bytes).unwrap();
        assert_eq!(header.packet_type, PacketType::Nack);
        assert_eq!(
            message,
            ControlMessage::Nack(Nack {
                stream_epoch: 9,
                frame_seq: 42,
                frag_count: 8,
                missing: vec![0, 2, 7],
            })
        );
        let maximum = ControlMessage::Nack(Nack {
            stream_epoch: 1,
            frame_seq: 2,
            frag_count: 64,
            missing: (0..64).collect(),
        });
        assert_eq!(
            parse_control(&encode_control(7, 1, &maximum)).unwrap().1,
            maximum
        );
    }

    #[test]
    fn nack_rejects_empty_oversized_unsorted_duplicate_and_out_of_range_indices() {
        for length in [0, 65, 255] {
            let mut bytes = nack_bytes();
            bytes[CONTROL_HEADER_SIZE + 10] = length;
            assert_eq!(
                parse_control(&bytes),
                Err(WireError::InvalidField("missing_len"))
            );
        }
        for indices in [[2u16, 0, 7], [0, 0, 7], [0, 2, 8]] {
            let mut bytes = nack_bytes();
            for (i, value) in indices.into_iter().enumerate() {
                bytes[27 + i * 2..29 + i * 2].copy_from_slice(&value.to_le_bytes());
            }
            assert_eq!(
                parse_control(&bytes),
                Err(WireError::InvalidField("missing"))
            );
        }
    }

    #[test]
    fn nack_rejects_short_bodies_even_with_matching_prefix_length() {
        let bytes = nack_bytes();
        for length in CONTROL_HEADER_SIZE..bytes.len() {
            let mut truncated = bytes[..length].to_vec();
            truncated[6..8].copy_from_slice(&((length - CONTROL_HEADER_SIZE) as u16).to_le_bytes());
            assert_eq!(parse_control(&truncated), Err(WireError::Truncated));
        }
    }

    #[test]
    fn old_control_bodies_default_new_tails() {
        for message in all_messages() {
            let (_, decoded) = parse_control(&encode_control(7, 1, &message)).unwrap();
            match decoded {
                ControlMessage::Hello2(hello) => {
                    assert_eq!(hello.device_id, 0);
                    assert_eq!(hello.preferred_fps, 0);
                    assert_eq!(hello.auth_token, [0; 16]);
                    assert_eq!(hello.pairing_code, 0);
                }
                ControlMessage::HelloAck(ack) => {
                    assert_eq!(ack.auth_token, [0; 16]);
                    assert_eq!(ack.host_caps, 0);
                }
                ControlMessage::ReceiverReport(report) => {
                    assert_eq!(report.frags_repaired, 0);
                    assert_eq!(report.nacks_sent, 0);
                    assert_eq!(report.audio_packets_lost, 0);
                    assert_eq!(report.audio_buffer_ms, 0);
                    assert_eq!(report.pointer_shape_id, 0);
                }
                _ => {}
            }
        }
    }

    fn sample_config() -> StreamConfig {
        StreamConfig {
            stream_epoch: 3,
            width: 2560,
            height: 1440,
            fps: 60,
            codec: CODEC_H264,
            flags: 0,
            bitrate_bps: 15_000_000,
        }
    }

    fn all_messages() -> Vec<ControlMessage> {
        vec![
            ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 0xDEAD_BEEF,
                listen_port: 9876,
                decoder_caps: CAP_DECODE_H264 | CAP_DECODE_HEVC,
                feature_caps: FEATURE_WANTS_INPUT,
                screen_px_w: 2420,
                screen_px_h: 1668,
                screen_pt_w: 1210,
                screen_pt_h: 834,
                refresh_hz: 120,
                device_name: "Ali's iPad Pro".to_string(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
            ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Ok,
                accepted_version: 2,
                client_nonce: 0xDEAD_BEEF,
                session_id: 0x1234_5678,
                heartbeat_interval_ms: 1000,
                report_interval_ms: 500,
                liveness_timeout_ms: 3000,
                stream_config: sample_config(),
                host_name: "ALI-PC".to_string(),
                auth_token: [0; 16],
                host_caps: 0,
                reason: String::new(),
            }),
            ControlMessage::Heartbeat(Heartbeat {
                host_time_us: 987_654_321,
                stream_config: sample_config(),
                notice: NOTICE_NONE,
                frames_sent: None,
            }),
            ControlMessage::Heartbeat(Heartbeat {
                host_time_us: 987_654_321,
                stream_config: sample_config(),
                notice: NOTICE_NONE,
                frames_sent: Some(0),
            }),
            ControlMessage::Bye(ByeReason::AppBackground),
            ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: 3,
                last_complete_seq: 4111,
                reason: KeyframeReason::GapLoss,
                max_media_dgram: 0,
            }),
            ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: 3,
                last_complete_seq: 0,
                reason: KeyframeReason::Startup,
                max_media_dgram: 1200,
            }),
            ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 3,
                highest_seq: 5000,
                frames_complete: 4990,
                frames_dropped: 10,
                frags_received: 120_000,
                frags_lost: 250,
                jitter_us: 2_100,
                decode_fps_x10: 599,
                assembler_depth: 1,
                decode_depth: 0,
                e2e_latency_ms_x10: 321,
                rtt_ms_x10: 28,
                ..Default::default()
            }),
            ControlMessage::Ping(Ping { t1_us: 42 }),
            ControlMessage::Pong(Pong {
                t1_us: 42,
                t2_us: 43,
                t3_us: 44,
            }),
            ControlMessage::StreamConfig(sample_config()),
            ControlMessage::Identify(Identify {
                version: IDENTIFY_VERSION,
                nonce: [7; 16],
            }),
            ControlMessage::Identity(Identity {
                version: IDENTIFY_VERSION,
                nonce: [7; 16],
                port: 9876,
                addresses: vec![[192, 0, 2, 7], [198, 51, 100, 7]],
                proof: [9; 32],
            }),
            ControlMessage::InputEvent(InputEvent {
                input_ver: 1,
                kind: 0,
                phase: 1,
                buttons: 1,
                event_id: 77,
                x_norm: 32768,
                y_norm: 16384,
                pressure_x1000: 500,
                scroll_dx: -3,
                scroll_dy: 12,
                keycode: 0,
                modifiers: 0,
                client_time_us: 1_000_001,
                tilt_x: 0,
                tilt_y: 0,
            }),
            ControlMessage::PointerShape(PointerShapeChunk {
                shape_id: 7,
                source_type: 1,
                flags: POINTER_SHAPE_INVERT,
                width: 2,
                height: 2,
                hot_x: 1,
                hot_y: 0,
                total_len: 18,
                offset: 16,
                data: vec![0x80, 0x40],
            }),
            ControlMessage::PointerState(PointerState {
                flags: POINTER_VISIBLE,
                shape_id: 7,
                x: -3,
                y: 1439,
                host_time_us: 5_000_001,
            }),
        ]
    }

    fn shape_chunk() -> PointerShapeChunk {
        all_messages()
            .into_iter()
            .find_map(|message| match message {
                ControlMessage::PointerShape(chunk) => Some(chunk),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn pointer_shape_chunks_cover_the_image_in_order_within_a_datagram() {
        let (width, height) = (64u16, 48u16);
        let bytes: Vec<u8> = (0..pointer_shape_len(width, height, POINTER_SHAPE_INVERT))
            .map(|i| i as u8)
            .collect();
        let chunks =
            pointer_shape_chunks(9, 1, POINTER_SHAPE_INVERT, (width, height), (3, 4), &bytes);
        assert_eq!(chunks.len(), bytes.len().div_ceil(POINTER_CHUNK_MAX));
        let mut joined = Vec::new();
        for chunk in &chunks {
            assert_eq!(chunk.offset as usize, joined.len());
            let datagram = encode_control(1, 1, &ControlMessage::PointerShape(chunk.clone()));
            assert!(datagram.len() <= super::super::MAX_DGRAM_SIZE);
            let (_, parsed) = parse_control(&datagram).unwrap();
            assert_eq!(parsed, ControlMessage::PointerShape(chunk.clone()));
            joined.extend_from_slice(&chunk.data);
        }
        assert_eq!(joined, bytes);
    }

    #[test]
    fn pointer_shapes_reject_a_zero_id_bad_sizes_lengths_and_versions() {
        let mut cases: Vec<(PointerShapeChunk, &str)> = Vec::new();
        let mut zero = shape_chunk();
        zero.shape_id = 0;
        cases.push((zero, "shape_id"));
        for (w, h) in [(0, 2), (2, 0), (POINTER_MAX_SIDE + 1, 2)] {
            let mut sized = shape_chunk();
            (sized.width, sized.height) = (w, h);
            cases.push((sized, "pointer_size"));
        }
        let mut no_mask = shape_chunk();
        no_mask.flags = 0;
        cases.push((no_mask, "total_len"));
        let mut past_end = shape_chunk();
        past_end.offset = 17;
        cases.push((past_end, "offset"));
        let mut empty = shape_chunk();
        empty.data.clear();
        cases.push((empty, "offset"));
        for (chunk, field) in cases {
            let datagram = encode_control(1, 1, &ControlMessage::PointerShape(chunk));
            assert_eq!(
                parse_control(&datagram),
                Err(WireError::InvalidField(field))
            );
        }
        for message in [
            ControlMessage::PointerShape(shape_chunk()),
            ControlMessage::PointerState(PointerState::default()),
        ] {
            let mut datagram = encode_control(1, 1, &message);
            datagram[CONTROL_HEADER_SIZE] = POINTER_VERSION + 1;
            assert_eq!(
                parse_control(&datagram),
                Err(WireError::InvalidField("pointer_version"))
            );
        }
    }

    #[test]
    fn a_report_names_the_pointer_shape_after_the_older_tail() {
        let report = ReceiverReport {
            stream_epoch: 3,
            pointer_shape_id: 0x0102_0304,
            ..Default::default()
        };
        let datagram = encode_control(1, 1, &ControlMessage::ReceiverReport(report));
        assert_eq!(
            datagram.len(),
            CONTROL_HEADER_SIZE + RECEIVER_REPORT_SIZE + 14 + 4
        );
        assert_eq!(
            parse_control(&datagram).unwrap().1,
            ControlMessage::ReceiverReport(report)
        );
        // A parser that stops after the older tail reads the same report
        // without the id: the tail is appended, so nothing before it moved.
        let mut older = datagram[..datagram.len() - 4].to_vec();
        let body_len = (older.len() - CONTROL_HEADER_SIZE) as u16;
        older[6..8].copy_from_slice(&body_len.to_le_bytes());
        let ControlMessage::ReceiverReport(old) = parse_control(&older).unwrap().1 else {
            unreachable!()
        };
        assert_eq!(
            old,
            ReceiverReport {
                pointer_shape_id: 0,
                ..report
            }
        );
    }

    #[test]
    fn every_message_round_trips() {
        for message in all_messages() {
            let session_id = if matches!(message, ControlMessage::Hello2(_)) {
                0
            } else {
                0x1234_5678
            };
            let datagram = encode_control(session_id, 9, &message);
            let (header, parsed) = parse_control(&datagram)
                .unwrap_or_else(|e| panic!("{message:?} failed to parse: {e}"));
            assert_eq!(header.session_id, session_id);
            assert_eq!(header.msg_seq, 9);
            assert_eq!(parsed, message, "round-trip mismatch");
        }
    }

    #[test]
    fn every_truncation_of_every_message_is_rejected() {
        for message in all_messages() {
            let datagram = encode_control(1, 1, &message);
            for len in 0..datagram.len() {
                assert!(
                    parse_control(&datagram[..len]).is_err(),
                    "{message:?} truncated to {len}/{} bytes must not parse",
                    datagram.len()
                );
            }
        }
    }

    #[test]
    fn a_heartbeat_notice_is_appended_only_when_there_is_one() {
        let heartbeat = |notice| {
            ControlMessage::Heartbeat(Heartbeat {
                host_time_us: 987_654_321,
                stream_config: sample_config(),
                notice,
                frames_sent: None,
            })
        };
        // No notice: the bytes every earlier iPad already reads.
        let plain = encode_control(7, 1, &heartbeat(NOTICE_NONE));
        assert_eq!(plain.len(), CONTROL_HEADER_SIZE + 8 + STREAM_CONFIG_SIZE);
        // A notice is one byte more. Such a body, cut back to the old length
        // as an older iPad reads it, is the plain heartbeat; an unknown value
        // reaches the iPad as it is, for it to ignore.
        for notice in [NOTICE_PC_LOCKED, NOTICE_NO_DISPLAY, 0xC8] {
            let datagram = encode_control(7, 1, &heartbeat(notice));
            assert_eq!(datagram.len(), plain.len() + 1);
            assert_eq!(datagram[..plain.len()][8..], plain[8..]);
            assert_eq!(parse_control(&datagram).unwrap().1, heartbeat(notice));
            for len in 0..datagram.len() {
                assert!(parse_control(&datagram[..len]).is_err());
            }
        }
        assert_eq!(parse_control(&plain).unwrap().1, heartbeat(NOTICE_NONE));
    }

    /// The media size fields are appended: without them the bytes are the
    /// ones every earlier PC and iPad reads, and a body cut back to the old
    /// length, as an older parser reads it, is the request without them.
    #[test]
    fn the_media_size_fields_are_appended_only_when_set() {
        let request = |max_media_dgram| {
            ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: 3,
                last_complete_seq: 0,
                reason: KeyframeReason::Startup,
                max_media_dgram,
            })
        };
        let plain = encode_control(7, 1, &request(0));
        assert_eq!(plain.len(), CONTROL_HEADER_SIZE + 9);
        let sized = encode_control(7, 1, &request(576));
        assert_eq!(sized.len(), plain.len() + 2);
        assert_eq!(sized[8..plain.len()], plain[8..]);
        assert_eq!(&sized[plain.len()..], &576u16.to_le_bytes());
        assert_eq!(parse_control(&sized).unwrap().1, request(576));
        assert_eq!(parse_control(&plain).unwrap().1, request(0));

        let heartbeat = |notice, frames_sent| {
            ControlMessage::Heartbeat(Heartbeat {
                host_time_us: 987_654_321,
                stream_config: sample_config(),
                notice,
                frames_sent,
            })
        };
        let plain = encode_control(7, 1, &heartbeat(NOTICE_NONE, None));
        // The count needs the notice byte before it, even with no notice.
        let counted = encode_control(7, 1, &heartbeat(NOTICE_NONE, Some(0x0102_0304)));
        assert_eq!(counted.len(), plain.len() + 5);
        assert_eq!(counted[8..plain.len()], plain[8..]);
        assert_eq!(&counted[plain.len()..], &[NOTICE_NONE, 4, 3, 2, 1]);
        assert_eq!(
            parse_control(&counted).unwrap().1,
            heartbeat(NOTICE_NONE, Some(0x0102_0304))
        );
        let locked = encode_control(7, 1, &heartbeat(NOTICE_PC_LOCKED, Some(9)));
        assert_eq!(
            parse_control(&locked).unwrap().1,
            heartbeat(NOTICE_PC_LOCKED, Some(9))
        );
    }

    #[test]
    fn trailing_bytes_are_ignored_for_forward_compat() {
        let mut datagram = encode_control(1, 1, &ControlMessage::Ping(Ping { t1_us: 5 }));
        datagram.extend_from_slice(&[0xAA, 0xBB]);
        // payload_len must still match, so a forward-compatible sender bumps it.
        let new_len = (datagram.len() - CONTROL_HEADER_SIZE) as u16;
        datagram[6..8].copy_from_slice(&new_len.to_le_bytes());
        let (_, parsed) = parse_control(&datagram).expect("extended body must parse");
        assert_eq!(parsed, ControlMessage::Ping(Ping { t1_us: 5 }));
    }

    #[test]
    fn invalid_enums_and_names_are_rejected() {
        // Unknown bye reason.
        let mut bye = encode_control(1, 1, &ControlMessage::Bye(ByeReason::Error));
        *bye.last_mut().unwrap() = 200;
        assert_eq!(
            parse_control(&bye),
            Err(WireError::InvalidField("bye_reason"))
        );

        // HELLO_ACK with Ok status but zero session id.
        let ack = ControlMessage::HelloAck(HelloAck {
            status: HelloStatus::Ok,
            accepted_version: 2,
            client_nonce: 1,
            session_id: 0,
            heartbeat_interval_ms: 1000,
            report_interval_ms: 500,
            liveness_timeout_ms: 3000,
            stream_config: sample_config(),
            host_name: String::new(),
            auth_token: [0; 16],
            host_caps: 0,
            reason: String::new(),
        });
        // encode_control would happily serialize it; the parser must reject.
        let datagram = encode_control(0, 1, &ack);
        assert_eq!(
            parse_control(&datagram),
            Err(WireError::InvalidField("session_id"))
        );

        // Oversized name length byte.
        let hello = encode_control(
            0,
            1,
            &ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: 9876,
                decoder_caps: CAP_DECODE_H264,
                feature_caps: 0,
                screen_px_w: 100,
                screen_px_h: 100,
                screen_pt_w: 100,
                screen_pt_h: 100,
                refresh_hz: 60,
                device_name: "x".to_string(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
        );
        let name_len_at = CONTROL_HEADER_SIZE + 21;
        let mut bad = hello.clone();
        bad[name_len_at] = (MAX_NAME_LEN + 1) as u8;
        assert_eq!(
            parse_control(&bad),
            Err(WireError::InvalidField("name_len"))
        );
    }

    #[test]
    fn long_device_names_are_truncated_on_char_boundary() {
        let long_name = "é".repeat(60); // 120 UTF-8 bytes
        let datagram = encode_control(
            0,
            1,
            &ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: 9876,
                decoder_caps: CAP_DECODE_H264,
                feature_caps: 0,
                screen_px_w: 1,
                screen_px_h: 1,
                screen_pt_w: 1,
                screen_pt_h: 1,
                refresh_hz: 60,
                device_name: long_name,
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
        );
        let (_, parsed) = parse_control(&datagram).unwrap();
        let ControlMessage::Hello2(hello) = parsed else {
            panic!("wrong type");
        };
        assert!(hello.device_name.len() <= MAX_NAME_LEN);
        assert_eq!(hello.device_name, "é".repeat(32)); // 64 bytes exactly
    }

    #[test]
    fn random_bytes_never_panic() {
        // Deterministic pseudo-random fuzz (no rand dep): xorshift over lengths/bytes.
        let mut state = 0x1234_5678_9ABC_DEFFu64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 80) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
            let _ = parse_control(&bytes); // must not panic
            let _ = super::super::media::MediaHeader::decode(&bytes);
            let _ = super::super::classify(&bytes);
        }
    }

    fn standby_ack(status: HelloStatus, reason: &str) -> HelloAck {
        HelloAck {
            status,
            accepted_version: 2,
            client_nonce: 9,
            session_id: 0,
            heartbeat_interval_ms: 1000,
            report_interval_ms: 500,
            liveness_timeout_ms: 3000,
            stream_config: StreamConfig::default(),
            host_name: "PC".to_string(),
            auth_token: [0; 16],
            host_caps: 0,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn not_streaming_statuses_and_their_reason_round_trip() {
        for status in [
            HelloStatus::Stopped,
            HelloStatus::Restarting,
            HelloStatus::Failed,
        ] {
            for reason in ["", "display_changing"] {
                let ack = standby_ack(status, reason);
                let bytes = encode_control(0, 1, &ControlMessage::HelloAck(ack.clone()));
                assert_eq!(
                    parse_control(&bytes).unwrap().1,
                    ControlMessage::HelloAck(ack)
                );
            }
        }
        // A reason past the cap is cut, so it always parses.
        let long = "x".repeat(100);
        let bytes = encode_control(
            0,
            1,
            &ControlMessage::HelloAck(standby_ack(HelloStatus::Failed, &long)),
        );
        let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&bytes) else {
            panic!("a long reason must still parse");
        };
        assert_eq!(ack.reason.len(), MAX_REASON_LEN);
    }

    /// 0.3.4 and older know statuses 0-5 only and drop the whole ACK for
    /// any other, like a lost datagram: the host must not send them 6-8.
    #[test]
    fn a_status_past_the_known_ones_is_rejected() {
        let mut bytes = encode_control(
            0,
            1,
            &ControlMessage::HelloAck(standby_ack(HelloStatus::Error, "")),
        );
        bytes[CONTROL_HEADER_SIZE] = 9;
        assert_eq!(
            parse_control(&bytes),
            Err(WireError::InvalidField("status"))
        );
    }

    #[test]
    fn failed_reason_tokens_are_short_distinct_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for reason in FailedReason::ALL {
            let token = reason.token();
            assert!(token.len() <= MAX_REASON_LEN && token.is_ascii(), "{token}");
            assert!(seen.insert(token), "{token} twice");
            assert_eq!(FailedReason::from_token(token), Some(reason));
        }
        assert_eq!(FailedReason::from_token("something new"), None);
    }

    fn standby_hello2(feature_caps: u16, device_name: &str) -> Vec<u8> {
        encode_control(
            0,
            1,
            &ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 7,
                listen_port: 0,
                decoder_caps: CAP_DECODE_H264,
                feature_caps,
                screen_px_w: 1,
                screen_px_h: 1,
                screen_pt_w: 1,
                screen_pt_h: 1,
                refresh_hz: 60,
                device_name: device_name.into(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
        )
    }

    /// Any answer fits in a HELLO2 that asks for standby answers: the
    /// longest ACK there is, with a 64-byte host name, a token and the
    /// longest reason, is no larger. The padding parses as the HELLO2 it was.
    #[test]
    fn a_hello2_that_asks_for_standby_answers_fits_any_ack() {
        let mut longest = standby_ack(HelloStatus::Failed, &"r".repeat(MAX_REASON_LEN));
        longest.host_name = "h".repeat(MAX_NAME_LEN);
        longest.auth_token = [9; 16];
        longest.host_caps = 0xFFFF;
        let ack = encode_control(0, 1, &ControlMessage::HelloAck(longest));
        assert_eq!(ack.len(), MAX_HELLO_ACK_LEN);
        for name in ["", "iPad", &"n".repeat(MAX_NAME_LEN)] {
            let padded = standby_hello2(FEATURE_STANDBY_STATUS, name);
            assert!(
                padded.len() >= ack.len(),
                "{} < {}",
                padded.len(),
                ack.len()
            );
            let Ok((_, ControlMessage::Hello2(hello))) = parse_control(&padded) else {
                panic!("a padded HELLO2 must parse");
            };
            assert_eq!(hello.device_name, name);
            assert_eq!((hello.device_id, hello.auth_token), (0, [0; 16]));
        }
        assert_eq!(
            standby_hello2(FEATURE_STANDBY_STATUS, "").len(),
            STANDBY_HELLO2_LEN
        );
        // Without the bit, as from 0.3.4, nothing is added.
        assert_eq!(standby_hello2(0, "").len(), CONTROL_HEADER_SIZE + 22);
    }
}
