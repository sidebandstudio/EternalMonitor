//! EternalMonitor wire protocol v3: an authenticated handshake per connection,
//! then every packet encrypted and authenticated with a replay window.
//!
//! v3 keeps the v2 envelope: every datagram still starts with the 8-byte v2
//! common prefix with version byte 2, so a v2 peer's version gate sees a
//! well-formed HELLO2 and can answer `VersionUnsupported`. "v3" lives only in
//! HELLO2's protocol range and in the new packet types below.
//!
//! # Handshake
//!
//! `Noise_NNpsk0_25519_ChaChaPoly_SHA256` (Noise revision 34) with the
//! device's 32-byte key `K_dev` as the PSK and the prologue
//! `lv_cat("EternalMonitor session v1", [3], dev_id, host_id)` ([`lv_cat`]).
//!
//! Message 1, iPad → host, packet type HELLO2 (0x10), `session_id` 0:
//!
//! ```text
//! [0..16]   v2 control header      type 0x10, session_id 0, msg_seq 1
//! [16..67]  HELLO2 shell (51)      proto_min = proto_max = 3, client_nonce,
//!                                  listen_port, every other field zero, empty
//!                                  name, 29-byte tail present and all zero
//! [67..69]  extension tag u16      EXT_SESSION
//! [69..71]  extension len u16      bytes that follow, to the end
//! [71..87]  dev_id                 in the clear, so the host can find K_dev
//! [87..]    Noise message 1        e (32) || AEAD(Hello payload) (+16)
//! ```
//!
//! The shell exists only for the version gate. A v3 host reads every session
//! field from the encrypted [`wire::HelloPayload`], never from the shell.
//!
//! Message 2, host → iPad, packet type [`HANDSHAKE2_TYPE`], `session_id` 0:
//!
//! ```text
//! [0..16]   v2 control header      type 0x41, session_id 0, msg_seq 0
//! [16..18]  extension tag u16      EXT_SESSION
//! [18..20]  extension len u16
//! [20..]    Noise message 2        e (32) || AEAD(Accept or Stale) (+16)
//! ```
//!
//! # Records
//!
//! After the handshake, each direction seals with its own key from Noise
//! `Split()` (initiator → responder uses the first key):
//!
//! ```text
//! [0..8]    v2 common prefix       type SEALED_TYPE (0x40), flags 0,
//!                                  payload_len = datagram_len - 20
//! [8..12]   session_id u32         assigned by the host in message 2
//! [12..20]  counter u64            per direction, from 0, never reused
//! [20..]    ChaCha20-Poly1305      seal(inner_type u8 || body), 16-byte tag
//! ```
//!
//! The AAD is the whole 20-byte header. The nonce is the Noise ChaChaPoly
//! nonce: four zero bytes, then the counter little-endian. Overhead is 37
//! bytes. A retransmit is sealed again under a fresh counter.
//!
//! Integers are little-endian throughout, like v2.

pub mod client;
pub mod handshake;
pub mod host;
pub mod pairing;
pub mod record;
pub mod replay;
pub mod wire;

#[cfg(test)]
mod golden;
#[cfg(test)]
mod golden_client;
#[cfg(test)]
mod golden_differential;
#[cfg(test)]
mod vectors;

use std::time::Duration;

/// The protocol version negotiated in HELLO2's `proto_min`/`proto_max`.
pub const PROTOCOL_VERSION: u8 = 3;
/// Noise protocol name. Fixed: any change is a new protocol version.
pub const NOISE_PARAMS: &str = "Noise_NNpsk0_25519_ChaChaPoly_SHA256";
/// First item of the session prologue.
pub const SESSION_PROLOGUE_LABEL: &[u8] = b"EternalMonitor session v1";

/// Packet type of a sealed record. Not a v2 [`crate::v2::PacketType`], so v2
/// receivers drop it as an unknown type.
pub const SEALED_TYPE: u8 = 0x40;
/// Packet type of handshake message 2.
pub const HANDSHAKE2_TYPE: u8 = 0x41;

/// Extension tag for the reconnect handshake. Pairing methods take other
/// tags.
pub const EXT_SESSION: u16 = 0x0001;
/// Extension tag for QR pairing (method P1). Code pairing (CPace, phase 4)
/// takes another.
pub const EXT_PAIR_QR: u16 = 0x0002;

/// 16-byte identifiers: `dev_id` (assigned by the host at pairing) and
/// `host_id` (one per host install).
pub type Id16 = [u8; 16];
/// A 32-byte symmetric key.
pub type Key32 = [u8; 32];

/// Length-prefixed concatenation, as CPace's `lv_cat`: each item is preceded
/// by its length as an unsigned LEB128 integer.
pub fn lv_cat(items: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        let mut len = item.len();
        loop {
            let byte = (len & 0x7F) as u8;
            len >>= 7;
            if len == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out.extend_from_slice(item);
    }
    out
}

/// The reconnect handshake's Noise prologue.
pub fn session_prologue(dev_id: &Id16, host_id: &Id16) -> Vec<u8> {
    lv_cat(&[SESSION_PROLOGUE_LABEL, &[PROTOCOL_VERSION], dev_id, host_id])
}

/// Hard limits after which a session needs a fresh handshake.
pub const SESSION_MAX_AGE: Duration = Duration::from_secs(12 * 60 * 60);
/// Sealed packets per direction before a fresh handshake is required.
pub const MAX_RECORDS_PER_DIRECTION: u64 = 1 << 32;

/// The largest message 1 timestamp either side uses or accepts, in µs. Far
/// beyond any clock (about 146,000 years), and far enough below `u64::MAX`
/// that a stored value can always be passed by `+ 1`.
pub const MAX_TIMESTAMP_US: u64 = 1 << 62;

/// Key buffers that wipe themselves on drop.
pub use zeroize::Zeroizing;

/// Why a v3 datagram or payload was rejected. Callers drop and count these;
/// none of them ever produces a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3Error {
    /// A byte-level parse failure.
    Wire(crate::v2::WireError),
    /// AEAD or handshake authentication failed.
    Auth,
    /// The record counter is outside the replay window or already seen.
    Replay,
    /// The sending direction has used up its counters; a fresh handshake is
    /// required.
    Exhausted,
    /// The handshake library refused the message for a reason other than
    /// authentication (wrong state, bad length).
    Handshake,
}

impl From<crate::v2::WireError> for V3Error {
    fn from(error: crate::v2::WireError) -> Self {
        Self::Wire(error)
    }
}

impl std::fmt::Display for V3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(error) => write!(f, "{error}"),
            Self::Auth => write!(f, "authentication failed"),
            Self::Replay => write!(f, "replayed or too old"),
            Self::Exhausted => write!(f, "record counter exhausted"),
            Self::Handshake => write!(f, "handshake rejected"),
        }
    }
}

impl std::error::Error for V3Error {}

/// Source of random bytes for session ids, nonces and ephemeral keys.
pub trait Random {
    fn fill(&mut self, out: &mut [u8]);
}

/// The operating system's CSPRNG.
pub struct SystemRandom(ring::rand::SystemRandom);

impl Default for SystemRandom {
    fn default() -> Self {
        Self(ring::rand::SystemRandom::new())
    }
}

impl Random for SystemRandom {
    fn fill(&mut self, out: &mut [u8]) {
        use ring::rand::SecureRandom;
        self.0.fill(out).expect("system random source failed");
    }
}

/// Deterministic bytes for golden transcripts, simulations and fuzzing: a
/// SHA-256 counter stream over a seed. Not a CSPRNG for real keys.
#[cfg(any(test, feature = "test-hooks"))]
pub struct SeededRandom {
    seed: [u8; 32],
    counter: u64,
}

#[cfg(any(test, feature = "test-hooks"))]
impl SeededRandom {
    pub fn new(seed: u64) -> Self {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&seed.to_le_bytes());
        Self {
            seed: bytes,
            counter: 0,
        }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl Random for SeededRandom {
    fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(32) {
            let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
            ctx.update(&self.seed);
            ctx.update(&self.counter.to_le_bytes());
            self.counter += 1;
            let block = ctx.finish();
            chunk.copy_from_slice(&block.as_ref()[..chunk.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lv_cat_prefixes_each_item_with_its_leb128_length() {
        assert_eq!(lv_cat(&[b"ab", b"", b"c"]), b"\x02ab\x00\x01c");
        let long = vec![7u8; 200];
        let out = lv_cat(&[&long]);
        assert_eq!(&out[..2], &[0xC8, 0x01], "200 = 0x48 | 0x80, then 1");
        assert_eq!(out.len(), 202);
    }

    #[test]
    fn session_prologue_layout() {
        let prologue = session_prologue(&[0xAA; 16], &[0xBB; 16]);
        let mut expected = vec![25u8];
        expected.extend_from_slice(b"EternalMonitor session v1");
        expected.extend_from_slice(&[1, 3, 16]);
        expected.extend_from_slice(&[0xAA; 16]);
        expected.push(16);
        expected.extend_from_slice(&[0xBB; 16]);
        assert_eq!(prologue, expected);
    }

    #[test]
    fn sealed_and_handshake2_types_are_unknown_to_v2() {
        use crate::v2::{classify, Classified, PacketType};
        assert_eq!(PacketType::from_u8(SEALED_TYPE), None);
        assert_eq!(PacketType::from_u8(HANDSHAKE2_TYPE), None);
        let mut record = vec![0u8; 40];
        record[..2].copy_from_slice(&crate::v2::MAGIC.to_le_bytes());
        record[2] = crate::v2::VERSION;
        record[3] = SEALED_TYPE;
        assert_eq!(classify(&record), Classified::Unknown);
    }
}
