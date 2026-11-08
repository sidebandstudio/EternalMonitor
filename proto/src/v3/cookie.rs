//! The return-routability cookie for message 1 (both the session and the
//! pairing handshake), in the style of WireGuard's cookie reply.
//!
//! While a host is under load it answers a message 1 that carries no valid
//! cookie for its source with a cookie reply, and does nothing else: no
//! Noise read, no Diffie-Hellman, no per-source window. The iPad sends its
//! message 1 again with the cookie, which proves it receives at that
//! address, so spoofed sources can never reach the host's crypto or its
//! shared budgets.
//!
//! - **The cookie** is `HMAC-SHA-256(secret, binding)[..16]`. For a session
//!   the binding is the `dev_id` and the source's IP and port
//!   ([`PeerAddr::cookie_input`]); for pairing, the QR window's `nonce_r`
//!   and the source. The secret is 32 random bytes rotated every
//!   [`SECRET_LIFETIME`]. The
//!   previous secret is still accepted, so a cookie is good for at least
//!   that long.
//! - **The cookie reply**, host → iPad, packet type [`COOKIE_TYPE`]:
//!
//!   ```text
//!   [0..16]   v2 control header      type 0x42, session_id 0, msg_seq 0
//!   [16..18]  extension tag u16      the handshake's (which key opens it)
//!   [18..20]  length u16             44
//!   [20..32]  nonce                  12 random bytes
//!   [32..64]  ChaCha20-Poly1305      seal(cookie), 16-byte tag
//!   ```
//!
//!   The AAD is `SHA-256` of the message 1 it answers, without any cookie.
//!   The key ([`session_key`], [`pairing_key`]) comes from the handshake's
//!   own secret, so only the holder of that secret (the `dev_id`'s key, or
//!   the QR) can read it.
//! - **Message 1 with a cookie** ends in one more extension,
//!   [`EXT_COOKIE`], length 16: `mac2 = HMAC-SHA-256(cookie, message 1
//!   without it)[..16]`. The authenticated part of message 1 is unchanged.
//!
//! [`PeerAddr::cookie_input`]: super::host::PeerAddr::cookie_input

use std::time::Duration;

use super::pairing::hkdf32;
use super::wire::{self, V3Kind, EXTENSION_HEADER_SIZE, MSG1_EXTENSION_OFFSET};
use super::{Id16, Key32, Random, Zeroizing, EXT_PAIR_QR, EXT_SESSION};
use crate::v2::control::CONTROL_HEADER_SIZE;
use crate::v2::{WireError, MAGIC, VERSION};

/// Packet type of a cookie reply. Not a v2 type, so v2 receivers drop it.
pub const COOKIE_TYPE: u8 = 0x42;
/// Extension tag of a message 1's cookie MAC.
pub const EXT_COOKIE: u16 = 0x00FF;
pub const COOKIE_LEN: usize = 16;
pub const MAC2_LEN: usize = 16;
/// Bytes of a cookie reply: the header, the extension header, nonce, sealed
/// cookie and tag.
pub const COOKIE_REPLY_SIZE: usize = CONTROL_HEADER_SIZE + EXTENSION_HEADER_SIZE + 12 + 32;
/// How long a cookie secret is used for new cookies; the previous one is
/// still accepted for as long again.
pub const SECRET_LIFETIME: Duration = Duration::from_secs(120);
/// How long an iPad attaches a cookie it got. Below [`SECRET_LIFETIME`], so
/// the host still accepts it.
pub const CLIENT_COOKIE_LIFETIME: Duration = Duration::from_secs(110);
/// HKDF salt of the cookie keys.
pub const COOKIE_KEY_SALT: &[u8] = b"EM v3 cookie";

fn hmac16(key: &[u8], data: &[u8]) -> [u8; 16] {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let tag = ring::hmac::sign(&key, data);
    tag.as_ref()[..16].try_into().expect("16 of 32 bytes")
}

/// The key a session's cookie reply is sealed under:
/// HKDF-SHA-256(salt `"EM v3 cookie"`, IKM `K_dev`, info `dev_id`).
pub fn session_key(k_dev: &Key32, dev_id: &Id16) -> Zeroizing<Key32> {
    hkdf32(COOKIE_KEY_SALT, k_dev, dev_id)
}

/// The key a pairing's cookie reply is sealed under: HKDF-SHA-256(salt
/// `"EM v3 cookie"`, IKM the QR pairing PSK, info `"pair"`).
pub fn pairing_key(psk: &Key32) -> Zeroizing<Key32> {
    hkdf32(COOKIE_KEY_SALT, psk, b"pair")
}

/// The cookie for `binding` under `secret`: [`session_binding`] or
/// [`pairing_binding`]. The iPad treats a cookie as opaque.
pub fn cookie_value(secret: &Key32, binding: &[u8]) -> [u8; COOKIE_LEN] {
    hmac16(secret, binding)
}

/// What a session cookie binds: the `dev_id`, then the address
/// ([`PeerAddr::cookie_input`]). A cookie one device got is no use with
/// another's message 1, even from the same address.
pub fn session_binding(dev_id: &Id16, address: &[u8]) -> Vec<u8> {
    [&dev_id[..], address].concat()
}

/// What a pairing cookie binds: the QR window's `nonce_r`, then the
/// address. A cookie from an earlier QR is no use under a later one.
pub fn pairing_binding(nonce_r: &Id16, address: &[u8]) -> Vec<u8> {
    [&nonce_r[..], address].concat()
}

/// `HMAC-SHA-256(cookie, message 1 without a cookie)[..16]`.
pub fn mac2(cookie: &[u8; COOKIE_LEN], msg1: &[u8]) -> [u8; MAC2_LEN] {
    hmac16(cookie, msg1)
}

/// The end of a message 1's handshake extension, or `None` if `datagram`
/// isn't a v3 message 1 whose extension fits.
fn handshake_end(datagram: &[u8]) -> Option<usize> {
    if wire::classify(datagram) != V3Kind::Handshake1 {
        return None;
    }
    let at = MSG1_EXTENSION_OFFSET;
    let len = datagram.get(at + 2..at + 4)?;
    let end = at + EXTENSION_HEADER_SIZE + usize::from(u16::from_le_bytes([len[0], len[1]]));
    (end <= datagram.len()).then_some(end)
}

/// Writes `payload_len` for a datagram of `len` bytes.
fn set_payload_len(datagram: &mut [u8]) {
    let len = u16::try_from(datagram.len() - CONTROL_HEADER_SIZE).expect("fits a datagram");
    datagram[6..8].copy_from_slice(&len.to_le_bytes());
}

/// Splits a message 1 into the message 1 without its cookie and the cookie
/// MAC it carries, if any. A message 1 with no cookie comes back as it is.
/// Anything after the handshake extension other than exactly one
/// [`EXT_COOKIE`] of length 16 is left in place, so the handshake's own
/// parser rejects it.
pub fn split(datagram: &[u8]) -> (std::borrow::Cow<'_, [u8]>, Option<[u8; MAC2_LEN]>) {
    let Some(end) = handshake_end(datagram) else {
        return (datagram.into(), None);
    };
    let tail = &datagram[end..];
    if tail.len() != EXTENSION_HEADER_SIZE + MAC2_LEN
        || tail[0..2] != EXT_COOKIE.to_le_bytes()
        || tail[2..4] != (MAC2_LEN as u16).to_le_bytes()
    {
        return (datagram.into(), None);
    }
    let mut msg1 = datagram[..end].to_vec();
    set_payload_len(&mut msg1);
    let mac = tail[4..].try_into().expect("16 bytes");
    (msg1.into(), Some(mac))
}

/// `msg1` (without a cookie) with `cookie`'s MAC attached.
pub fn attach(msg1: &[u8], cookie: &[u8; COOKIE_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg1.len() + EXTENSION_HEADER_SIZE + MAC2_LEN);
    out.extend_from_slice(msg1);
    out.extend_from_slice(&EXT_COOKIE.to_le_bytes());
    out.extend_from_slice(&(MAC2_LEN as u16).to_le_bytes());
    out.extend_from_slice(&mac2(cookie, msg1));
    set_payload_len(&mut out);
    out
}

/// A cookie reply as it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieReply {
    /// The handshake it answers: [`EXT_SESSION`] or [`EXT_PAIR_QR`].
    pub tag: u16,
    pub nonce: [u8; 12],
    /// The sealed cookie and its tag.
    pub sealed: [u8; 32],
}

impl CookieReply {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; CONTROL_HEADER_SIZE];
        out[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        out[2] = VERSION;
        out[3] = COOKIE_TYPE;
        out.extend_from_slice(&self.tag.to_le_bytes());
        out.extend_from_slice(&44u16.to_le_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.sealed);
        set_payload_len(&mut out);
        out
    }

    /// Parses exactly what [`encode`](Self::encode) writes.
    pub fn parse(datagram: &[u8]) -> Result<Self, WireError> {
        if datagram.len() != COOKIE_REPLY_SIZE {
            return Err(WireError::LengthMismatch);
        }
        if datagram[0..2] != MAGIC.to_le_bytes() || datagram[2] != VERSION {
            return Err(WireError::BadMagic);
        }
        if datagram[3] != COOKIE_TYPE {
            return Err(WireError::UnknownType(datagram[3]));
        }
        let mut expected = [0u8; 16];
        expected[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        expected[2] = VERSION;
        expected[3] = COOKIE_TYPE;
        expected[6..8].copy_from_slice(&48u16.to_le_bytes());
        if datagram[..16] != expected {
            return Err(WireError::InvalidField("header"));
        }
        let tag = u16::from_le_bytes([datagram[16], datagram[17]]);
        if tag != EXT_SESSION && tag != EXT_PAIR_QR {
            return Err(WireError::InvalidField("extension_tag"));
        }
        if datagram[18..20] != 44u16.to_le_bytes() {
            return Err(WireError::LengthMismatch);
        }
        Ok(Self {
            tag,
            nonce: datagram[20..32].try_into().expect("12 bytes"),
            sealed: datagram[32..64].try_into().expect("32 bytes"),
        })
    }
}

fn aead_key(key: &Key32) -> ring::aead::LessSafeKey {
    let unbound = ring::aead::UnboundKey::new(&ring::aead::CHACHA20_POLY1305, key)
        .expect("32-byte ChaCha20-Poly1305 key");
    ring::aead::LessSafeKey::new(unbound)
}

/// `SHA-256` of a message 1 without its cookie: the AAD of its reply.
pub fn msg1_digest(msg1: &[u8]) -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, msg1)
        .as_ref()
        .try_into()
        .expect("32 bytes")
}

/// Seals `cookie` into a reply for the message 1 with `digest`.
pub fn seal(
    key: &Key32,
    tag: u16,
    nonce: [u8; 12],
    cookie: &[u8; COOKIE_LEN],
    digest: &[u8; 32],
) -> CookieReply {
    let mut in_out = cookie.to_vec();
    aead_key(key)
        .seal_in_place_append_tag(
            ring::aead::Nonce::assume_unique_for_key(nonce),
            ring::aead::Aad::from(digest),
            &mut in_out,
        )
        .expect("a 16-byte cookie seals");
    CookieReply {
        tag,
        nonce,
        sealed: in_out.try_into().expect("16 + 16 bytes"),
    }
}

/// Opens a reply to the message 1 with `digest`: the cookie, or `None` for a
/// reply under another key or for another message 1.
pub fn open(key: &Key32, reply: &CookieReply, digest: &[u8; 32]) -> Option<[u8; COOKIE_LEN]> {
    let mut in_out = reply.sealed;
    let plain = aead_key(key)
        .open_in_place(
            ring::aead::Nonce::assume_unique_for_key(reply.nonce),
            ring::aead::Aad::from(digest),
            &mut in_out,
        )
        .ok()?;
    plain.try_into().ok()
}

/// The host's cookie secrets.
///
/// Cookie replies have no budget of their own. A reply is a fixed amount of
/// symmetric work, it's smaller than the message 1 it answers, and a budget
/// would be the one thing a flood of spoofed message 1s could use up to
/// keep the genuine iPad from its cookie.
///
/// A reply's nonce is a random 64-bit prefix and a 32-bit counter, with a
/// fresh prefix whenever the counter wraps: never repeated under one jar,
/// and prefixes drawn at random for every jar (a host start) collide with
/// a chance of about one in 2^64 per pair, although the reply keys stay the
/// same for a device across starts.
pub struct CookieJar {
    current: Zeroizing<Key32>,
    previous: Option<Zeroizing<Key32>>,
    rotated: Duration,
    nonce_prefix: [u8; 8],
    nonce_counter: u32,
}

impl CookieJar {
    pub fn new(now: Duration, rng: &mut dyn Random) -> Self {
        let mut current = Zeroizing::new([0u8; 32]);
        rng.fill(&mut *current);
        let mut nonce_prefix = [0u8; 8];
        rng.fill(&mut nonce_prefix);
        Self {
            current,
            previous: None,
            rotated: now,
            nonce_prefix,
            nonce_counter: 0,
        }
    }

    /// The next reply nonce.
    fn next_nonce(&mut self, rng: &mut dyn Random) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.nonce_prefix);
        nonce[8..].copy_from_slice(&self.nonce_counter.to_le_bytes());
        self.nonce_counter = self.nonce_counter.wrapping_add(1);
        if self.nonce_counter == 0 {
            rng.fill(&mut self.nonce_prefix);
        }
        nonce
    }

    /// Moves to a fresh secret once the current one is [`SECRET_LIFETIME`]
    /// old; the one before is forgotten.
    fn rotate(&mut self, now: Duration, rng: &mut dyn Random) {
        if now.saturating_sub(self.rotated) < SECRET_LIFETIME {
            return;
        }
        let mut next = Zeroizing::new([0u8; 32]);
        rng.fill(&mut *next);
        // Two lifetimes or more since the last rotation: the old current is
        // too old to keep as well.
        let stale = now.saturating_sub(self.rotated) >= 2 * SECRET_LIFETIME;
        let old = std::mem::replace(&mut self.current, next);
        self.previous = (!stale).then_some(old);
        self.rotated = now;
    }

    /// Whether `mac` is a valid cookie MAC for `msg1` (without its cookie)
    /// under `binding`'s cookie, from the current or the previous secret.
    pub fn verify(
        &mut self,
        now: Duration,
        binding: &[u8],
        msg1: &[u8],
        mac: &[u8; MAC2_LEN],
        rng: &mut dyn Random,
    ) -> bool {
        self.rotate(now, rng);
        std::iter::once(&self.current)
            .chain(self.previous.as_ref())
            .any(|secret| {
                use subtle::ConstantTimeEq;
                let expected = mac2(&cookie_value(secret, binding), msg1);
                bool::from(expected.ct_eq(mac))
            })
    }

    /// A reply to `msg1` (without any cookie) carrying `binding`'s current
    /// cookie, sealed under `key`.
    pub fn reply(
        &mut self,
        now: Duration,
        binding: &[u8],
        key: &Key32,
        tag: u16,
        msg1: &[u8],
        rng: &mut dyn Random,
    ) -> Vec<u8> {
        self.rotate(now, rng);
        let cookie = cookie_value(&self.current, binding);
        let nonce = self.next_nonce(rng);
        seal(key, tag, nonce, &cookie, &msg1_digest(msg1)).encode()
    }
}

/// When a host requires a cookie on every message 1. In every mode, a
/// cached answer (message 2, or Stale, TooNew and PairAgain) goes to an
/// address other than the one its message 1 came from only with a cookie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieMode {
    /// While the handshake is under load: the default.
    UnderLoad,
    /// Always, for tests of the cookie path.
    Always,
    /// Never for load, for tests of the budgets beneath it. Never a
    /// host's setting.
    Off,
}

/// Message 1 arrivals per second, before any check, for the load test:
/// this second's and the one before. Counting arrivals rather than what the
/// cookie lets through keeps the load on for as long as the flood lasts.
#[derive(Debug, Clone, Copy)]
pub(super) struct LoadMeter {
    start: Duration,
    count: u32,
    last: u32,
}

impl LoadMeter {
    pub(super) fn new(now: Duration) -> Self {
        Self {
            start: now,
            count: 0,
            last: 0,
        }
    }

    pub(super) fn record(&mut self, now: Duration) {
        let elapsed = now.saturating_sub(self.start);
        if elapsed >= Duration::from_secs(1) {
            self.last = if elapsed < Duration::from_secs(2) {
                self.count
            } else {
                0
            };
            self.start = now;
            self.count = 0;
        }
        self.count = self.count.saturating_add(1);
    }

    /// Whether this second or the one before reached `per_second`.
    pub(super) fn busy(&self, now: Duration, per_second: u32) -> bool {
        let elapsed = now.saturating_sub(self.start);
        if elapsed < Duration::from_secs(1) {
            self.count >= per_second || self.last >= per_second
        } else {
            elapsed < Duration::from_secs(2) && self.count >= per_second
        }
    }
}

/// Whether a datagram is a cookie reply, from its first bytes.
pub fn is_reply(datagram: &[u8]) -> bool {
    datagram.len() >= 4
        && datagram[0..2] == MAGIC.to_le_bytes()
        && datagram[2] == VERSION
        && datagram[3] == COOKIE_TYPE
}

/// What an iPad keeps of a cookie it got: the cookie and when, per host.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HeldCookie {
    pub cookie: [u8; COOKIE_LEN],
    pub received: Duration,
}

impl std::fmt::Debug for HeldCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldCookie")
            .field("received", &self.received)
            .finish_non_exhaustive()
    }
}

impl HeldCookie {
    /// Whether it's still young enough to attach.
    pub fn fresh(&self, now: Duration) -> bool {
        now.saturating_sub(self.received) < CLIENT_COOKIE_LIFETIME
    }
}

#[cfg(test)]
mod tests;
