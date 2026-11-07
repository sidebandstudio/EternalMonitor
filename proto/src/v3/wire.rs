//! v3 byte formats: the two handshake datagrams, the payloads Noise carries
//! inside them, the slot status, and the session messages sealed in records.
//!
//! Every parser is total: it returns a value or a [`WireError`], never panics,
//! and never reads past its input. Encrypted payloads are append-only like v2
//! control bodies: decoders ignore trailing bytes they don't understand. The
//! cleartext handshake framing is exact: no trailing bytes.

use super::{Id16, EXT_SESSION, HANDSHAKE2_TYPE, PROTOCOL_VERSION};
use crate::v2::control::{StreamConfig, CONTROL_HEADER_SIZE, MAX_NAME_LEN};
use crate::v2::{CommonPrefix, PacketType, WireError, MAGIC, PREFIX_SIZE, VERSION};

/// HELLO2 shell body: 22 fixed bytes with an empty name, then the 29-byte tail.
pub const HELLO2_SHELL_SIZE: usize = 51;
/// Extension tag + length.
pub const EXTENSION_HEADER_SIZE: usize = 4;
/// Offset of the extension in message 1.
pub const MSG1_EXTENSION_OFFSET: usize = CONTROL_HEADER_SIZE + HELLO2_SHELL_SIZE;
/// Noise `e` plus the AEAD tag on the payload.
pub const NOISE_OVERHEAD: usize = 32 + 16;
/// The largest Noise message either side accepts. Comfortably above both
/// message sizes, far below the 65,535 Noise limit.
pub const MAX_NOISE_MESSAGE: usize = 1024;

/// Message 1 as it arrives, before any cryptography.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg1<'a> {
    /// From the shell. Unauthenticated; only the version gate uses it.
    pub client_nonce: u32,
    pub dev_id: Id16,
    pub noise: &'a [u8],
}

/// Builds message 1 around a Noise message.
pub fn encode_msg1(client_nonce: u32, listen_port: u16, dev_id: &Id16, noise: &[u8]) -> Vec<u8> {
    let ext_len = 16 + noise.len();
    let body_len = HELLO2_SHELL_SIZE + EXTENSION_HEADER_SIZE + ext_len;
    let mut out = Vec::with_capacity(CONTROL_HEADER_SIZE + body_len);
    out.resize(CONTROL_HEADER_SIZE, 0);
    CommonPrefix {
        packet_type: PacketType::Hello2,
        flags: 0,
        payload_len: u16::try_from(body_len).expect("message 1 fits a datagram"),
    }
    .encode_into(&mut out);
    out[8..12].copy_from_slice(&0u32.to_le_bytes());
    out[12..16].copy_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&[PROTOCOL_VERSION, PROTOCOL_VERSION]);
    out.extend_from_slice(&client_nonce.to_le_bytes());
    out.extend_from_slice(&listen_port.to_le_bytes());
    // decoder_caps, feature_caps, four screen fields, refresh_hz, name_len,
    // then the 29-byte tail: 14 + 29 zero bytes.
    out.resize(out.len() + 14 + 29, 0);
    out.extend_from_slice(&EXT_SESSION.to_le_bytes());
    out.extend_from_slice(&(ext_len as u16).to_le_bytes());
    out.extend_from_slice(dev_id);
    out.extend_from_slice(noise);
    out
}

/// Parses message 1. Accepts only the exact shell [`encode_msg1`] writes, so
/// one HELLO2 can never be read two ways.
pub fn parse_msg1(datagram: &[u8]) -> Result<Msg1<'_>, WireError> {
    let body = control_body(datagram, PacketType::Hello2 as u8)?;
    let mut r = Reader::new(body);
    let shell = r.take(HELLO2_SHELL_SIZE)?;
    if shell[0] != PROTOCOL_VERSION || shell[1] != PROTOCOL_VERSION {
        return Err(WireError::InvalidField("proto_range"));
    }
    let client_nonce = u32::from_le_bytes([shell[2], shell[3], shell[4], shell[5]]);
    if shell[8..].iter().any(|&b| b != 0) {
        return Err(WireError::InvalidField("shell"));
    }
    if datagram[8..16] != [0, 0, 0, 0, 1, 0, 0, 0] {
        return Err(WireError::InvalidField("header"));
    }
    let ext = extension(&mut r)?;
    let mut e = Reader::new(ext);
    let dev_id = e.array16()?;
    let noise = e.rest();
    check_noise_len(noise)?;
    Ok(Msg1 {
        client_nonce,
        dev_id,
        noise,
    })
}

/// Builds message 2 around a Noise message.
pub fn encode_msg2(noise: &[u8]) -> Vec<u8> {
    let body_len = EXTENSION_HEADER_SIZE + noise.len();
    let mut out = vec![0u8; CONTROL_HEADER_SIZE];
    encode_raw_prefix(&mut out, HANDSHAKE2_TYPE, body_len);
    out.extend_from_slice(&EXT_SESSION.to_le_bytes());
    out.extend_from_slice(&(noise.len() as u16).to_le_bytes());
    out.extend_from_slice(noise);
    out
}

/// Parses message 2 and returns its Noise message.
pub fn parse_msg2(datagram: &[u8]) -> Result<&[u8], WireError> {
    let body = control_body(datagram, HANDSHAKE2_TYPE)?;
    if datagram[8..16] != [0; 8] {
        return Err(WireError::InvalidField("header"));
    }
    let mut r = Reader::new(body);
    let noise = extension(&mut r)?;
    check_noise_len(noise)?;
    Ok(noise)
}

/// The kind of a v3 datagram, from its first bytes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3Kind {
    /// A HELLO2 asking for protocol 3: hand to [`parse_msg1`].
    Handshake1,
    Handshake2,
    Sealed,
    /// Anything else, including v2 HELLO2s and v2 control traffic.
    Other,
}

/// Routes a datagram without copying or parsing beyond the first 18 bytes.
pub fn classify(datagram: &[u8]) -> V3Kind {
    if datagram.len() < PREFIX_SIZE
        || datagram[0..2] != MAGIC.to_le_bytes()
        || datagram[2] != VERSION
    {
        return V3Kind::Other;
    }
    match datagram[3] {
        t if t == super::SEALED_TYPE => V3Kind::Sealed,
        HANDSHAKE2_TYPE => V3Kind::Handshake2,
        t if t == PacketType::Hello2 as u8
            && datagram.len() > CONTROL_HEADER_SIZE + 1
            && datagram[CONTROL_HEADER_SIZE + 1] >= PROTOCOL_VERSION =>
        {
            V3Kind::Handshake1
        }
        _ => V3Kind::Other,
    }
}

/// The encrypted payload of message 1: a timestamp and the session fields
/// HELLO2 carries in v2. No credential travels here; the handshake itself
/// proves the key.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelloPayload {
    /// Strictly increasing per host on the iPad: `max(clock µs, last + 1)`.
    pub timestamp_us: u64,
    pub listen_port: u16,
    pub decoder_caps: u16,
    pub feature_caps: u16,
    pub screen_px_w: u16,
    pub screen_px_h: u16,
    pub screen_pt_w: u16,
    pub screen_pt_h: u16,
    pub refresh_hz: u8,
    pub preferred_fps: u8,
    pub device_id: u64,
    pub device_name: String,
}

impl HelloPayload {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40 + self.device_name.len());
        out.extend_from_slice(&self.timestamp_us.to_le_bytes());
        for value in [
            self.listen_port,
            self.decoder_caps,
            self.feature_caps,
            self.screen_px_w,
            self.screen_px_h,
            self.screen_pt_w,
            self.screen_pt_h,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.push(self.refresh_hz);
        out.push(self.preferred_fps);
        out.extend_from_slice(&self.device_id.to_le_bytes());
        push_name(&mut out, &self.device_name);
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes);
        Ok(Self {
            timestamp_us: r.u64()?,
            listen_port: r.u16()?,
            decoder_caps: r.u16()?,
            feature_caps: r.u16()?,
            screen_px_w: r.u16()?,
            screen_px_h: r.u16()?,
            screen_pt_w: r.u16()?,
            screen_pt_h: r.u16()?,
            refresh_hz: r.u8()?,
            preferred_fps: r.u8()?,
            device_id: r.u64()?,
            device_name: r.name()?,
        })
    }
}

/// State of a credential record the host holds for a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordState {
    Pending = 0,
    Active = 1,
}

/// Records a slot may list: 1 active and 3 pending.
pub const MAX_HELD: usize = 4;
/// Tombstones a slot keeps.
pub const MAX_TOMBSTONES: usize = 8;
/// Largest encoded status: 1 + 4 × 17 + 1 + 8 × 16.
pub const MAX_SLOT_STATUS_SIZE: usize = 198;

/// What the host holds for one slot, and which keys it has retired.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlotStatus {
    pub held: Vec<(Id16, RecordState)>,
    pub tombstones: Vec<Id16>,
}

impl SlotStatus {
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        assert!(self.held.len() <= MAX_HELD && self.tombstones.len() <= MAX_TOMBSTONES);
        out.push(self.held.len() as u8);
        for (dev_id, state) in &self.held {
            out.extend_from_slice(dev_id);
            out.push(*state as u8);
        }
        out.push(self.tombstones.len() as u8);
        for dev_id in &self.tombstones {
            out.extend_from_slice(dev_id);
        }
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, WireError> {
        let held_count = usize::from(r.u8()?);
        if held_count > MAX_HELD {
            return Err(WireError::InvalidField("held_count"));
        }
        let mut held = Vec::with_capacity(held_count);
        for _ in 0..held_count {
            let dev_id = r.array16()?;
            let state = match r.u8()? {
                0 => RecordState::Pending,
                1 => RecordState::Active,
                _ => return Err(WireError::InvalidField("record_state")),
            };
            held.push((dev_id, state));
        }
        if held
            .iter()
            .filter(|(_, s)| *s == RecordState::Active)
            .count()
            > 1
        {
            return Err(WireError::InvalidField("active_count"));
        }
        let tomb_count = usize::from(r.u8()?);
        if tomb_count > MAX_TOMBSTONES {
            return Err(WireError::InvalidField("tombstone_count"));
        }
        let mut tombstones = Vec::with_capacity(tomb_count);
        for _ in 0..tomb_count {
            tombstones.push(r.array16()?);
        }
        let mut all: Vec<&Id16> = held.iter().map(|(id, _)| id).chain(&tombstones).collect();
        all.sort_unstable();
        if all.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(WireError::InvalidField("duplicate_dev_id"));
        }
        Ok(Self { held, tombstones })
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        Self::read(&mut Reader::new(bytes))
    }
}

/// The host's answer inside message 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg2Payload {
    Accept(Accept),
    /// The timestamp was not newer than the last committed one. The iPad
    /// jumps past `stored_timestamp_us` and tries once more.
    Stale {
        stored_timestamp_us: u64,
    },
}

const MSG2_ACCEPT: u8 = 0;
const MSG2_STALE: u8 = 1;

/// The v3 HelloAck: the v2 fields minus status, version and credential, plus
/// the slot status.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Accept {
    /// Nonzero. Every record of this session carries it.
    pub session_id: u32,
    pub heartbeat_interval_ms: u16,
    pub report_interval_ms: u16,
    pub liveness_timeout_ms: u16,
    pub stream_config: StreamConfig,
    pub host_caps: u16,
    pub host_name: String,
    pub slot_status: SlotStatus,
}

impl Msg2Payload {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96 + MAX_SLOT_STATUS_SIZE);
        match self {
            Self::Accept(a) => {
                assert_ne!(a.session_id, 0);
                out.push(MSG2_ACCEPT);
                out.extend_from_slice(&a.session_id.to_le_bytes());
                out.extend_from_slice(&a.heartbeat_interval_ms.to_le_bytes());
                out.extend_from_slice(&a.report_interval_ms.to_le_bytes());
                out.extend_from_slice(&a.liveness_timeout_ms.to_le_bytes());
                encode_stream_config(&a.stream_config, &mut out);
                out.extend_from_slice(&a.host_caps.to_le_bytes());
                push_name(&mut out, &a.host_name);
                a.slot_status.encode_into(&mut out);
            }
            Self::Stale {
                stored_timestamp_us,
            } => {
                out.push(MSG2_STALE);
                out.extend_from_slice(&stored_timestamp_us.to_le_bytes());
            }
        }
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        let mut r = Reader::new(bytes);
        match r.u8()? {
            MSG2_ACCEPT => {
                let session_id = r.u32()?;
                if session_id == 0 {
                    return Err(WireError::InvalidField("session_id"));
                }
                Ok(Self::Accept(Accept {
                    session_id,
                    heartbeat_interval_ms: r.u16()?,
                    report_interval_ms: r.u16()?,
                    liveness_timeout_ms: r.u16()?,
                    stream_config: read_stream_config(&mut r)?,
                    host_caps: r.u16()?,
                    host_name: r.name()?,
                    slot_status: SlotStatus::read(&mut r)?,
                }))
            }
            MSG2_STALE => Ok(Self::Stale {
                stored_timestamp_us: r.u64()?,
            }),
            _ => Err(WireError::InvalidField("msg2_kind")),
        }
    }
}

/// Inner types of the session messages sealed in records. Values below 0x40
/// are the v2 [`PacketType`] of the message carried (its v2 body follows).
pub const INNER_SESSION_CONFIRM: u8 = 0x50;
pub const INNER_SESSION_CONFIRM_ACK: u8 = 0x51;
pub const INNER_SESSION_REFUSED: u8 = 0x52;
pub const INNER_BYE: u8 = PacketType::Bye as u8;

/// The sealed messages the session state machines act on themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMessage {
    /// iPad → host: commit this session. Empty body.
    Confirm,
    /// Host → iPad: committed, and durable.
    ConfirmAck(SlotStatus),
    /// Host → iPad: this key's record conflicted and is retired.
    Refused(SlotStatus),
    /// Either way: the session is over. One byte, the v2 `ByeReason`.
    Bye(u8),
}

impl SessionMessage {
    pub fn inner_type(&self) -> u8 {
        match self {
            Self::Confirm => INNER_SESSION_CONFIRM,
            Self::ConfirmAck(_) => INNER_SESSION_CONFIRM_ACK,
            Self::Refused(_) => INNER_SESSION_REFUSED,
            Self::Bye(_) => INNER_BYE,
        }
    }

    pub fn encode_body(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Confirm => {}
            Self::ConfirmAck(status) | Self::Refused(status) => status.encode_into(&mut out),
            Self::Bye(reason) => out.push(*reason),
        }
        out
    }

    /// Parses a session message, or returns `Ok(None)` for any other inner
    /// type, which the caller passes on to the application.
    pub fn parse(inner_type: u8, body: &[u8]) -> Result<Option<Self>, WireError> {
        let mut r = Reader::new(body);
        Ok(Some(match inner_type {
            INNER_SESSION_CONFIRM => Self::Confirm,
            INNER_SESSION_CONFIRM_ACK => Self::ConfirmAck(SlotStatus::read(&mut r)?),
            INNER_SESSION_REFUSED => Self::Refused(SlotStatus::read(&mut r)?),
            INNER_BYE => Self::Bye(r.u8()?),
            _ => return Ok(None),
        }))
    }
}

fn encode_raw_prefix(out: &mut [u8], packet_type: u8, body_len: usize) {
    out[0..2].copy_from_slice(&MAGIC.to_le_bytes());
    out[2] = VERSION;
    out[3] = packet_type;
    out[4] = 0;
    out[5] = 0;
    let len = u16::try_from(body_len).expect("body fits a datagram");
    out[6..8].copy_from_slice(&len.to_le_bytes());
}

/// Checks magic, version, type, zero flags and the length invariant, and
/// returns the body after the 16-byte control header.
fn control_body(datagram: &[u8], packet_type: u8) -> Result<&[u8], WireError> {
    if datagram.len() < CONTROL_HEADER_SIZE {
        return Err(WireError::Truncated);
    }
    if datagram[0..2] != MAGIC.to_le_bytes() {
        return Err(WireError::BadMagic);
    }
    if datagram[2] != VERSION {
        return Err(WireError::UnsupportedVersion(datagram[2]));
    }
    if datagram[3] != packet_type {
        return Err(WireError::UnknownType(datagram[3]));
    }
    if datagram[4] != 0 || datagram[5] != 0 {
        return Err(WireError::InvalidField("flags"));
    }
    let payload_len = usize::from(u16::from_le_bytes([datagram[6], datagram[7]]));
    if payload_len != datagram.len() - CONTROL_HEADER_SIZE {
        return Err(WireError::LengthMismatch);
    }
    Ok(&datagram[CONTROL_HEADER_SIZE..])
}

/// Reads one extension that runs exactly to the end of the body.
fn extension<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], WireError> {
    let tag = r.u16()?;
    if tag != EXT_SESSION {
        return Err(WireError::InvalidField("extension_tag"));
    }
    let len = usize::from(r.u16()?);
    if len != r.remaining() {
        return Err(WireError::LengthMismatch);
    }
    r.take(len)
}

fn check_noise_len(noise: &[u8]) -> Result<(), WireError> {
    if noise.len() < NOISE_OVERHEAD || noise.len() > MAX_NOISE_MESSAGE {
        return Err(WireError::InvalidField("noise_len"));
    }
    Ok(())
}

fn encode_stream_config(cfg: &StreamConfig, out: &mut Vec<u8>) {
    out.extend_from_slice(&cfg.stream_epoch.to_le_bytes());
    out.extend_from_slice(&cfg.width.to_le_bytes());
    out.extend_from_slice(&cfg.height.to_le_bytes());
    out.extend_from_slice(&cfg.fps.to_le_bytes());
    out.push(cfg.codec);
    out.push(cfg.flags);
    out.extend_from_slice(&cfg.bitrate_bps.to_le_bytes());
}

fn read_stream_config(r: &mut Reader<'_>) -> Result<StreamConfig, WireError> {
    Ok(StreamConfig {
        stream_epoch: r.u32()?,
        width: r.u16()?,
        height: r.u16()?,
        fps: r.u16()?,
        codec: r.u8()?,
        flags: r.u8()?,
        bitrate_bps: r.u32()?,
    })
}

/// Writes a length-prefixed name. Names over [`MAX_NAME_LEN`] bytes are cut
/// at a character boundary, as v2 does.
fn push_name(out: &mut Vec<u8>, name: &str) {
    let mut end = name.len().min(MAX_NAME_LEN);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    out.push(end as u8);
    out.extend_from_slice(&name.as_bytes()[..end]);
}

/// Bounds-checked little-endian reader. Every read returns the value or
/// `Truncated`; indexing can't panic.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.at
    }

    fn rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.at..];
        self.at = self.data.len();
        rest
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(len).ok_or(WireError::Truncated)?;
        let slice = self.data.get(self.at..end).ok_or(WireError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn array16(&mut self) -> Result<Id16, WireError> {
        Ok(self.take(16)?.try_into().unwrap())
    }

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
    use crate::v2::control::{parse_control, ControlMessage, HelloStatus};

    fn sample_status() -> SlotStatus {
        SlotStatus {
            held: vec![
                ([1; 16], RecordState::Active),
                ([2; 16], RecordState::Pending),
            ],
            tombstones: vec![[3; 16]],
        }
    }

    #[test]
    fn msg1_is_a_valid_v2_hello2_asking_for_protocol_3_with_a_zero_tail() {
        let noise = vec![0x5A; 120];
        let datagram = encode_msg1(0xDEAD_BEEF, 50_000, &[9; 16], &noise);
        let (header, message) = parse_control(&datagram).expect("v2 parser accepts the shell");
        assert_eq!(header.packet_type, PacketType::Hello2);
        assert_eq!(header.session_id, 0);
        let ControlMessage::Hello2(hello) = message else {
            panic!("not a HELLO2");
        };
        assert_eq!((hello.proto_min, hello.proto_max), (3, 3));
        assert_eq!(hello.client_nonce, 0xDEAD_BEEF);
        assert_eq!(hello.listen_port, 50_000);
        assert_eq!(hello.device_name, "");
        assert_eq!(hello.auth_token, [0; 16]);
        assert_eq!(hello.pairing_code, 0);
        assert_eq!(hello.device_id, 0);

        let parsed = parse_msg1(&datagram).unwrap();
        assert_eq!(parsed.dev_id, [9; 16]);
        assert_eq!(parsed.noise, &noise[..]);
        assert_eq!(classify(&datagram), V3Kind::Handshake1);
    }

    #[test]
    fn msg1_rejects_any_shell_or_framing_change() {
        let datagram = encode_msg1(1, 2, &[9; 16], &[0x5A; 64]);
        for at in 0..datagram.len() {
            let mut bad = datagram.clone();
            bad[at] ^= 0x01;
            let allowed = (18..24).contains(&at) // client_nonce, listen_port
                || at >= MSG1_EXTENSION_OFFSET + 4; // dev_id and Noise bytes
            assert_eq!(parse_msg1(&bad).is_ok(), allowed, "byte {at}");
        }
        assert!(parse_msg1(&datagram[..datagram.len() - 1]).is_err());
        let mut longer = datagram.clone();
        longer.push(0);
        assert!(parse_msg1(&longer).is_err());
    }

    #[test]
    fn a_v2_hello2_is_not_v3() {
        let v2 = crate::v2::control::encode_control(
            0,
            1,
            &ControlMessage::Hello2(crate::v2::control::Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: 2,
                decoder_caps: 1,
                feature_caps: 0,
                screen_px_w: 0,
                screen_px_h: 0,
                screen_pt_w: 0,
                screen_pt_h: 0,
                refresh_hz: 60,
                device_name: "x".into(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
        );
        assert_eq!(classify(&v2), V3Kind::Other);
        assert!(parse_msg1(&v2).is_err());
    }

    #[test]
    fn msg2_round_trip_and_v2_ignores_it() {
        let noise = vec![0x33; 200];
        let datagram = encode_msg2(&noise);
        assert_eq!(parse_msg2(&datagram).unwrap(), &noise[..]);
        assert_eq!(classify(&datagram), V3Kind::Handshake2);
        assert!(parse_control(&datagram).is_err());
        let mut bad = datagram.clone();
        bad[9] = 1;
        assert!(parse_msg2(&bad).is_err());
    }

    #[test]
    fn hello_payload_round_trip_ignores_trailing_bytes() {
        let payload = HelloPayload {
            timestamp_us: 1_700_000_000_000_000,
            listen_port: 9876,
            decoder_caps: 3,
            feature_caps: 0x17,
            screen_px_w: 2732,
            screen_px_h: 2048,
            screen_pt_w: 1366,
            screen_pt_h: 1024,
            refresh_hz: 120,
            preferred_fps: 120,
            device_id: 0x0102_0304_0506_0708,
            device_name: "iPad".into(),
        };
        let mut bytes = payload.encode();
        assert_eq!(HelloPayload::parse(&bytes).unwrap(), payload);
        bytes.extend_from_slice(&[1, 2, 3]);
        assert_eq!(HelloPayload::parse(&bytes).unwrap(), payload);
        assert!(HelloPayload::parse(&payload.encode()[..20]).is_err());
    }

    #[test]
    fn msg2_payload_round_trip_and_size_bound() {
        let accept = Msg2Payload::Accept(Accept {
            session_id: 7,
            heartbeat_interval_ms: 1000,
            report_interval_ms: 500,
            liveness_timeout_ms: 3000,
            stream_config: StreamConfig::default(),
            host_caps: 0x3F,
            host_name: "h".repeat(MAX_NAME_LEN),
            slot_status: SlotStatus {
                held: (0..4)
                    .map(|i| {
                        let state = if i == 0 {
                            RecordState::Active
                        } else {
                            RecordState::Pending
                        };
                        ([i as u8; 16], state)
                    })
                    .collect(),
                tombstones: (10..18).map(|i| [i as u8; 16]).collect(),
            },
        });
        let bytes = accept.encode();
        assert_eq!(Msg2Payload::parse(&bytes).unwrap(), accept);
        // 99-byte v3 HelloAck body at most (94 here) plus a 198-byte status.
        assert!(bytes.len() <= 99 + MAX_SLOT_STATUS_SIZE, "{}", bytes.len());
        let stale = Msg2Payload::Stale {
            stored_timestamp_us: 42,
        };
        assert_eq!(Msg2Payload::parse(&stale.encode()).unwrap(), stale);
        assert!(Msg2Payload::parse(&[9]).is_err());
        let _ = HelloStatus::Ok;
    }

    #[test]
    fn slot_status_rejects_overfull_duplicate_or_two_active() {
        let mut bytes = Vec::new();
        sample_status().encode_into(&mut bytes);
        assert_eq!(SlotStatus::parse(&bytes).unwrap(), sample_status());

        let dup = SlotStatus {
            held: vec![([1; 16], RecordState::Pending)],
            tombstones: vec![[1; 16]],
        };
        let mut bytes = Vec::new();
        dup.encode_into(&mut bytes);
        assert!(SlotStatus::parse(&bytes).is_err());

        let two_active = SlotStatus {
            held: vec![
                ([1; 16], RecordState::Active),
                ([2; 16], RecordState::Active),
            ],
            tombstones: vec![],
        };
        let mut bytes = Vec::new();
        two_active.encode_into(&mut bytes);
        assert!(SlotStatus::parse(&bytes).is_err());

        assert!(SlotStatus::parse(&[5]).is_err());
        assert!(SlotStatus::parse(&[0, 9]).is_err());
        assert!(SlotStatus::parse(&[0, 1, 0]).is_err());
    }

    #[test]
    fn session_messages_round_trip_and_pass_others_through() {
        for message in [
            SessionMessage::Confirm,
            SessionMessage::ConfirmAck(sample_status()),
            SessionMessage::Refused(sample_status()),
            SessionMessage::Bye(2),
        ] {
            let body = message.encode_body();
            assert_eq!(
                SessionMessage::parse(message.inner_type(), &body).unwrap(),
                Some(message)
            );
        }
        assert_eq!(
            SessionMessage::parse(PacketType::InputEvent as u8, &[1, 2]).unwrap(),
            None
        );
        assert!(SessionMessage::parse(INNER_BYE, &[]).is_err());
    }
}
