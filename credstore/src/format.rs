//! The two byte formats the store keeps: the credential database file and
//! the keyring blob.
//!
//! Both parsers are total: they return a value or a [`FormatError`], never
//! panic, and never read past their input. Every count is bounded before
//! anything is allocated for it.
//!
//! # Database file (`credentials-<generation>.db`)
//!
//! ```text
//! magic "EMV3CRED", version u8 = 1, generation u64, head u64 (≥ generation),
//!   head fingerprint 16
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
//! Integers are little-endian. `generation` is the generation whose key `W`
//! the file is under; `head` is the newest committed generation its contents
//! need (above `generation` while a rotation isn't confirmed durable yet),
//! and the head fingerprint is [`key_fingerprint`] of that generation's key:
//! when the head is the keyring's committed or staged generation, a file
//! written under another key for that generation (another history) is told
//! apart.
//! The wrap key and the MAC key are derived from `W` with HKDF-SHA-256; each
//! wrap binds `(dev_id, generation)` as associated data. `dev_id`s, slots and tombstones
//! are readable without `W`, so records that can't be opened can still be
//! listed and tombstoned.
//!
//! # Keyring blob
//!
//! ```text
//! magic "EMV3KRNG", version u8 = 1
//! committed generation u64, committed W 32
//! base generation u64 (≤ committed), base W 32 (equal to committed W when
//!   the generations are equal)
//! boot length u8 (≤ MAX_BOOT_ID_LEN) + boot id (the boot that stored the
//!   blob; empty where a store is durable at once, or the id is unknown)
//! staged flag u8 (0 or 1), [staged generation u64, staged W 32]
//! intent flag u8 (0 or 1), [op_id 16,
//!   invalidate count u8 (≤ MAX_RECORDS) + dev_id 16 × count,
//!   promote count u8 (≤ MAX_RECORDS) + dev_id 16 × count]
//! ```
//!
//! The base is the generation the database file is under: the newest one
//! known to be durable. A staged key and an intent are present together or
//! not at all, and the staged generation is above the committed one. The blob lives only in
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
/// Pending records a slot may hold, next to its one active record.
pub const MAX_PENDING_PER_SLOT: usize = 3;
/// Longest boot id a keyring blob records.
pub const MAX_BOOT_ID_LEN: usize = 64;
/// Every generation, in either format, is at least 1 and below this. One
/// rotation per user action never gets near it.
pub const MAX_GENERATION: u64 = 1 << 62;

/// True for a generation either format accepts.
pub fn valid_generation(g: u64) -> bool {
    (1..MAX_GENERATION).contains(&g)
}

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
    /// The newest committed generation the contents need.
    pub head: u64,
    /// [`key_fingerprint`] of the head generation's committed key.
    pub head_fp: [u8; 16],
    pub records: Vec<DiskRecord>,
    /// Per slot, oldest first.
    pub tombstones: Vec<(Id16, Vec<Id16>)>,
}

/// The keyring's contents.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyringState {
    pub committed: (u64, Zeroizing<Key32>),
    /// The generation and key the database file is under.
    pub base: (u64, Zeroizing<Key32>),
    /// The id of the boot that stored this blob (empty if unknown, or not
    /// needed).
    pub boot: Vec<u8>,
    /// A rotation in progress: the next generation's key and what it does.
    pub staged: Option<(u64, Zeroizing<Key32>, Intent)>,
}

impl std::fmt::Debug for KeyringState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringState")
            .field("committed_generation", &self.committed.0)
            .field("base_generation", &self.base.0)
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

/// A short public label of a generation's key, recorded in the database
/// header: HKDF-SHA-256 of `W` with its own label and the generation, never
/// the key itself.
pub fn key_fingerprint(w: &Key32, generation: u64) -> [u8; 16] {
    let mut info = b"key fingerprint ".to_vec();
    info.extend_from_slice(&generation.to_le_bytes());
    let full = derive(w, &info);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
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
        // Room for the tag up front: the key is never copied by a regrowth.
        let mut buf = Zeroizing::new(Vec::with_capacity(WRAPPED_LEN));
        let at = buf.as_ptr();
        buf.extend_from_slice(key);
        self.wrap
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(&self.aad(dev_id)),
                &mut *buf,
            )
            .expect("32-byte plaintext");
        debug_assert_eq!(buf.as_ptr(), at, "never reallocated");
        let mut out = [0u8; WRAPPED_LEN];
        out.copy_from_slice(&buf);
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
    assert!(db.head >= db.generation && valid_generation(db.head));
    out.extend_from_slice(&db.head.to_le_bytes());
    out.extend_from_slice(&db.head_fp);
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
    if !valid_generation(generation) {
        return Err(FormatError::InvalidField("generation"));
    }
    let head = r.u64()?;
    if !valid_generation(head) || head < generation {
        return Err(FormatError::InvalidField("head"));
    }
    let head_fp: [u8; 16] = r.take(16)?.try_into().unwrap();
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
        head,
        head_fp,
        records,
        tombstones,
    };
    check_db(&db)?;
    Ok(db)
}

/// Every invariant the store keeps, so a file that breaks one (even with a
/// valid MAC) is never loaded:
/// - unique `dev_id`s across records and tombstones, and unique slots in the
///   tombstone list, each with 1 to 8 entries;
/// - per slot, at most one active and three pending records;
/// - an active record replaces nothing; a pending record never replaces
///   itself, and if the record it replaces is still held, that's its slot's
///   active record;
/// - no held record is tombstoned.
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
    let mut retired: Vec<&Id16> = db.tombstones.iter().flat_map(|(_, t)| t).collect();
    retired.sort_unstable();
    if retired.windows(2).any(|w| w[0] == w[1]) {
        return Err(FormatError::InvalidField("duplicate_tombstone"));
    }
    if db.tombstones.iter().any(|(_, t)| t.is_empty()) {
        return Err(FormatError::InvalidField("empty_tombstones"));
    }
    for r in &db.records {
        let in_slot = |state| {
            db.records
                .iter()
                .filter(|o| o.slot_id == r.slot_id && o.state == state)
                .count()
        };
        if in_slot(RecordState::Active) > 1 {
            return Err(FormatError::InvalidField("two_active"));
        }
        if in_slot(RecordState::Pending) > MAX_PENDING_PER_SLOT {
            return Err(FormatError::InvalidField("too_many_pending"));
        }
        if retired.binary_search(&&r.dev_id).is_ok() {
            return Err(FormatError::InvalidField("tombstoned_record"));
        }
        match (r.state, r.replaces) {
            (RecordState::Active, Some(_)) => {
                return Err(FormatError::InvalidField("active_replaces"));
            }
            (RecordState::Pending, Some(old)) => {
                if old == r.dev_id {
                    return Err(FormatError::InvalidField("replaces_itself"));
                }
                if let Some(o) = db.records.iter().find(|o| o.dev_id == old) {
                    if o.slot_id != r.slot_id || o.state != RecordState::Active {
                        return Err(FormatError::InvalidField("replaces_not_active"));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The encoded size of `state`.
fn keyring_len(state: &KeyringState) -> usize {
    8 + 1
        + (8 + 32) * 2
        + 1
        + state.boot.len()
        + match &state.staged {
            Some((_, _, i)) => {
                1 + 8 + 32 + 1 + 16 + 1 + 16 * i.invalidate.len() + 1 + 16 * i.promote.len()
            }
            None => 2,
        }
}

/// Encodes the keyring blob into one buffer of exactly its size, so no key
/// is ever left behind by a regrowth.
pub fn encode_keyring(state: &KeyringState) -> Zeroizing<Vec<u8>> {
    let len = keyring_len(state);
    let mut out = Zeroizing::new(Vec::with_capacity(len));
    let at = out.as_ptr();
    assert!(valid_generation(state.committed.0) && valid_generation(state.base.0));
    assert!(state.base.0 <= state.committed.0);
    assert!(state.base.0 < state.committed.0 || state.base.1 == state.committed.1);
    out.extend_from_slice(KEYRING_MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&state.committed.0.to_le_bytes());
    out.extend_from_slice(&state.committed.1[..]);
    out.extend_from_slice(&state.base.0.to_le_bytes());
    out.extend_from_slice(&state.base.1[..]);
    assert!(state.boot.len() <= MAX_BOOT_ID_LEN);
    out.push(state.boot.len() as u8);
    out.extend_from_slice(&state.boot);
    match &state.staged {
        Some((g, w, intent)) => {
            assert!(*g > state.committed.0);
            assert!(valid_generation(*g));
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
    assert_eq!(out.len(), len);
    debug_assert_eq!(out.as_ptr(), at, "never reallocated");
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
    if !valid_generation(g) {
        return Err(FormatError::InvalidField("generation"));
    }
    let w = r.key()?;
    let base_g = r.u64()?;
    let base_w = r.key()?;
    if !valid_generation(base_g) || base_g > g || (base_g == g && base_w != w) {
        return Err(FormatError::InvalidField("base"));
    }
    let boot_len = usize::from(r.u8()?);
    if boot_len > MAX_BOOT_ID_LEN {
        return Err(FormatError::InvalidField("boot_len"));
    }
    let boot = r.take(boot_len)?.to_vec();
    let staged = match (r.u8()?, ()) {
        (0, ()) => {
            if r.u8()? != 0 {
                return Err(FormatError::InvalidField("intent_without_staged"));
            }
            None
        }
        (1, ()) => {
            let staged_g = r.u64()?;
            if staged_g <= g || !valid_generation(staged_g) {
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
        base: (base_g, base_w),
        boot,
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
            head: 3,
            head_fp: [0; 16],
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
    fn db_parser_enforces_every_store_invariant() {
        let keys = GenerationKeys::new(&[0x11; 32], 3);
        let check = |db: &DbFile, field: &'static str| {
            assert_eq!(
                parse_db(&encode_db(db, &keys)),
                Err(FormatError::InvalidField(field)),
                "{field}"
            );
        };
        // Four pending records in one slot.
        let mut pending = sample(&keys);
        for dev in 3..=5 {
            pending
                .records
                .push(record(dev, 9, RecordState::Pending, &keys));
        }
        check(&pending, "too_many_pending");
        pending.records.pop();
        assert!(
            parse_db(&encode_db(&pending, &keys)).is_ok(),
            "three is fine"
        );
        // Replacing itself, or a held record that isn't the slot's active.
        let mut itself = sample(&keys);
        itself.records[1].replaces = Some(itself.records[1].dev_id);
        check(&itself, "replaces_itself");
        let mut not_active = sample(&keys);
        not_active
            .records
            .push(record(3, 9, RecordState::Pending, &keys));
        not_active.records[2].replaces = Some([2; 16]);
        check(&not_active, "replaces_not_active");
        let mut other_slot = sample(&keys);
        other_slot
            .records
            .push(record(3, 8, RecordState::Pending, &keys));
        other_slot.records[2].replaces = Some([1; 16]);
        check(&other_slot, "replaces_not_active");
        // A replaced record that's gone is fine (it was retired).
        let mut gone = sample(&keys);
        gone.records[1].replaces = Some([0x77; 16]);
        assert!(parse_db(&encode_db(&gone, &keys)).is_ok());
        // An active record replacing anything.
        let mut active = sample(&keys);
        active.records[0].replaces = Some([0x77; 16]);
        check(&active, "active_replaces");
        // Tombstones: duplicate ids, empty lists.
        let mut dup = sample(&keys);
        dup.tombstones.push(([8; 16], vec![[5; 16]]));
        check(&dup, "duplicate_tombstone");
        let mut empty = sample(&keys);
        empty.tombstones.push(([8; 16], Vec::new()));
        check(&empty, "empty_tombstones");
    }

    #[test]
    fn both_formats_share_one_generation_bound() {
        let ok = |g| {
            let keys = GenerationKeys::new(&[0x11; 32], g);
            let mut db = sample(&keys);
            db.generation = g;
            db.head = g;
            db.records.clear();
            parse_db(&encode_db(&db, &keys)).is_ok()
        };
        assert!(ok(1) && ok(MAX_GENERATION - 1));
        // Encoded by hand: the encoders refuse these.
        let mut db = encode_db(
            &DbFile {
                generation: 5,
                head: 5,
                head_fp: [0; 16],
                records: Vec::new(),
                tombstones: Vec::new(),
            },
            &GenerationKeys::new(&[0x11; 32], 5),
        );
        for bad in [0, MAX_GENERATION, u64::MAX] {
            db[9..17].copy_from_slice(&bad.to_le_bytes());
            assert_eq!(parse_db(&db), Err(FormatError::InvalidField("generation")));
        }
        let at = |committed: u64, staged: bool| {
            let mut blob = encode_keyring(&KeyringState {
                committed: (5, Zeroizing::new([1; 32])),
                base: (5, Zeroizing::new([1; 32])),
                boot: Vec::new(),
                staged: staged.then(|| (6, Zeroizing::new([2; 32]), Intent::default())),
            })
            .to_vec();
            blob[9..17].copy_from_slice(&committed.to_le_bytes());
            // The base stays at 5 with its own key: below the committed one.
            blob[57..89].copy_from_slice(&[7; 32]);
            if staged {
                blob[91..99].copy_from_slice(&(committed.wrapping_add(1)).to_le_bytes());
            }
            parse_keyring(&blob).is_ok()
        };
        assert!(at(MAX_GENERATION - 1, false));
        assert!(at(MAX_GENERATION - 2, true));
        assert!(
            !at(MAX_GENERATION - 1, true),
            "staged would reach the bound"
        );
        assert!(!at(MAX_GENERATION, false));
        assert!(!at(0, false));
        assert!(valid_generation(MAX_GENERATION - 1) && !valid_generation(MAX_GENERATION));
    }

    #[test]
    fn the_largest_keyring_blob_is_encoded_without_reallocating() {
        // The debug assertion in encode_keyring checks the buffer never
        // moved; this makes it run at the largest size.
        let ids: Vec<Id16> = (0..MAX_RECORDS as u8).map(|i| [i; 16]).collect();
        let state = KeyringState {
            committed: (5, Zeroizing::new([1; 32])),
            base: (5, Zeroizing::new([1; 32])),
            boot: Vec::new(),
            staged: Some((
                6,
                Zeroizing::new([2; 32]),
                Intent {
                    op_id: [3; 16],
                    invalidate: ids[..32].to_vec(),
                    promote: ids[32..].to_vec(),
                },
            )),
        };
        let blob = encode_keyring(&state);
        assert_eq!(blob.len(), blob.capacity());
        assert_eq!(parse_keyring(&blob).unwrap(), state);
        let plain = encode_keyring(&KeyringState {
            committed: (5, Zeroizing::new([1; 32])),
            base: (5, Zeroizing::new([1; 32])),
            boot: Vec::new(),
            staged: None,
        });
        assert_eq!(plain.len(), plain.capacity());
    }

    #[test]
    fn the_base_and_head_follow_their_rules() {
        // A base below the committed generation, with its own key.
        let behind = KeyringState {
            committed: (7, Zeroizing::new([1; 32])),
            base: (5, Zeroizing::new([2; 32])),
            boot: Vec::new(),
            staged: None,
        };
        let blob = encode_keyring(&behind);
        assert_eq!(parse_keyring(&blob).unwrap(), behind);
        // The base above the committed generation, or level with it under
        // another key: rejected.
        let mut above = blob.to_vec();
        above[49..57].copy_from_slice(&8u64.to_le_bytes());
        assert_eq!(
            parse_keyring(&above),
            Err(FormatError::InvalidField("base"))
        );
        let mut level = blob.to_vec();
        level[49..57].copy_from_slice(&7u64.to_le_bytes());
        assert_eq!(
            parse_keyring(&level),
            Err(FormatError::InvalidField("base"))
        );
        // A head below the file's generation: rejected.
        let keys = GenerationKeys::new(&[0x11; 32], 3);
        let mut db = encode_db(&sample(&keys), &keys);
        assert_eq!(parse_db(&db).unwrap().head, 3);
        db[17..25].copy_from_slice(&2u64.to_le_bytes());
        assert_eq!(parse_db(&db), Err(FormatError::InvalidField("head")));
        db[17..25].copy_from_slice(&9u64.to_le_bytes());
        assert_eq!(parse_db(&db).unwrap().head, 9);
    }

    #[test]
    fn the_head_fingerprint_names_one_key_and_generation() {
        let a = key_fingerprint(&[1; 32], 3);
        assert_eq!(a, key_fingerprint(&[1; 32], 3));
        assert_ne!(a, key_fingerprint(&[2; 32], 3), "another key");
        assert_ne!(a, key_fingerprint(&[1; 32], 4), "another generation");
        assert!(
            !a.windows(16).any(|w| w == &[1u8; 16][..]),
            "not the key itself"
        );
        let keys = GenerationKeys::new(&[0x11; 32], 3);
        let mut db = sample(&keys);
        db.head_fp = a;
        assert_eq!(parse_db(&encode_db(&db, &keys)).unwrap().head_fp, a);
    }

    #[test]
    fn the_boot_id_round_trips_and_is_bounded() {
        let state = KeyringState {
            committed: (4, Zeroizing::new([1; 32])),
            base: (4, Zeroizing::new([1; 32])),
            boot: b"1D2C3B4A-0000-4000-8000-00000000000F".to_vec(),
            staged: None,
        };
        let blob = encode_keyring(&state);
        assert_eq!(blob.len(), blob.capacity());
        assert_eq!(parse_keyring(&blob).unwrap(), state);
        let mut long = blob.to_vec();
        long[89] = MAX_BOOT_ID_LEN as u8 + 1;
        assert_eq!(
            parse_keyring(&long),
            Err(FormatError::InvalidField("boot_len"))
        );
    }

    #[test]
    fn keyring_round_trip_and_rules() {
        let plain = KeyringState {
            committed: (4, Zeroizing::new([1; 32])),
            base: (4, Zeroizing::new([1; 32])),
            boot: Vec::new(),
            staged: None,
        };
        assert_eq!(parse_keyring(&encode_keyring(&plain)).unwrap(), plain);
        let rotating = KeyringState {
            committed: (4, Zeroizing::new([1; 32])),
            base: (4, Zeroizing::new([1; 32])),
            boot: Vec::new(),
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
        wrong_gen[91] = 4; // staged generation, level with the committed one
        assert!(parse_keyring(&wrong_gen).is_err());
        assert!(parse_keyring(&bytes[..bytes.len() - 1]).is_err());
    }
}
