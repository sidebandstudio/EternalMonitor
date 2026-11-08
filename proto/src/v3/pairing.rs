//! QR pairing (design §4.1, method P1): how an iPad gets its first key for a
//! host, or a new one.
//!
//! The host shows a QR code with a fresh 32-byte secret `S`. The iPad scans
//! it and runs `Noise_NNpsk0_25519_ChaChaPoly_SHA256` with a PSK derived from
//! `S` and the binding context `ctx` ([`pairing_ctx`]), which is also the
//! prologue. Then:
//!
//! 1. message 1, iPad → host: the HELLO2 shell with the [`EXT_PAIR_QR`]
//!    extension, carrying `slot_id`, `nonce_i` and the iPad's version in the
//!    clear, then the Noise message (payload: the device name);
//! 2. message 2, host → iPad: a fresh `dev_id` and `K_new`, encrypted. The
//!    host keeps them in memory only;
//! 3. the iPad stores `K_new` durably, then sends a sealed `PairConfirm`:
//!    `HMAC(K_new, "confirm" ‖ h)`, and, if it holds the slot's active key,
//!    that key's `dev_id` and `HMAC(K_old, "replace" ‖ h)`. Or `PairAbort`
//!    if it couldn't store;
//! 4. the host checks the confirm and the replace proof (`replaces` is set
//!    only for the slot's current active record), stores the pending record
//!    (the caller runs that write, [`PairOutput::Store`]), and only then
//!    sends a sealed `PairDone` with the slot status, or `PairRefused` when a
//!    cap would be exceeded. The QR's secret is used up.
//!
//! `h` is the Noise handshake hash. A retried `PairConfirm` is a new record
//! with a fresh counter; once the record is stored, the host answers it with
//! the same `PairDone`, sealed again. Code pairing (CPace) is phase 4.
//!
//! [`EXT_PAIR_QR`]: super::EXT_PAIR_QR

use std::net::SocketAddr;
use std::time::Duration;

use snow::HandshakeState;

use super::client::ClientKey;
use super::handshake::{builder, map_snow, split};
use super::host::{PeerAddr, SourceLimiter};
use super::record::{Opener, RecordHeader, Sealer};
use super::wire::{
    self, PairAccept, PairHello, PairMessage, SlotStatus, V3Kind, MAX_NOISE_MESSAGE,
    PAIR_REFUSED_TOO_MANY,
};
use super::{lv_cat, Id16, Key32, Random, V3Error, Zeroizing, EXT_PAIR_QR, PROTOCOL_VERSION};
use crate::v2::WireError;

/// First item of `ctx`.
pub const PAIRING_LABEL: &[u8] = b"EternalMonitor pairing v1";
/// HKDF salt of the QR pairing PSK.
pub const QR_PSK_SALT: &[u8] = b"EM v3 qr psk";
/// How long a QR's secret is valid.
pub const QR_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// The QR's URL scheme.
pub const QR_SCHEME: &str = "eternaldisplay://";
/// The method item of `ctx` for QR pairing.
pub const METHOD_QR: &[u8] = b"qr";

/// What the QR code carries.
#[derive(Clone, PartialEq, Eq)]
pub struct QrPayload {
    /// Where the host listens.
    pub addr: SocketAddr,
    pub host_id: Id16,
    pub nonce_r: Id16,
    /// `S`: 32 random bytes, fresh for each QR.
    pub secret: Zeroizing<Key32>,
    /// The host app's version: part of `ctx`, which the iPad needs before
    /// its first message.
    pub host_version: String,
}

impl std::fmt::Debug for QrPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QrPayload")
            .field("addr", &self.addr)
            .field("host_id", &self.host_id)
            .field("host_version", &self.host_version)
            .finish_non_exhaustive()
    }
}

/// Appends lowercase hex, one character at a time.
fn push_hex(out: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0F)]));
    }
}

/// Exactly `N` bytes of lowercase hex.
fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let digits = text.as_bytes();
    if digits.len() != 2 * N {
        return None;
    }
    let value = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, pair) in digits.chunks(2).enumerate() {
        out[i] = (value(pair[0])? << 4) | value(pair[1])?;
    }
    Some(out)
}

impl QrPayload {
    /// `eternaldisplay://<ip>:<port>?v=3&h=<host_id>&n=<nonce_r>&s=<S>&hv=<version>`,
    /// with lowercase hex; an IPv6 address is in brackets.
    pub fn encode(&self) -> Zeroizing<String> {
        assert!(wire::valid_version(&self.host_version));
        const { assert!(PROTOCOL_VERSION < 10, "one digit") };
        let addr = self.addr.to_string();
        let len = QR_SCHEME.len()
            + addr.len()
            + "?v=3&h=&n=&s=&hv=".len()
            + 2 * (16 + 16 + 32)
            + self.host_version.len();
        // Written in place at its final size: the secret's hex is never in
        // memory that isn't wiped.
        let mut out = Zeroizing::new(String::with_capacity(len));
        let start = out.as_ptr();
        out.push_str(QR_SCHEME);
        out.push_str(&addr);
        out.push_str("?v=");
        out.push(char::from(b'0' + PROTOCOL_VERSION));
        out.push_str("&h=");
        push_hex(&mut out, &self.host_id);
        out.push_str("&n=");
        push_hex(&mut out, &self.nonce_r);
        out.push_str("&s=");
        push_hex(&mut out, &*self.secret);
        out.push_str("&hv=");
        out.push_str(&self.host_version);
        assert_eq!(out.len(), len);
        assert_eq!(out.as_ptr(), start, "never reallocated");
        out
    }

    /// Parses exactly what [`encode`](Self::encode) writes: each field once,
    /// in that order, nothing else.
    pub fn parse(text: &str) -> Result<Self, WireError> {
        let bad = |what| WireError::InvalidField(what);
        let rest = text.strip_prefix(QR_SCHEME).ok_or(bad("qr_scheme"))?;
        let (authority, query) = rest.split_once('?').ok_or(bad("qr_query"))?;
        let addr: SocketAddr = authority.parse().map_err(|_| bad("qr_addr"))?;
        if addr.port() == 0 || addr.to_string() != authority {
            return Err(bad("qr_addr"));
        }
        let mut fields = query.split('&');
        let mut field = |name: &str| {
            fields
                .next()
                .and_then(|f| f.strip_prefix(name))
                .and_then(|f| f.strip_prefix('='))
                .ok_or(bad("qr_field"))
        };
        if field("v")? != PROTOCOL_VERSION.to_string() {
            return Err(bad("qr_version"));
        }
        let host_id = unhex::<16>(field("h")?).ok_or(bad("qr_host_id"))?;
        let nonce_r = unhex::<16>(field("n")?).ok_or(bad("qr_nonce"))?;
        let secret = Zeroizing::new(unhex::<32>(field("s")?).ok_or(bad("qr_secret"))?);
        let host_version = field("hv")?;
        if !wire::valid_version(host_version) {
            return Err(bad("qr_host_version"));
        }
        if fields.next().is_some() {
            return Err(bad("qr_trailing"));
        }
        Ok(Self {
            addr,
            host_id,
            nonce_r,
            secret,
            host_version: host_version.to_string(),
        })
    }
}

/// The binding context, `lv_cat` of: the label, protocol 3, the iPad and
/// host versions, the method, `"ipad:" ‖ slot_id`, `"host:" ‖ host_id`,
/// `nonce_i` and `nonce_r`. It's the Noise prologue and the PSK's info.
pub fn pairing_ctx(
    ipad_version: &str,
    host_version: &str,
    slot_id: &Id16,
    host_id: &Id16,
    nonce_i: &Id16,
    nonce_r: &Id16,
) -> Vec<u8> {
    let ipad = [b"ipad:".as_slice(), slot_id].concat();
    let host = [b"host:".as_slice(), host_id].concat();
    lv_cat(&[
        PAIRING_LABEL,
        &[PROTOCOL_VERSION],
        ipad_version.as_bytes(),
        host_version.as_bytes(),
        METHOD_QR,
        &ipad,
        &host,
        nonce_i,
        nonce_r,
    ])
}

/// The QR pairing PSK: HKDF-SHA-256, salt `"EM v3 qr psk"`, IKM `S`, info
/// `SHA-256(ctx)`, 32 bytes.
pub fn qr_psk(secret: &Key32, ctx: &[u8]) -> Zeroizing<Key32> {
    let info = ring::digest::digest(&ring::digest::SHA256, ctx);
    hkdf32(QR_PSK_SALT, secret, info.as_ref())
}

/// HKDF-SHA-256 (RFC 5869) with a 32-byte output.
fn hkdf32(salt: &[u8], ikm: &[u8], info: &[u8]) -> Zeroizing<Key32> {
    struct Len;
    impl ring::hkdf::KeyType for Len {
        fn len(&self) -> usize {
            32
        }
    }
    let info = [info];
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, salt).extract(ikm);
    let okm = prk.expand(&info, Len).expect("32 bytes is a valid length");
    let mut out = Zeroizing::new([0u8; 32]);
    okm.fill(&mut *out).expect("lengths match");
    out
}

/// HMAC-SHA-256 (RFC 2104) over the concatenation of `parts`.
fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let mut ctx = ring::hmac::Context::with_key(&key);
    for part in parts {
        ctx.update(part);
    }
    ctx.sign()
        .as_ref()
        .try_into()
        .expect("HMAC-SHA-256 is 32 bytes")
}

fn mac(key: &Key32, label: &[u8], h: &[u8; 32]) -> [u8; 32] {
    hmac(key, &[label, h])
}

/// Compares in constant time.
fn mac_matches(key: &Key32, label: &[u8], h: &[u8; 32], tag: &[u8; 32]) -> bool {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::verify(&key, &[label, h.as_slice()].concat(), tag).is_ok()
}

/// `HMAC-SHA-256(K_new, "confirm" ‖ h)`.
pub fn confirm_mac(k_new: &Key32, h: &[u8; 32]) -> [u8; 32] {
    mac(k_new, b"confirm", h)
}

/// `HMAC-SHA-256(K_old, "replace" ‖ h)`.
pub fn replace_mac(k_old: &Key32, h: &[u8; 32]) -> [u8; 32] {
    mac(k_old, b"replace", h)
}

/// What the host's credential store answers during pairing. In-memory
/// lookups only, as for [`super::host::HostCredentials`].
pub trait PairCredentials {
    /// The slot's active record and its key, if it has one.
    fn active_in_slot(&self, slot_id: &Id16) -> Option<(Id16, Zeroizing<Key32>)>;
    /// The status of the slot `dev_id` belongs to.
    fn slot_status(&self, dev_id: &Id16) -> SlotStatus;
}

/// A pending record to store, the host's step 5. `Debug` leaves the key out.
pub struct PendingPairing {
    pub dev_id: Id16,
    pub slot_id: Id16,
    pub key: Box<Zeroizing<Key32>>,
    /// Set only after a valid replace proof from the slot's active key.
    pub replaces: Option<Id16>,
    pub device_name: String,
}

impl std::fmt::Debug for PendingPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPairing")
            .field("dev_id", &self.dev_id)
            .field("slot_id", &self.slot_id)
            .field("replaces", &self.replaces)
            .field("device_name", &self.device_name)
            .finish_non_exhaustive()
    }
}

/// How the caller's write of a [`PendingPairing`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreResult {
    Stored,
    /// It would exceed a cap: refused, terminally.
    TooMany,
    /// It failed (the store will be reopened): the iPad's next confirm tries
    /// again.
    Failed,
}

#[derive(Debug)]
pub enum PairOutput<A> {
    Send {
        to: A,
        datagram: Vec<u8>,
    },
    /// Write this record durably, then call [`PairingHost::stored`].
    Store(PendingPairing),
    /// The record is stored and the iPad told.
    Paired {
        dev_id: Id16,
        device_name: String,
    },
}

/// Timing and limits of the host's side.
#[derive(Debug, Clone)]
pub struct PairingConfig {
    /// How long a pairing waits for its `PairConfirm` after message 2.
    pub confirm_lifetime: Duration,
    /// How long a finished pairing still answers retried confirms.
    pub answered_lifetime: Duration,
    /// Shortest gap between two answers to one pairing's retries.
    pub reanswer_interval: Duration,
    /// Message 1s read per second from one source (its
    /// [`PeerAddr::rate_bucket`]), before the QR's PSK authenticates them.
    /// Sources beyond the table share one budget of the same size. There's
    /// no host-wide limit before authentication, as on the session path: one
    /// would let many senders inside their own limits starve a genuine iPad.
    /// A read that fails is one AEAD attempt and no Diffie-Hellman (about
    /// 10 µs), so the most the table admits, (1,024 + 1) × 20 a second, is
    /// about a fifth of one core.
    pub msg1_per_source_per_second: u32,
}

impl Default for PairingConfig {
    fn default() -> Self {
        Self {
            confirm_lifetime: Duration::from_secs(5),
            answered_lifetime: Duration::from_secs(30),
            reanswer_interval: Duration::from_millis(50),
            msg1_per_source_per_second: 20,
        }
    }
}

/// Message 1s one QR window remembers having handled. Each needs the QR's
/// secret to authenticate, so only its holder can fill it; once full, the
/// window takes no new pairing.
const MAX_HANDLED_MSG1: usize = 256;

struct Window {
    /// Which `open` made it: a write finishing for an earlier window never
    /// uses up a later one.
    generation: u64,
    secret: Zeroizing<Key32>,
    nonce_r: Id16,
    expires: Duration,
    /// SHA-256 of the Noise part of every message 1 that started a pairing
    /// under this window: a replay of one starts nothing.
    handled: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    AwaitingConfirm,
    Storing,
    Stored,
    Refused(u8),
}

struct Attempt<A> {
    /// The authenticated part of the message 1 it answered.
    msg1_noise: Vec<u8>,
    msg2: Vec<u8>,
    last_msg2: Duration,
    session_id: u32,
    sealer: Sealer,
    opener: Opener,
    h: [u8; 32],
    slot_id: Id16,
    device_name: String,
    dev_id: Id16,
    k_new: Zeroizing<Key32>,
    /// Where answers go: the source of the newest authenticated record.
    peer: A,
    step: Step,
    deadline: Duration,
    last_answer: Option<Duration>,
    /// The window it came from, and when that window's QR expires. Until a
    /// confirm authorizes the write, the attempt expires with it.
    window: u64,
    qr_expires: Duration,
    /// A valid confirm arrived before the QR expired: the write may go on
    /// (and be retried after a failure) past the QR's expiry.
    authorized: bool,
}

impl<A> Attempt<A> {
    /// Unconfirmed, and past its own deadline or its QR's expiry.
    fn expired(&self, now: Duration) -> bool {
        !self.authorized && (now >= self.deadline || now >= self.qr_expires)
    }
}

/// The host's side of QR pairing, as a sans-IO state machine like
/// [`super::host::HostSessions`]: one QR window and at most one pairing in
/// flight.
pub struct PairingHost<A: PeerAddr> {
    config: PairingConfig,
    host_id: Id16,
    host_version: String,
    host_name: String,
    rng: Box<dyn Random + Send>,
    window: Option<Window>,
    /// The generation the next window gets.
    next_generation: u64,
    attempt: Option<Attempt<A>>,
    /// Message 1 reads per source.
    per_source: SourceLimiter<A::Bucket>,
}

impl<A: PeerAddr> PairingHost<A> {
    pub fn new(
        config: PairingConfig,
        host_id: Id16,
        host_version: &str,
        host_name: &str,
        rng: Box<dyn Random + Send>,
    ) -> Self {
        assert!(wire::valid_version(host_version));
        Self {
            per_source: SourceLimiter::new(config.msg1_per_source_per_second),
            config,
            host_id,
            host_version: host_version.to_string(),
            host_name: host_name.to_string(),
            rng,
            window: None,
            next_generation: 1,
            attempt: None,
        }
    }

    /// Opens the pairing window with a fresh QR for `addr`. Any earlier QR's
    /// secret stops working, and a pairing under it that no confirm has
    /// authorized ends; one already authorized finishes its write, and only
    /// its own window is used up when it does.
    pub fn open(&mut self, now: Duration, addr: SocketAddr) -> QrPayload {
        let mut secret = Zeroizing::new([0u8; 32]);
        self.rng.fill(&mut *secret);
        let mut nonce_r = [0u8; 16];
        self.rng.fill(&mut nonce_r);
        let generation = self.next_generation;
        self.next_generation += 1;
        self.window = Some(Window {
            generation,
            secret: secret.clone(),
            nonce_r,
            expires: now + QR_LIFETIME,
            handled: Vec::new(),
        });
        self.attempt.take_if(|a| !a.authorized);
        QrPayload {
            addr,
            host_id: self.host_id,
            nonce_r,
            secret,
            host_version: self.host_version.clone(),
        }
    }

    /// The name message 2 gives the iPad from the next pairing on.
    pub fn set_host_name(&mut self, host_name: &str) {
        self.host_name = host_name.to_string();
    }

    /// Closes the window: its secret stops working, and a pairing under it
    /// that no confirm has authorized ends.
    pub fn close(&mut self) {
        self.window = None;
        self.attempt.take_if(|a| !a.authorized);
    }

    /// Ends the pairing in flight, if any, without answering it: for a store
    /// that can't be used any more, where it's unknown whether a write
    /// landed.
    pub fn abandon(&mut self) {
        self.attempt = None;
    }

    /// Whether a QR is open.
    pub fn is_open(&self) -> bool {
        self.window.is_some()
    }

    /// Whether a pairing is waiting for its confirm or being written at
    /// `now`: one a new message 1 can't replace.
    pub fn in_flight(&self, now: Duration) -> bool {
        self.attempt.as_ref().is_some_and(|a| {
            !a.expired(now) && matches!(a.step, Step::AwaitingConfirm | Step::Storing)
        })
    }

    /// The `session_id` of the pairing in flight, if any: its records go to
    /// [`handle_datagram`](Self::handle_datagram).
    pub fn session_id(&self) -> Option<u32> {
        self.attempt.as_ref().map(|a| a.session_id)
    }

    /// Handles a pairing message 1, or a record of the pairing in flight.
    /// Anything else is ignored.
    pub fn handle_datagram(
        &mut self,
        now: Duration,
        from: &A,
        datagram: &[u8],
        creds: &dyn PairCredentials,
    ) -> Vec<PairOutput<A>> {
        let mut out = Vec::new();
        match wire::classify(datagram) {
            V3Kind::Handshake1 if wire::extension_tag(datagram) == Some(EXT_PAIR_QR) => {
                self.handle_msg1(now, from, datagram, &mut out)
            }
            V3Kind::Sealed => self.handle_record(now, from, datagram, creds, &mut out),
            _ => {}
        }
        out
    }

    fn handle_msg1(
        &mut self,
        now: Duration,
        from: &A,
        datagram: &[u8],
        out: &mut Vec<PairOutput<A>>,
    ) {
        let Some(window) = self.window.as_ref().filter(|w| now < w.expires) else {
            return;
        };
        let Ok(pair1) = wire::parse_pair1(datagram) else {
            return;
        };
        if let Some(attempt) = self
            .attempt
            .as_mut()
            .filter(|a| a.msg1_noise == pair1.noise)
        {
            // The same message 1 again: the same message 2, at most every
            // 100 ms.
            if attempt.step == Step::AwaitingConfirm
                && now.saturating_sub(attempt.last_msg2) >= Duration::from_millis(100)
            {
                attempt.last_msg2 = now;
                out.push(PairOutput::Send {
                    to: from.clone(),
                    datagram: attempt.msg2.clone(),
                });
            }
            return;
        }
        // One pairing at a time: one waiting for its confirm or being
        // written stays until it ends; a message 1 never displaces it. One
        // that's only answering retries gives way to a new pairing.
        self.attempt.take_if(|a| a.expired(now));
        if self
            .attempt
            .as_ref()
            .is_some_and(|a| matches!(a.step, Step::AwaitingConfirm | Step::Storing))
        {
            return;
        }
        // A message 1 that already started a pairing under this QR starts
        // nothing again, even once that pairing is over.
        let digest: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, pair1.noise)
            .as_ref()
            .try_into()
            .expect("32 bytes");
        if window.handled.contains(&digest) || window.handled.len() >= MAX_HANDLED_MSG1 {
            return;
        }
        // One source can't use up everyone's reads.
        if !self.per_source.allow(now, &from.rate_bucket()) {
            return;
        }
        let ctx = pairing_ctx(
            pair1.ipad_version,
            &self.host_version,
            &pair1.slot_id,
            &self.host_id,
            &pair1.nonce_i,
            &window.nonce_r,
        );
        let psk = qr_psk(&window.secret, &ctx);
        let Ok(mut state) = builder(&psk, &ctx).build_responder() else {
            return;
        };
        let mut payload = Zeroizing::new(vec![0u8; MAX_NOISE_MESSAGE]);
        let Ok(len) = state.read_message(pair1.noise, &mut payload) else {
            return;
        };
        let Ok(hello) = PairHello::parse(&payload[..len]) else {
            return;
        };
        let mut dev_id = [0u8; 16];
        self.rng.fill(&mut dev_id);
        let mut k_new = Zeroizing::new([0u8; 32]);
        self.rng.fill(&mut *k_new);
        let session_id = loop {
            let mut bytes = [0u8; 4];
            self.rng.fill(&mut bytes);
            let id = u32::from_le_bytes(bytes);
            if id != 0 {
                break id;
            }
        };
        let accept = PairAccept {
            session_id,
            dev_id,
            k_new: k_new.clone(),
            host_name: self.host_name.clone(),
        };
        let mut noise = vec![0u8; MAX_NOISE_MESSAGE];
        let Ok(len) = state.write_message(&accept.encode(), &mut noise) else {
            return;
        };
        noise.truncate(len);
        let msg2 = wire::encode_pair2(&noise);
        let keys = split(&mut state);
        let h = *keys.handshake_hash();
        let (sealer, opener) = keys.responder(session_id);
        let window = self.window.as_mut().expect("checked above");
        window.handled.push(digest);
        let (generation, qr_expires) = (window.generation, window.expires);
        out.push(PairOutput::Send {
            to: from.clone(),
            datagram: msg2.clone(),
        });
        self.attempt = Some(Attempt {
            msg1_noise: pair1.noise.to_vec(),
            msg2,
            last_msg2: now,
            session_id,
            sealer,
            opener,
            h,
            slot_id: pair1.slot_id,
            device_name: hello.device_name,
            dev_id,
            k_new,
            peer: from.clone(),
            step: Step::AwaitingConfirm,
            deadline: now + self.config.confirm_lifetime,
            last_answer: None,
            window: generation,
            qr_expires,
            authorized: false,
        });
    }

    fn handle_record(
        &mut self,
        now: Duration,
        from: &A,
        datagram: &[u8],
        creds: &dyn PairCredentials,
        out: &mut Vec<PairOutput<A>>,
    ) {
        // An unconfirmed pairing ends with its QR, or its own deadline,
        // before any record of it is read.
        self.attempt.take_if(|a| a.expired(now));
        let Some(attempt) = self.attempt.as_mut() else {
            return;
        };
        let Ok(header) = RecordHeader::parse(datagram) else {
            return;
        };
        if header.session_id != attempt.session_id {
            return;
        }
        let Ok(opened) = attempt.opener.open(datagram) else {
            return;
        };
        attempt.peer = from.clone();
        match PairMessage::parse(opened.inner_type, &opened.body) {
            Ok(Some(PairMessage::Confirm {
                confirm_mac: tag,
                replace,
            })) => {
                if !mac_matches(&attempt.k_new, b"confirm", &attempt.h, &tag) {
                    return;
                }
                match attempt.step {
                    Step::AwaitingConfirm => {
                        // `replaces` only for the slot's active record, with
                        // a valid proof from its key.
                        let replaces = replace.and_then(|(old, proof)| {
                            let (active, k_old) = creds.active_in_slot(&attempt.slot_id)?;
                            (active == old && mac_matches(&k_old, b"replace", &attempt.h, &proof))
                                .then_some(old)
                        });
                        attempt.step = Step::Storing;
                        attempt.authorized = true;
                        out.push(PairOutput::Store(PendingPairing {
                            dev_id: attempt.dev_id,
                            slot_id: attempt.slot_id,
                            key: Box::new(attempt.k_new.clone()),
                            replaces,
                            device_name: attempt.device_name.clone(),
                        }));
                    }
                    Step::Storing => {}
                    Step::Stored | Step::Refused(_) => {
                        if attempt.last_answer.is_none_or(|at| {
                            now.saturating_sub(at) >= self.config.reanswer_interval
                        }) {
                            Self::answer(now, attempt, creds, out);
                        }
                    }
                }
            }
            Ok(Some(PairMessage::Abort)) if attempt.step == Step::AwaitingConfirm => {
                self.attempt = None;
            }
            _ => {}
        }
    }

    /// Seals the answer the pairing has: `PairDone` with the slot status, or
    /// `PairRefused`.
    fn answer(
        now: Duration,
        attempt: &mut Attempt<A>,
        creds: &dyn PairCredentials,
        out: &mut Vec<PairOutput<A>>,
    ) {
        let message = match attempt.step {
            Step::Stored => PairMessage::Done(creds.slot_status(&attempt.dev_id)),
            Step::Refused(reason) => PairMessage::Refused(reason),
            Step::AwaitingConfirm | Step::Storing => return,
        };
        if let Ok(sealed) = attempt
            .sealer
            .seal(message.inner_type(), &message.encode_body())
        {
            attempt.last_answer = Some(now);
            out.push(PairOutput::Send {
                to: attempt.peer.clone(),
                datagram: sealed,
            });
        }
    }

    /// The caller's write of the [`PairOutput::Store`] for `dev_id` ended.
    /// Stored: `PairDone` goes out and the QR's secret is used up. Too many:
    /// `PairRefused`. Failed: the next retried confirm writes again.
    pub fn stored(
        &mut self,
        now: Duration,
        dev_id: &Id16,
        result: StoreResult,
        creds: &dyn PairCredentials,
    ) -> Vec<PairOutput<A>> {
        let mut out = Vec::new();
        let Some(attempt) = self
            .attempt
            .as_mut()
            .filter(|a| a.dev_id == *dev_id && a.step == Step::Storing)
        else {
            return out;
        };
        match result {
            StoreResult::Stored => {
                attempt.step = Step::Stored;
                attempt.deadline = now + self.config.answered_lifetime;
                // Its own QR is used up; a newer one opened meanwhile stays.
                let generation = attempt.window;
                self.window.take_if(|w| w.generation == generation);
                Self::answer(now, attempt, creds, &mut out);
                out.push(PairOutput::Paired {
                    dev_id: *dev_id,
                    device_name: attempt.device_name.clone(),
                });
            }
            StoreResult::TooMany => {
                attempt.step = Step::Refused(PAIR_REFUSED_TOO_MANY);
                attempt.deadline = now + self.config.answered_lifetime;
                Self::answer(now, attempt, creds, &mut out);
            }
            StoreResult::Failed => {
                attempt.step = Step::AwaitingConfirm;
                attempt.deadline = now + self.config.confirm_lifetime;
            }
        }
        out
    }

    /// Runs the timers: the window's expiry, and the end of a pairing that
    /// got no confirm or whose answers are no longer needed. A pairing being
    /// stored waits for its write.
    pub fn poll(&mut self, now: Duration) {
        if self.window.as_ref().is_some_and(|w| now >= w.expires) {
            self.window = None;
        }
        self.per_source.prune(now);
        self.attempt
            .take_if(|a| a.expired(now) || (a.step != Step::Storing && now >= a.deadline));
    }
}

/// The iPad's side of QR pairing: the reference for phase 3, and the
/// counterpart in tests.
pub struct PairingClient {
    state: Option<HandshakeState>,
    msg1: Vec<u8>,
    session: Option<(Sealer, Opener, [u8; 32])>,
    accept: Option<PairAccept>,
}

impl std::fmt::Debug for PairingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingClient").finish_non_exhaustive()
    }
}

/// What the host answered a confirm with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairResult {
    /// Stored: the key is pending on the host. Connect with it.
    Done(SlotStatus),
    /// Nothing was stored: delete `K_new` and show why.
    Refused(u8),
}

impl PairingClient {
    /// Starts pairing from a scanned QR. `slot_id` is the iPad's place on
    /// this host (kept per `host_id`); `nonce_i` is fresh.
    pub fn start(
        qr: &QrPayload,
        slot_id: &Id16,
        ipad_version: &str,
        device_name: &str,
        listen_port: u16,
        rng: &mut dyn Random,
    ) -> Result<Self, V3Error> {
        let mut nonce_i = [0u8; 16];
        rng.fill(&mut nonce_i);
        let mut client_nonce = [0u8; 4];
        rng.fill(&mut client_nonce);
        let ctx = pairing_ctx(
            ipad_version,
            &qr.host_version,
            slot_id,
            &qr.host_id,
            &nonce_i,
            &qr.nonce_r,
        );
        let psk = qr_psk(&qr.secret, &ctx);
        let mut state = builder(&psk, &ctx).build_initiator().map_err(map_snow)?;
        let hello = PairHello {
            device_name: device_name.to_string(),
        };
        let mut noise = vec![0u8; MAX_NOISE_MESSAGE];
        let len = state
            .write_message(&hello.encode(), &mut noise)
            .map_err(map_snow)?;
        noise.truncate(len);
        let msg1 = wire::encode_pair1(
            u32::from_le_bytes(client_nonce),
            listen_port,
            slot_id,
            &nonce_i,
            ipad_version,
            &noise,
        );
        Ok(Self {
            state: Some(state),
            msg1,
            session: None,
            accept: None,
        })
    }

    /// Message 1; retries resend these bytes.
    pub fn msg1(&self) -> &[u8] {
        &self.msg1
    }

    /// Reads message 2. On failure nothing changes. On success the iPad
    /// stores `dev_id` and `K_new` durably, then calls
    /// [`confirm`](Self::confirm), or [`abort`](Self::abort) if it couldn't.
    pub fn read_msg2(&mut self, datagram: &[u8]) -> Result<&PairAccept, V3Error> {
        let noise = wire::parse_pair2(datagram)?;
        let state = self.state.as_mut().ok_or(V3Error::Handshake)?;
        let mut payload = Zeroizing::new(vec![0u8; MAX_NOISE_MESSAGE]);
        let len = state.read_message(noise, &mut payload).map_err(map_snow)?;
        let accept = PairAccept::parse(&payload[..len])?;
        let mut state = self.state.take().expect("checked above");
        let keys = split(&mut state);
        let h = *keys.handshake_hash();
        let (sealer, opener) = keys.initiator(accept.session_id);
        self.session = Some((sealer, opener, h));
        Ok(self.accept.insert(accept))
    }

    /// A sealed `PairConfirm`, once `K_new` is stored. `replace` is the key
    /// this iPad believes is the slot's active one, if any. Each call is a
    /// fresh record, for retries.
    pub fn confirm(&mut self, replace: Option<&ClientKey>) -> Result<Vec<u8>, V3Error> {
        let (Some((sealer, _, h)), Some(accept)) = (self.session.as_mut(), self.accept.as_ref())
        else {
            return Err(V3Error::Handshake);
        };
        let message = PairMessage::Confirm {
            confirm_mac: confirm_mac(&accept.k_new, h),
            replace: replace.map(|old| (old.dev_id, replace_mac(&old.k_dev, h))),
        };
        sealer.seal(message.inner_type(), &message.encode_body())
    }

    /// A sealed `PairAbort`: the iPad couldn't store `K_new`.
    pub fn abort(&mut self) -> Result<Vec<u8>, V3Error> {
        let (sealer, _, _) = self.session.as_mut().ok_or(V3Error::Handshake)?;
        let message = PairMessage::Abort;
        sealer.seal(message.inner_type(), &message.encode_body())
    }

    /// The host's answer, if `datagram` is one.
    pub fn handle_record(&mut self, datagram: &[u8]) -> Option<PairResult> {
        let (_, opener, _) = self.session.as_mut()?;
        let opened = opener.open(datagram).ok()?;
        match PairMessage::parse(opened.inner_type, &opened.body) {
            Ok(Some(PairMessage::Done(status))) => Some(PairResult::Done(status)),
            Ok(Some(PairMessage::Refused(reason))) => Some(PairResult::Refused(reason)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
