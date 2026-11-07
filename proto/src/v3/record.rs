//! Sealed records: the 20-byte header, ChaCha20-Poly1305 with the header as
//! AAD, explicit per-direction counters, and the replay window.
//!
//! See the [module docs](super) for the layout. A [`Sealer`] owns one
//! direction's send key and counter; an [`Opener`] owns the other direction's
//! receive key and replay window. Neither knows about sockets or time.

use std::sync::Mutex;

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};

use super::replay::ReplayWindow;
use super::{Key32, V3Error, MAX_RECORDS_PER_DIRECTION, SEALED_TYPE};
use crate::v2::{WireError, MAGIC, PREFIX_SIZE, VERSION};

/// Common prefix, `session_id` and counter.
pub const RECORD_HEADER_SIZE: usize = PREFIX_SIZE + 4 + 8;
/// Poly1305 tag.
pub const TAG_SIZE: usize = 16;
/// Header, inner type byte and tag: a record is this much larger than the
/// message body it carries.
pub const RECORD_OVERHEAD: usize = RECORD_HEADER_SIZE + 1 + TAG_SIZE;

/// The cleartext part of a record, all of it authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    pub session_id: u32,
    pub counter: u64,
}

impl RecordHeader {
    /// Reads the header and checks the framing. The AEAD isn't touched.
    pub fn parse(datagram: &[u8]) -> Result<Self, WireError> {
        if datagram.len() < RECORD_HEADER_SIZE + 1 + TAG_SIZE {
            return Err(WireError::Truncated);
        }
        if datagram[0..2] != MAGIC.to_le_bytes() {
            return Err(WireError::BadMagic);
        }
        if datagram[2] != VERSION {
            return Err(WireError::UnsupportedVersion(datagram[2]));
        }
        if datagram[3] != SEALED_TYPE {
            return Err(WireError::UnknownType(datagram[3]));
        }
        if datagram[4] != 0 || datagram[5] != 0 {
            return Err(WireError::InvalidField("flags"));
        }
        let payload_len = usize::from(u16::from_le_bytes([datagram[6], datagram[7]]));
        if payload_len != datagram.len() - RECORD_HEADER_SIZE {
            return Err(WireError::LengthMismatch);
        }
        let session_id = u32::from_le_bytes(datagram[8..12].try_into().unwrap());
        if session_id == 0 {
            return Err(WireError::InvalidField("session_id"));
        }
        Ok(Self {
            session_id,
            counter: u64::from_le_bytes(datagram[12..20].try_into().unwrap()),
        })
    }

    fn encode_into(self, out: &mut [u8], payload_len: usize) {
        out[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        out[2] = VERSION;
        out[3] = SEALED_TYPE;
        out[4] = 0;
        out[5] = 0;
        let len = u16::try_from(payload_len).expect("record fits a datagram");
        out[6..8].copy_from_slice(&len.to_le_bytes());
        out[8..12].copy_from_slice(&self.session_id.to_le_bytes());
        out[12..20].copy_from_slice(&self.counter.to_le_bytes());
    }
}

/// The Noise ChaChaPoly nonce: 32 zero bits, then the counter little-endian.
fn nonce(counter: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&counter.to_le_bytes());
    Nonce::assume_unique_for_key(bytes)
}

fn aead_key(key: &Key32) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("32-byte key"))
}

/// One AEAD operation with the record nonce, for the vector tests.
#[cfg(test)]
pub(crate) fn seal_raw(key: &Key32, counter: u64, aad: &[u8], in_out: &mut Vec<u8>) {
    aead_key(key)
        .seal_in_place_append_tag(nonce(counter), Aad::from(aad), in_out)
        .expect("plaintext within AEAD limits");
}

/// Opens one AEAD operation in place, returning the plaintext length.
#[cfg(test)]
pub(crate) fn open_raw(
    key: &Key32,
    counter: u64,
    aad: &[u8],
    in_out: &mut [u8],
) -> Result<usize, V3Error> {
    aead_key(key)
        .open_in_place(nonce(counter), Aad::from(aad), in_out)
        .map(|plain| plain.len())
        .map_err(|_| V3Error::Auth)
}

/// One direction's sending half. Only [`SessionKeys`] makes one in shipped
/// code, and it makes exactly one per key, so a counter never repeats under a
/// key.
pub struct Sealer {
    key: LessSafeKey,
    session_id: u32,
    next: u64,
}

impl Sealer {
    fn from_key(key: &Key32, session_id: u32) -> Self {
        Self {
            key: aead_key(key),
            session_id,
            next: 0,
        }
    }

    /// A sealer on a caller's key, for tests, benchmarks and fuzzing only.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn new(key: &Key32, session_id: u32) -> Self {
        Self::from_key(key, session_id)
    }

    /// Moves the next counter forward, for tests of the counter limits. It
    /// can't move backwards.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn skip_to(&mut self, counter: u64) {
        self.next = self.next.max(counter);
    }

    pub fn session_id(&self) -> u32 {
        self.session_id
    }

    /// Records sealed so far.
    pub fn sealed(&self) -> u64 {
        self.next
    }

    /// Seals `inner_type || body` as one record into `out`, replacing its
    /// contents. Each call uses the next counter, so a retransmit sealed again
    /// never reuses a nonce.
    pub fn seal_into(
        &mut self,
        inner_type: u8,
        body: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(), V3Error> {
        if self.next >= MAX_RECORDS_PER_DIRECTION {
            return Err(V3Error::Exhausted);
        }
        let header = RecordHeader {
            session_id: self.session_id,
            counter: self.next,
        };
        self.next += 1;
        out.clear();
        out.resize(RECORD_HEADER_SIZE, 0);
        header.encode_into(out, 1 + body.len() + TAG_SIZE);
        out.push(inner_type);
        out.extend_from_slice(body);
        let (aad, plaintext) = out.split_at_mut(RECORD_HEADER_SIZE);
        let tag = self
            .key
            .seal_in_place_separate_tag(nonce(header.counter), Aad::from(&*aad), plaintext)
            .expect("plaintext within AEAD limits");
        out.extend_from_slice(tag.as_ref());
        Ok(())
    }

    pub fn seal(&mut self, inner_type: u8, body: &[u8]) -> Result<Vec<u8>, V3Error> {
        let mut out = Vec::with_capacity(RECORD_OVERHEAD + body.len());
        self.seal_into(inner_type, body, &mut out)?;
        Ok(out)
    }
}

/// An authenticated record's content. `Debug` shows the body's length, not
/// its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Opened {
    pub counter: u64,
    pub inner_type: u8,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("counter", &self.counter)
            .field("inner_type", &self.inner_type)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// One direction's receiving half. `open` takes `&self` and is safe to call
/// from several threads; the replay window has its own lock.
pub struct Opener {
    key: LessSafeKey,
    session_id: u32,
    window: Mutex<ReplayWindow>,
}

impl Opener {
    fn from_key(key: &Key32, session_id: u32) -> Self {
        Self {
            key: aead_key(key),
            session_id,
            window: Mutex::new(ReplayWindow::new()),
        }
    }

    /// An opener on a caller's key, for tests, benchmarks and fuzzing only.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn new(key: &Key32, session_id: u32) -> Self {
        Self::from_key(key, session_id)
    }

    pub fn session_id(&self) -> u32 {
        self.session_id
    }

    /// The highest counter that has authenticated so far. Only authenticated
    /// records move it.
    pub fn highest(&self) -> Option<u64> {
        self.window.lock().unwrap().top()
    }

    /// Opens a record in place. On success returns the counter, the inner
    /// type and the body, which borrows from `datagram`. In order:
    /// 1. framing, the session id and the counter limit;
    /// 2. a read-only replay check;
    /// 3. the AEAD;
    /// 4. check and mark the counter, under the window's lock.
    ///
    /// A failure at any step leaves the window as it was.
    pub fn open_in_place<'a>(
        &self,
        datagram: &'a mut [u8],
    ) -> Result<(u64, u8, &'a [u8]), V3Error> {
        let header = RecordHeader::parse(datagram)?;
        if header.session_id != self.session_id {
            return Err(V3Error::Wire(WireError::InvalidField("session_id")));
        }
        if header.counter >= MAX_RECORDS_PER_DIRECTION {
            return Err(V3Error::Replay);
        }
        if !self.window.lock().unwrap().check(header.counter) {
            return Err(V3Error::Replay);
        }
        let (aad, sealed) = datagram.split_at_mut(RECORD_HEADER_SIZE);
        let plain = self
            .key
            .open_in_place(nonce(header.counter), Aad::from(&*aad), sealed)
            .map_err(|_| V3Error::Auth)?;
        if !self.window.lock().unwrap().check_and_mark(header.counter) {
            return Err(V3Error::Replay);
        }
        let (inner_type, body) = plain.split_first().ok_or(V3Error::Auth)?;
        Ok((header.counter, *inner_type, body))
    }

    /// [`open_in_place`](Self::open_in_place) on a copy.
    pub fn open(&self, datagram: &[u8]) -> Result<Opened, V3Error> {
        let mut copy = datagram.to_vec();
        let (counter, inner_type, body) = self.open_in_place(&mut copy)?;
        Ok(Opened {
            counter,
            inner_type,
            body: body.to_vec(),
        })
    }
}

/// The two directions' keys from Noise `Split()`, and the handshake hash.
/// Only the handshake makes one. Turning it into a sealer and an opener
/// consumes it, so each key gets exactly one sealer.
pub struct SessionKeys {
    /// Initiator (iPad) → responder (host).
    pub(crate) initiator_to_responder: Key32,
    /// Responder (host) → initiator (iPad).
    pub(crate) responder_to_initiator: Key32,
    pub(crate) handshake_hash: [u8; 32],
}

impl SessionKeys {
    /// Keys from raw bytes, for tests and fuzzing only.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn from_raw(initiator_to_responder: Key32, responder_to_initiator: Key32) -> Self {
        Self {
            initiator_to_responder,
            responder_to_initiator,
            handshake_hash: [0; 32],
        }
    }

    /// The Noise handshake hash `h`, which later pairing messages bind to.
    pub fn handshake_hash(&self) -> &[u8; 32] {
        &self.handshake_hash
    }

    /// The iPad's halves for `session_id`.
    pub fn initiator(self, session_id: u32) -> (Sealer, Opener) {
        (
            Sealer::from_key(&self.initiator_to_responder, session_id),
            Opener::from_key(&self.responder_to_initiator, session_id),
        )
    }

    /// The host's halves for `session_id`.
    pub fn responder(self, session_id: u32) -> (Sealer, Opener) {
        (
            Sealer::from_key(&self.responder_to_initiator, session_id),
            Opener::from_key(&self.initiator_to_responder, session_id),
        )
    }
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKeys").finish_non_exhaustive()
    }
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        wipe(&mut self.initiator_to_responder);
        wipe(&mut self.responder_to_initiator);
    }
}

/// Overwrites key bytes so a dropped copy doesn't linger.
pub(crate) fn wipe(bytes: &mut [u8]) {
    zeroize::Zeroize::zeroize(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Sealer, Opener) {
        (Sealer::new(&[7; 32], 42), Opener::new(&[7; 32], 42))
    }

    #[test]
    fn seal_open_round_trip_and_overhead() {
        let (mut sealer, opener) = pair();
        let body = vec![0xAB; 1200];
        let record = sealer.seal(0x20, &body).unwrap();
        assert_eq!(record.len(), body.len() + RECORD_OVERHEAD);
        assert_eq!(RECORD_OVERHEAD, 37);
        assert_eq!(
            RecordHeader::parse(&record).unwrap(),
            RecordHeader {
                session_id: 42,
                counter: 0
            }
        );
        let opened = opener.open(&record).unwrap();
        assert_eq!((opened.counter, opened.inner_type), (0, 0x20));
        assert_eq!(opened.body, body);
        let second = sealer.seal(0x20, &body).unwrap();
        assert_eq!(opener.open(&second).unwrap().counter, 1);
        assert_ne!(
            record[20..],
            second[20..],
            "fresh counter, fresh ciphertext"
        );
    }

    #[test]
    fn every_bit_of_a_record_is_authenticated() {
        let (mut sealer, _) = pair();
        let record = sealer.seal(0x13, &[1, 2, 3]).unwrap();
        for at in 0..record.len() {
            for bit in 0..8 {
                let mut bad = record.clone();
                bad[at] ^= 1 << bit;
                let opener = Opener::new(&[7; 32], 42);
                assert!(opener.open(&bad).is_err(), "byte {at} bit {bit}");
                // The failed attempt didn't move the window.
                assert!(opener.open(&record).is_ok(), "byte {at} bit {bit}");
            }
        }
    }

    #[test]
    fn duplicates_and_old_records_are_dropped_reorders_kept() {
        let (mut sealer, opener) = pair();
        let records: Vec<_> = (0..5000).map(|_| sealer.seal(1, b"x").unwrap()).collect();
        assert!(opener.open(&records[4999]).is_ok());
        assert_eq!(opener.open(&records[4999]), Err(V3Error::Replay));
        assert!(
            opener.open(&records[4999 - 4095]).is_ok(),
            "inside the window"
        );
        assert_eq!(opener.open(&records[4999 - 4096]), Err(V3Error::Replay));
        assert!(opener.open(&records[4000]).is_ok());
    }

    #[test]
    fn wrong_key_session_or_direction_fails() {
        let (mut sealer, _) = pair();
        let record = sealer.seal(1, b"x").unwrap();
        assert_eq!(Opener::new(&[8; 32], 42).open(&record), Err(V3Error::Auth));
        assert!(Opener::new(&[7; 32], 43).open(&record).is_err());
        let (mut ipad_send, ipad_recv) = SessionKeys::from_raw([1; 32], [2; 32]).initiator(5);
        let (mut host_send, host_recv) = SessionKeys::from_raw([1; 32], [2; 32]).responder(5);
        let up = ipad_send.seal(1, b"up").unwrap();
        let down = host_send.seal(1, b"down").unwrap();
        assert_eq!(host_recv.open(&up).unwrap().body, b"up");
        assert_eq!(ipad_recv.open(&down).unwrap().body, b"down");
        let reflected = ipad_send.seal(1, b"up").unwrap();
        assert_eq!(
            ipad_recv.open(&reflected),
            Err(V3Error::Auth),
            "reflected record"
        );
    }

    #[test]
    fn counter_limit_stops_sealing_and_opening() {
        let (mut sealer, opener) = pair();
        sealer.next = MAX_RECORDS_PER_DIRECTION - 1;
        let last = sealer.seal(1, b"x").unwrap();
        assert!(opener.open(&last).is_ok());
        assert_eq!(sealer.seal(1, b"x").unwrap_err(), V3Error::Exhausted);

        let mut forged = last.clone();
        forged[12..20].copy_from_slice(&MAX_RECORDS_PER_DIRECTION.to_le_bytes());
        assert_eq!(opener.open(&forged), Err(V3Error::Replay));
    }

    #[test]
    fn framing_rejects_short_flags_and_length_mismatch() {
        let (mut sealer, _) = pair();
        let record = sealer.seal(1, b"").unwrap();
        assert_eq!(record.len(), RECORD_OVERHEAD);
        assert!(RecordHeader::parse(&record).is_ok());
        assert_eq!(
            RecordHeader::parse(&record[..36]),
            Err(WireError::Truncated)
        );
        let mut flags = record.clone();
        flags[4] = 1;
        assert!(RecordHeader::parse(&flags).is_err());
        let mut longer = record.clone();
        longer.push(0);
        assert_eq!(RecordHeader::parse(&longer), Err(WireError::LengthMismatch));
        let mut zero_session = record.clone();
        zero_session[8..12].fill(0);
        assert!(RecordHeader::parse(&zero_session).is_err());
    }

    #[test]
    fn concurrent_open_of_the_same_record_passes_once() {
        use std::sync::{Arc, Barrier};
        let (mut sealer, opener) = pair();
        let opener = Arc::new(opener);
        let records: Arc<Vec<_>> =
            Arc::new((0..500).map(|_| sealer.seal(1, b"x").unwrap()).collect());
        let barrier = Arc::new(Barrier::new(6));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (opener, records, barrier) = (opener.clone(), records.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    records.iter().filter(|r| opener.open(r).is_ok()).count()
                })
            })
            .collect();
        let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 500);
    }

    #[test]
    fn debug_never_prints_plaintext() {
        let (mut sealer, opener) = pair();
        let record = sealer.seal(0x20, b"secret keystrokes").unwrap();
        let opened = opener.open(&record).unwrap();
        let text = format!("{opened:?}");
        assert!(!text.contains("115, 101, 99"), "{text}"); // "sec"
        assert!(text.contains("body_len: 17"), "{text}");
    }

    #[test]
    fn highest_moves_only_on_authenticated_records() {
        let (mut sealer, opener) = pair();
        assert_eq!(opener.highest(), None);
        sealer.skip_to(1000);
        let record = sealer.seal(1, b"x").unwrap();
        let mut forged = record.clone();
        *forged.last_mut().unwrap() ^= 1;
        assert!(opener.open(&forged).is_err());
        assert_eq!(opener.highest(), None);
        opener.open(&record).unwrap();
        assert_eq!(opener.highest(), Some(1000));
        sealer.skip_to(5);
        assert_eq!(sealer.sealed(), 1001, "skip_to never moves back");
    }
}
