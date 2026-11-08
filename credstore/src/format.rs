//! The two byte formats the store keeps: the credential database file and
//! the keyring blob.
//!
//! Both parsers are total: they return a value or a [`FormatError`], never
//! panic, and never read past their input. Every count is bounded before
//! anything is allocated for it.
//!
//! # Database file (`credentials.db`)
//!
//! ```text
//! magic "EMV3CRED", version u8 = 1, generation u64
//! record count u16 (≤ MAX_RECORDS), then each record:
//!   dev_id 16, slot_id 16, state u8 (0 pending, 1 active),
//!   flags u8 (bit 0: replaces present), [replaces 16],
//!   created_at_s u64, committed_timestamp_us u64,
//!   name length u8 (≤ 64) + UTF-8 name,
//!   wrap nonce 12, wrapped key 48 (ChaCha20-Poly1305 of K_dev)
//! tombstone slot count u16 (≤ MAX_TOMBSTONE_SLOTS), then each slot:
//!   slot_id 16, count u8 (≤ 8), dev_id 16 × count (oldest first)
//! mac 32: HMAC-SHA-256 over every byte before it
//! ```
//!
//! Integers are little-endian. The wrap key and the MAC key are derived from
//! the generation's wrapping key `W` with HKDF-SHA-256; each wrap binds
//! `(dev_id, generation)` as associated data. `dev_id`s, slots and tombstones
//! are readable without `W`, so records that can't be opened can still be
//! listed and tombstoned.
//!
//! # Keyring blob
//!
//! ```text
//! magic "EMV3KRNG", version u8 = 1
//! committed generation u64, committed W 32
//! staged flag u8 (0 or 1), [staged generation u64, staged W 32]
//! intent flag u8 (0 or 1), [op_id 16,
//!   invalidate count u8 (≤ MAX_RECORDS) + dev_id 16 × count,
//!   promote count u8 (≤ MAX_RECORDS) + dev_id 16 × count]
//! ```
//!
//! A staged key and an intent are present together or not at all, and the
//! staged generation is the committed one plus one. The blob lives only in
//! OS-protected storage, which protects it at rest.

use eternal_wire::v3::wire::RecordState;
use eternal_wire::v3::{Id16, Key32, Zeroizing};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::{hkdf, hmac};

/// Records a host holds at most, across every slot.
pub const MAX_RECORDS: usize = eternal_wire::v3::host::MAX_CREDENTIALS;
/// Tombstones a slot keeps.
pub const MAX_TOMBSTONES_PER_SLOT: usize = 8;
/// Slots whose tombstones are kept. Beyond this, the slot that changed
/// longest ago and holds no record is forgotten.
pub const MAX_TOMBSTONE_SLOTS: usize = 1024;
/// Longest device name, in UTF-8 bytes.
pub const MAX_NAME_LEN: usize = 64;
/// Largest generation either format accepts. One rotation per user action
/// never gets near it; the bound keeps `generation + 1` from overflowing.
pub const MAX_GENERATION: u64 = 1 << 62;

const DB_MAGIC: &[u8; 8] = b"EMV3CRED";
const KEYRING_MAGIC: &[u8; 8] = b"EMV3KRNG";
const VERSION: u8 = 1;
const MAC_LEN: usize = 32;
const WRAPPED_LEN: usize = 32 + 16;
const HKDF_SALT: &[u8] = b"EM v3 credential store";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    Truncated,
    BadMagic,
    UnsupportedVersion(u8),
    /// A field holds a value the format forbids.
    InvalidField(&'static str),
    /// Bytes after the end of the structure.
    TrailingBytes,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "truncated"),
            Self::BadMagic => write!(f, "bad magic"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported version {v}"),
            Self::InvalidField(name) => write!(f, "invalid field: {name}"),
            Self::TrailingBytes => write!(f, "trailing bytes"),
        }
    }
}

impl std::error::Error for FormatError {}

/// One record as stored: everything but the plaintext key.
#[derive(Clone, PartialEq, Eq)]
pub struct DiskRecord {
    pub dev_id: Id16,
    pub slot_id: Id16,
    pub state: RecordState,
    pub replaces: Option<Id16>,
    pub created_at_s: u64,
    pub committed_timestamp_us: u64,
    pub device_name: String,
    pub wrap_nonce: [u8; 12],
    pub wrapped_key: [u8; WRAPPED_LEN],
}

impl std::fmt::Debug for DiskRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskRecord")
            .field("dev_id", &self.dev_id)
            .field("slot_id", &self.slot_id)
            .field("state", &self.state)
            .field("replaces", &self.replaces)
            .finish_non_exhaustive()
    }
}

/// A database file, parsed but not yet authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbFile {
    pub generation: u64,
    pub records: Vec<DiskRecord>,
    /// Per slot, oldest first.
    pub tombstones: Vec<(Id16, Vec<Id16>)>,
}

/// The keyring's contents.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyringState {
    pub committed: (u64, Zeroizing<Key32>),
    /// A rotation in progress: the next generation's key and what it does.
    pub staged: Option<(u64, Zeroizing<Key32>, Intent)>,
}

impl std::fmt::Debug for KeyringState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringState")
            .field("committed_generation", &self.committed.0)
            .field("staged", &self.staged.as_ref().map(|(g, _, i)| (g, i)))
            .finish()
    }
}

/// What a rotation does: the records it invalidates and the pending
/// records it makes active.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Intent {
    pub op_id: Id16,
    pub invalidate: Vec<Id16>,
    pub promote: Vec<Id16>,
}

/// The keys derived from one generation's `W`.
pub struct GenerationKeys {
    wrap: LessSafeKey,
    mac: hmac::Key,
    generation: u64,
}

struct Len32;

impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

fn derive(w: &Key32, info: &[u8]) -> Zeroizing<Key32> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, HKDF_SALT).extract(w);
    let info = [info];
    let okm = prk.expand(&info, Len32).expect("32-byte HKDF output");
    let mut out = Zeroizing::new([0u8; 32]);
    okm.fill(&mut out[..]).expect("32-byte HKDF output");
    out
}

impl GenerationKeys {
    pub fn new(w: &Key32, generation: u64) -> Self {
        let mut mac_info = b"db mac ".to_vec();
        mac_info.extend_from_slice(&generation.to_le_bytes());
        let wrap = derive(w, b"key wrap");
        let mac = derive(w, &mac_info);
        Self {
            wrap: LessSafeKey::new(
                UnboundKey::new(&CHACHA20_POLY1305, &wrap[..]).expect("32 bytes"),
            ),
            mac: hmac::Key::new(hmac::HMAC_SHA256, &mac[..]),
            generation,
        }
    }

    fn aad(&self, dev_id: &Id16) -> [u8; 9 + 16 + 8] {
        let mut aad = [0u8; 33];
        aad[..9].copy_from_slice(b"EM v3 key");
        aad[9..25].copy_from_slice(dev_id);
        aad[25..].copy_from_slice(&self.generation.to_le_bytes());
        aad
    }

    /// Wraps `key` for `dev_id` under a fresh random nonce.
    pub fn wrap(&self, dev_id: &Id16, key: &Key32, nonce: [u8; 12]) -> [u8; WRAPPED_LEN] {
        let mut buf = key.to_vec();
        self.wrap
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(&self.aad(dev_id)),
                &mut buf,
            )
            .expect("32-byte plaintext");
        let mut out = [0u8; WRAPPED_LEN];
        out.copy_from_slice(&buf);
        zeroize::Zeroize::zeroize(&mut buf);
        out
    }

    /// Unwraps a record's key, or `None` if it wasn't wrapped under these
    /// keys for this `dev_id` and generation.
    pub fn unwrap(&self, record: &DiskRecord) -> Option<Zeroizing<Key32>> {
        let mut buf = record.wrapped_key;
        let plain = self
            .wrap
            .open_in_place(
                Nonce::assume_unique_for_key(record.wrap_nonce),
                Aad::from(&self.aad(&record.dev_id)),
                &mut buf,
            )
            .ok()?;
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(plain);
        zeroize::Zeroize::zeroize(&mut buf);
        Some(key)
    }

    fn sign(&self, body: &[u8]) -> [u8; MAC_LEN] {
        let tag = hmac::sign(&self.mac, body);
        let mut out = [0u8; MAC_LEN];
        out.copy_from_slice(tag.as_ref());
        out
    }

    /// True if `file` carries a valid MAC under these keys and claims their
    /// generation.
    pub fn verify(&self, file: &[u8]) -> bool {
        if file.len() < MAC_LEN {
            return false;
        }
        let (body, mac) = file.split_at(file.len() - MAC_LEN);
        matches!(parse_db(file), Ok(db) if db.generation == self.generation)
            && hmac::verify(&self.mac, body, mac).is_ok()
    }
}

/// Encodes a database file and signs it.
pub fn encode_db(db: &DbFile, keys: &GenerationKeys) -> Vec<u8> {
    assert_eq!(db.generation, keys.generation);
    assert!(db.records.len() <= MAX_RECORDS);
    assert!(db.tombstones.len() <= MAX_TOMBSTONE_SLOTS);
    let mut out = Vec::with_capacity(64 + db.records.len() * 200);
    out.extend_from_slice(DB_MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&db.generation.to_le_bytes());
    out.extend_from_slice(&(db.records.len() as u16).to_le_bytes());
    for r in &db.records {
        assert!(r.device_name.len() <= MAX_NAME_LEN);
        out.extend_from_slice(&r.dev_id);
        out.extend_from_slice(&r.slot_id);
        out.push(r.state as u8);
        match r.replaces {
            Some(old) => {
                out.push(1);
                out.extend_from_slice(&old);
            }
            None => out.push(0),
        }
        out.extend_from_slice(&r.created_at_s.to_le_bytes());
        out.extend_from_slice(&r.committed_timestamp_us.to_le_bytes());
        out.push(r.device_name.len() as u8);
        out.extend_from_slice(r.device_name.as_bytes());
        out.extend_from_slice(&r.wrap_nonce);
        out.extend_from_slice(&r.wrapped_key);
    }
    out.extend_from_slice(&(db.tombstones.len() as u16).to_le_bytes());
    for (slot, ids) in &db.tombstones {
        assert!(ids.len() <= MAX_TOMBSTONES_PER_SLOT);
        out.extend_from_slice(slot);
        out.push(ids.len() as u8);
        for id in ids {
            out.extend_from_slice(id);
        }
    }
    let mac = keys.sign(&out);
    out.extend_from_slice(&mac);
    out
}

/// Parses a database file without authenticating it. Use
/// [`GenerationKeys::verify`] before trusting anything but the `dev_id`s,
/// slots and tombstones.
pub fn parse_db(file: &[u8]) -> Result<DbFile, FormatError> {
    if file.len() < MAC_LEN {
        return Err(FormatError::Truncated);
    }
    let mut r = Reader::new(&file[..file.len() - MAC_LEN]);
    if r.take(8)? != DB_MAGIC {
        return Err(FormatError::BadMagic);
    }
    let version = r.u8()?;
    if version != VERSION {
        return Err(FormatError::UnsupportedVersion(version));
    }
    let generation = r.u64()?;
    if generation == 0 || generation > MAX_GENERATION {
        return Err(FormatError::InvalidField("generation"));
    }
    let count = usize::from(r.u16()?);
    if count > MAX_RECORDS {
        return Err(FormatError::InvalidField("record_count"));
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let dev_id = r.id()?;
        let slot_id = r.id()?;
        let state = match r.u8()? {
            0 => RecordState::Pending,
            1 => RecordState::Active,
            _ => return Err(FormatError::InvalidField("state")),
        };
        let replaces = match r.u8()? {
            0 => None,
            1 => Some(r.id()?),
            _ => return Err(FormatError::InvalidField("flags")),
        };
        let created_at_s = r.u64()?;
        let committed_timestamp_us = r.u64()?;
        let name_len = usize::from(r.u8()?);
        if name_len > MAX_NAME_LEN {
            return Err(FormatError::InvalidField("name_len"));
        }
        let device_name = std::str::from_utf8(r.take(name_len)?)
            .map_err(|_| FormatError::InvalidField("name_utf8"))?
            .to_string();
        let wrap_nonce = r.take(12)?.try_into().unwrap();
        let wrapped_key = r.take(WRAPPED_LEN)?.try_into().unwrap();
        records.push(DiskRecord {
            dev_id,
            slot_id,
            state,
            replaces,
            created_at_s,
            committed_timestamp_us,
            device_name,
            wrap_nonce,
            wrapped_key,
        });
    }
    let slots = usize::from(r.u16()?);
    if slots > MAX_TOMBSTONE_SLOTS {
        return Err(FormatError::InvalidField("tombstone_slots"));
    }
    let mut tombstones = Vec::with_capacity(slots);
    for _ in 0..slots {
        let slot = r.id()?;
        let n = usize::from(r.u8()?);
        if n > MAX_TOMBSTONES_PER_SLOT {
            return Err(FormatError::InvalidField("tombstone_count"));
        }
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            ids.push(r.id()?);
        }
        tombstones.push((slot, ids));
    }
    if r.remaining() != 0 {
        return Err(FormatError::TrailingBytes);
    }
    let db = DbFile {
        generation,
        records,
        tombstones,
    };
    check_db(&db)?;
    Ok(db)
}

/// The invariants every database file holds: unique `dev_id`s and slots, at
/// most one active record per slot, no record tombstoned, and a pending
/// record replaces only a record in its own slot.
fn check_db(db: &DbFile) -> Result<(), FormatError> {
    let mut ids: Vec<&Id16> = db.records.iter().map(|r| &r.dev_id).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|w| w[0] == w[1]) {
        return Err(FormatError::InvalidField("duplicate_dev_id"));
    }
    let mut slots: Vec<&Id16> = db.tombstones.iter().map(|(s, _)| s).collect();
    slots.sort_unstable();
    if slots.windows(2).any(|w| w[0] == w[1]) {
        return Err(FormatError::InvalidField("duplicate_slot"));
    }
    for r in &db.records {
        let active_in_slot = db
            .records
            .iter()
            .filter(|o| o.slot_id == r.slot_id && o.state == RecordState::Active)
            .count();
        if active_in_slot > 1 {
            return Err(FormatError::InvalidField("two_active"));
        }
        if db.tombstones.iter().any(|(_, t)| t.contains(&r.dev_id)) {
            return Err(FormatError::InvalidField("tombstoned_record"));
        }
        if r.state == RecordState::Active && r.replaces.is_some() {
            return Err(FormatError::InvalidField("active_replaces"));
        }
    }
    Ok(())
}

pub fn encode_keyring(state: &KeyringState) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(128));
    out.extend_from_slice(KEYRING_MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&state.committed.0.to_le_bytes());
    out.extend_from_slice(&state.committed.1[..]);
    match &state.staged {
        Some((g, w, intent)) => {
            assert_eq!(*g, state.committed.0 + 1);
            assert!(intent.invalidate.len() <= MAX_RECORDS && intent.promote.len() <= MAX_RECORDS);
            out.push(1);
            out.extend_from_slice(&g.to_le_bytes());
            out.extend_from_slice(&w[..]);
            out.push(1);
            out.extend_from_slice(&intent.op_id);
            out.push(intent.invalidate.len() as u8);
            for id in &intent.invalidate {
                out.extend_from_slice(id);
            }
            out.push(intent.promote.len() as u8);
            for id in &intent.promote {
                out.extend_from_slice(id);
            }
        }
        None => {
            out.push(0);
            out.push(0);
        }
    }
    out
}

pub fn parse_keyring(blob: &[u8]) -> Result<KeyringState, FormatError> {
    let mut r = Reader::new(blob);
    if r.take(8)? != KEYRING_MAGIC {
        return Err(FormatError::BadMagic);
    }
    let version = r.u8()?;
    if version != VERSION {
        return Err(FormatError::UnsupportedVersion(version));
    }
    let g = r.u64()?;
    if g == 0 || g >= MAX_GENERATION {
        return Err(FormatError::InvalidField("generation"));
    }
    let w = r.key()?;
    let staged = match (r.u8()?, ()) {
        (0, ()) => {
            if r.u8()? != 0 {
                return Err(FormatError::InvalidField("intent_without_staged"));
            }
            None
        }
        (1, ()) => {
            let staged_g = r.u64()?;
            if Some(staged_g) != g.checked_add(1) {
                return Err(FormatError::InvalidField("staged_generation"));
            }
            let staged_w = r.key()?;
            if r.u8()? != 1 {
                return Err(FormatError::InvalidField("staged_without_intent"));
            }
            let op_id = r.id()?;
            let invalidate = r.ids()?;
            let promote = r.ids()?;
            if promote.iter().any(|p| invalidate.contains(p)) {
                return Err(FormatError::InvalidField("promote_and_invalidate"));
            }
            Some((
                staged_g,
                staged_w,
                Intent {
                    op_id,
                    invalidate,
                    promote,
                },
            ))
        }
        _ => return Err(FormatError::InvalidField("staged_flag")),
    };
    if r.remaining() != 0 {
        return Err(FormatError::TrailingBytes);
    }
    Ok(KeyringState {
        committed: (g, w),
        staged,
    })
}

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

    fn take(&mut self, len: usize) -> Result<&'a [u8], FormatError> {
        let end = self.at.checked_add(len).ok_or(FormatError::Truncated)?;
        let slice = self.data.get(self.at..end).ok_or(FormatError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn id(&mut self) -> Result<Id16, FormatError> {
        Ok(self.take(16)?.try_into().unwrap())
    }

    fn key(&mut self) -> Result<Zeroizing<Key32>, FormatError> {
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(self.take(32)?);
        Ok(key)
    }

    fn ids(&mut self) -> Result<Vec<Id16>, FormatError> {
        let n = usize::from(self.u8()?);
        if n > MAX_RECORDS {
            return Err(FormatError::InvalidField("id_count"));
        }
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            let id = self.id()?;
            if ids.contains(&id) {
                return Err(FormatError::InvalidField("duplicate_id"));
            }
            ids.push(id);
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(dev: u8, slot: u8, state: RecordState, keys: &GenerationKeys) -> DiskRecord {
        let dev_id = [dev; 16];
        DiskRecord {
            dev_id,
            slot_id: [slot; 16],
            state,
            replaces: None,
            created_at_s: 1_000,
            committed_timestamp_us: 7,
            device_name: "iPad".into(),
            wrap_nonce: [dev; 12],
            wrapped_key: keys.wrap(&dev_id, &[dev ^ 0x55; 32], [dev; 12]),
        }
    }

    fn sample(keys: &GenerationKeys) -> DbFile {
        DbFile {
            generation: 3,
            records: vec![
                record(1, 9, RecordState::Active, keys),
                record(2, 9, RecordState::Pending, keys),
            ],
            tombstones: vec![([9; 16], vec![[5; 16], [6; 16]])],
        }
    }

    #[test]
    fn db_round_trip_and_authentication() {
        let keys = GenerationKeys::new(&[0x11; 32], 3);
        let db = sample(&keys);
        let bytes = encode_db(&db, &keys);
        assert_eq!(parse_db(&bytes).unwrap(), db);
        assert!(keys.verify(&bytes));
        assert_eq!(*keys.unwrap(&db.records[0]).unwrap(), [1 ^ 0x55; 32]);
        // Another W, another generation, or any flipped byte: no.
        assert!(!GenerationKeys::new(&[0x12; 32], 3).verify(&bytes));
        assert!(!GenerationKeys::new(&[0x11; 32], 4).verify(&bytes));
        for at in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[at] ^= 1;
            assert!(!keys.verify(&bad), "byte {at}");
        }
    }

    #[test]
    fn a_wrapped_key_is_bound_to_its_dev_id_and_generation() {
        let keys = GenerationKeys::new(&[0x11; 32], 3);
        let mut r = record(1, 9, RecordState::Active, &keys);
        r.dev_id = [2; 16];
        assert!(keys.unwrap(&r).is_none());
        let r = record(1, 9, RecordState::Active, &keys);
        assert!(GenerationKeys::new(&[0x11; 32], 4).unwrap(&r).is_none());
    }

    #[test]
    fn db_parser_rejects_broken_invariants() {
        let keys = GenerationKeys::new(&[0x11; 32], 3);
        let mut two_active = sample(&keys);
        two_active.records[1].state = RecordState::Active;
        assert_eq!(
            parse_db(&encode_db(&two_active, &keys)),
            Err(FormatError::InvalidField("two_active"))
        );
        let mut dup = sample(&keys);
        dup.records[1].dev_id = dup.records[0].dev_id;
        assert!(parse_db(&encode_db(&dup, &keys)).is_err());
        let mut tombstoned = sample(&keys);
        tombstoned.tombstones[0].1.push([1; 16]);
        assert!(parse_db(&encode_db(&tombstoned, &keys)).is_err());
        let bytes = encode_db(&sample(&keys), &keys);
        assert_eq!(
            parse_db(&bytes[..bytes.len() - 1]),
            Err(FormatError::Truncated)
        );
        let mut longer = bytes.clone();
        longer.insert(bytes.len() - MAC_LEN, 0);
        assert_eq!(parse_db(&longer), Err(FormatError::TrailingBytes));
    }

    #[test]
    fn keyring_round_trip_and_rules() {
        let plain = KeyringState {
            committed: (4, Zeroizing::new([1; 32])),
            staged: None,
        };
        assert_eq!(parse_keyring(&encode_keyring(&plain)).unwrap(), plain);
        let rotating = KeyringState {
            committed: (4, Zeroizing::new([1; 32])),
            staged: Some((
                5,
                Zeroizing::new([2; 32]),
                Intent {
                    op_id: [3; 16],
                    invalidate: vec![[4; 16]],
                    promote: vec![[5; 16]],
                },
            )),
        };
        let bytes = encode_keyring(&rotating);
        assert_eq!(parse_keyring(&bytes).unwrap(), rotating);
        let mut wrong_gen = bytes.to_vec();
        wrong_gen[50] = 9; // staged generation
        assert!(parse_keyring(&wrong_gen).is_err());
        assert!(parse_keyring(&bytes[..bytes.len() - 1]).is_err());
    }
}
