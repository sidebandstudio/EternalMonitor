//! The host's credential store: pending and active records per slot,
//! tombstones, the caps, and the three operations that change records
//! (Finalize, Remove, Expire), with crash recovery.
//!
//! # Durability
//!
//! - **Additions** (a new pending record, a Finalize that only promotes, a
//!   committed timestamp) are one plain write of the database under the
//!   committed generation: temporary file, sync, rename, sync the directory.
//! - **Every invalidation** goes through one procedure, which rotates the
//!   wrapping key:
//!   1. record the intent: store the keyring with the next generation's key
//!      staged and the intent; from here the records it invalidates are
//!      invalid in memory;
//!   2. rewrite the database at the next generation, without them, every
//!      survivor wrapped under the new key, their `dev_id`s tombstoned;
//!   3. commit the keyring: the new generation alone.
//!
//! [`CredentialStore::open`] finishes or discards whatever a crash left, by
//! the design's startup recovery table, and runs a startup discard (through
//! the same procedure) for any database it can't authenticate.
//!
//! # Reads
//!
//! The message-1 path never waits for the writer: [`CredentialStore::snapshot`]
//! returns an immutable in-memory view, refreshed after each operation, that
//! [`StoreCredentials`] turns into [`HostCredentials`]. Keys are unwrapped
//! once, on load, and are held only as [`Zeroizing`] values.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;

use eternal_wire::v3::host::HostCredentials;
use eternal_wire::v3::wire::{Accept, RecordState, SlotStatus};
use eternal_wire::v3::{Id16, Key32, Random, Zeroizing};

use crate::disk::Disk;
use crate::format::{
    encode_db, encode_keyring, parse_db, parse_keyring, DbFile, DiskRecord, FormatError,
    GenerationKeys, Intent, KeyringState, MAX_GENERATION, MAX_NAME_LEN, MAX_RECORDS,
    MAX_TOMBSTONES_PER_SLOT, MAX_TOMBSTONE_SLOTS,
};
use crate::keyring::Keyring;

/// The database file.
pub const DB_FILE: &str = "credentials.db";
/// Its temporary file during a write.
pub const DB_TMP: &str = "credentials.db.tmp";
/// Pending records a slot may hold, next to its one active record.
pub const MAX_PENDING_PER_SLOT: usize = 3;
/// A pending record older than this expires.
pub const PENDING_LIFETIME_S: u64 = 7 * 24 * 60 * 60;

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// A cap would be exceeded: 1 active and 3 pending per slot, 64 in all.
    TooMany,
    /// The `dev_id` is already held, or was retired.
    Duplicate,
    /// No record has this `dev_id`.
    Unknown,
    /// `replaces` doesn't name the slot's current active record.
    InvalidReplaces,
    /// An earlier operation failed after recording its intent. Reads still
    /// work; reopen the store to finish the operation.
    NeedsReopen,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::TooMany => write!(f, "too many records"),
            Self::Duplicate => write!(f, "dev_id already used"),
            Self::Unknown => write!(f, "unknown dev_id"),
            Self::InvalidReplaces => write!(f, "replaces isn't the slot's active record"),
            Self::NeedsReopen => write!(f, "the store must be reopened"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What pairing hands the store at step 5.
pub struct NewRecord {
    pub dev_id: Id16,
    pub slot_id: Id16,
    pub key: Zeroizing<Key32>,
    /// Set only after a valid replace proof from the slot's active key.
    pub replaces: Option<Id16>,
    pub device_name: String,
}

/// The result of [`CredentialStore::finalize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finalize {
    /// Already active (a retried commit).
    AlreadyActive,
    /// Now active; `retired` is the record it replaced, if any.
    Promoted { retired: Option<Id16> },
    /// It conflicted and is retired. Refuse the session.
    Conflict,
}

/// A record as the UI lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordInfo {
    pub dev_id: Id16,
    pub slot_id: Id16,
    pub state: RecordState,
    pub replaces: Option<Id16>,
    pub device_name: String,
    pub created_at_s: u64,
}

/// A record dropped at startup because it couldn't be opened: list it as
/// "needs pairing again".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discarded {
    pub dev_id: Id16,
    pub slot_id: Id16,
    pub device_name: String,
}

/// What [`CredentialStore::open`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// No keyring: a fresh one was made.
    FreshKeyring,
    /// A rotation was in progress and was finished.
    FinishedRotation,
    /// The database couldn't be authenticated (restored, rolled back,
    /// corrupt or from elsewhere) and was discarded.
    Discarded,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenReport {
    /// Empty when nothing needed doing.
    pub recovery: Vec<Recovery>,
    pub discarded: Vec<Discarded>,
}

struct Record {
    dev_id: Id16,
    slot_id: Id16,
    state: RecordState,
    replaces: Option<Id16>,
    created_at_s: u64,
    committed_timestamp_us: u64,
    device_name: String,
    key: Zeroizing<Key32>,
    /// The wrap under the current generation, once computed.
    wrapped: Option<([u8; 12], [u8; 48])>,
}

pub struct CredentialStore<D: Disk, K: Keyring> {
    disk: D,
    keyring: K,
    rng: Box<dyn Random + Send>,
    generation: u64,
    w: Zeroizing<Key32>,
    records: Vec<Record>,
    /// Per slot, oldest change first; each list oldest first.
    tombstones: VecDeque<(Id16, VecDeque<Id16>)>,
    needs_reopen: bool,
    snapshot: Arc<CredentialSnapshot>,
}

fn random_key(rng: &mut dyn Random) -> Zeroizing<Key32> {
    let mut key = Zeroizing::new([0u8; 32]);
    rng.fill(&mut key[..]);
    key
}

fn truncated_name(name: &str) -> String {
    let mut end = name.len().min(MAX_NAME_LEN);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

impl<D: Disk, K: Keyring> CredentialStore<D, K> {
    /// Opens the store, finishing or discarding whatever a crash, restore
    /// or rollback left.
    pub fn open(
        disk: D,
        keyring: K,
        rng: Box<dyn Random + Send>,
    ) -> Result<(Self, OpenReport), StoreError> {
        let mut store = Self {
            disk,
            keyring,
            rng,
            generation: 0,
            w: Zeroizing::new([0; 32]),
            records: Vec::new(),
            tombstones: VecDeque::new(),
            needs_reopen: false,
            snapshot: Arc::new(CredentialSnapshot::default()),
        };
        let mut report = OpenReport::default();
        store.recover(&mut report)?;
        store.refresh_snapshot();
        Ok((store, report))
    }

    fn recover(&mut self, report: &mut OpenReport) -> Result<(), StoreError> {
        // A stray temporary file is never the database.
        self.disk.remove(DB_TMP)?;
        let db_bytes = self.disk.read(DB_FILE)?;
        let parsed = db_bytes.as_deref().map(parse_db);
        let state = match self.keyring.load()? {
            Some(blob) => parse_keyring(&blob).ok(),
            None => None,
        };

        let Some(state) = state else {
            // No keyring (a new machine, or the OS store was wiped), or one
            // that can't be read: make a fresh one past any generation the
            // database claims, then discard what it can't open.
            let generation = match &parsed {
                Some(Ok(db)) if db.generation < MAX_GENERATION - 1 => db.generation + 1,
                _ => 1,
            };
            let w = random_key(self.rng.as_mut());
            self.keyring.store(&encode_keyring(&KeyringState {
                committed: (generation, w.clone()),
                staged: None,
            }))?;
            self.generation = generation;
            self.w = w;
            report.recovery.push(Recovery::FreshKeyring);
            if db_bytes.is_some() {
                self.discard(parsed, None, report)?;
            }
            return Ok(());
        };

        self.generation = state.committed.0;
        self.w = state.committed.1.clone();
        let current = GenerationKeys::new(&self.w, self.generation);

        match state.staged {
            None => match (&db_bytes, &parsed) {
                (None, _) => Ok(()),
                (Some(bytes), Some(Ok(db))) if current.verify(bytes) && self.load(db, &current) => {
                    Ok(())
                }
                // Restored from before an invalidation, rolled back, corrupt,
                // or from elsewhere: fail closed.
                _ => self.discard(parsed, None, report),
            },
            Some((next_g, next_w, intent)) => {
                let next = GenerationKeys::new(&next_w, next_g);
                match (&db_bytes, &parsed) {
                    // Crash during step 2, before the rename (from the
                    // committed generation), or after it, before step 3 (from
                    // the staged one, with the intent already applied): redo
                    // step 2 from memory, then step 3.
                    (Some(bytes), Some(Ok(db)))
                        if (current.verify(bytes) && self.load(db, &current))
                            || (next.verify(bytes) && self.load(db, &next)) =>
                    {
                        self.apply_intent(&intent);
                        self.finish_rotation(next_g, next_w, report)
                    }
                    // The rotation started before any database existed.
                    (None, _) => self.finish_rotation(next_g, next_w, report),
                    // Neither generation opens it: discard, folding the
                    // recorded intent into the same rotation.
                    _ => self.discard(parsed, Some(&intent), report),
                }
            }
        }
    }

    /// Loads an authenticated database into memory. False, with nothing
    /// loaded, if a key doesn't unwrap: the MAC held, so short of a bug
    /// this can't happen, and the file is treated as unreadable.
    fn load(&mut self, db: &DbFile, keys: &GenerationKeys) -> bool {
        let mut records = Vec::with_capacity(db.records.len());
        for r in &db.records {
            let Some(key) = keys.unwrap(r) else {
                return false;
            };
            records.push(Record {
                dev_id: r.dev_id,
                slot_id: r.slot_id,
                state: r.state,
                replaces: r.replaces,
                created_at_s: r.created_at_s,
                committed_timestamp_us: r.committed_timestamp_us,
                device_name: r.device_name.clone(),
                key,
                wrapped: Some((r.wrap_nonce, r.wrapped_key)),
            });
        }
        self.records = records;
        self.tombstones = db
            .tombstones
            .iter()
            .map(|(slot, ids)| (*slot, ids.iter().copied().collect()))
            .collect();
        true
    }

    /// The startup discard: every record in a database that can't be opened
    /// is retired through the full procedure, so its tombstones arrive with
    /// a generation change. Tombstones already in the file are kept.
    fn discard(
        &mut self,
        parsed: Option<Result<DbFile, FormatError>>,
        recorded: Option<&Intent>,
        report: &mut OpenReport,
    ) -> Result<(), StoreError> {
        self.records.clear();
        self.tombstones.clear();
        let mut invalidate: Vec<Id16> = recorded.map(|i| i.invalidate.clone()).unwrap_or_default();
        if let Some(Ok(db)) = parsed {
            self.tombstones = db
                .tombstones
                .iter()
                .map(|(slot, ids)| (*slot, ids.iter().copied().collect()))
                .collect();
            for r in &db.records {
                report.discarded.push(Discarded {
                    dev_id: r.dev_id,
                    slot_id: r.slot_id,
                    device_name: r.device_name.clone(),
                });
                self.add_tombstone(r.slot_id, r.dev_id);
                if !invalidate.contains(&r.dev_id) {
                    invalidate.push(r.dev_id);
                }
            }
        }
        invalidate.truncate(MAX_RECORDS);
        report.recovery.push(Recovery::Discarded);
        // Steps 1 to 3, with nothing left to keep.
        let next_g = self.generation + 1;
        let next_w = random_key(self.rng.as_mut());
        let mut op_id = [0u8; 16];
        self.rng.fill(&mut op_id);
        self.keyring.store(&encode_keyring(&KeyringState {
            committed: (self.generation, self.w.clone()),
            staged: Some((
                next_g,
                next_w.clone(),
                Intent {
                    op_id,
                    invalidate,
                    promote: Vec::new(),
                },
            )),
        }))?;
        self.finish_rotation(next_g, next_w, report)
    }

    /// Steps 2 and 3 of the procedure, from what's in memory.
    fn finish_rotation(
        &mut self,
        next_g: u64,
        next_w: Zeroizing<Key32>,
        report: &mut OpenReport,
    ) -> Result<(), StoreError> {
        let keys = GenerationKeys::new(&next_w, next_g);
        for r in &mut self.records {
            r.wrapped = None;
        }
        let bytes = self.encode(&keys, next_g);
        self.write_db(&bytes)?;
        self.keyring.store(&encode_keyring(&KeyringState {
            committed: (next_g, next_w.clone()),
            staged: None,
        }))?;
        self.generation = next_g;
        self.w = next_w;
        if !report.recovery.contains(&Recovery::Discarded) {
            report.recovery.push(Recovery::FinishedRotation);
        }
        Ok(())
    }

    fn write_db(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.disk.write_synced(DB_TMP, bytes)?;
        self.disk.rename(DB_TMP, DB_FILE)?;
        self.disk.sync_dir()
    }

    /// Encodes memory as a database at `generation`, wrapping any record
    /// that has no wrap for it yet.
    fn encode(&mut self, keys: &GenerationKeys, generation: u64) -> Vec<u8> {
        let mut disk_records = Vec::with_capacity(self.records.len());
        for r in &mut self.records {
            let (nonce, wrapped) = match r.wrapped {
                Some(w) => w,
                None => {
                    let mut nonce = [0u8; 12];
                    self.rng.fill(&mut nonce);
                    let w = (nonce, keys.wrap(&r.dev_id, &r.key, nonce));
                    r.wrapped = Some(w);
                    w
                }
            };
            disk_records.push(DiskRecord {
                dev_id: r.dev_id,
                slot_id: r.slot_id,
                state: r.state,
                replaces: r.replaces,
                created_at_s: r.created_at_s,
                committed_timestamp_us: r.committed_timestamp_us,
                device_name: r.device_name.clone(),
                wrap_nonce: nonce,
                wrapped_key: wrapped,
            });
        }
        let db = DbFile {
            generation,
            records: disk_records,
            tombstones: self
                .tombstones
                .iter()
                .map(|(slot, ids)| (*slot, ids.iter().copied().collect()))
                .collect(),
        };
        encode_db(&db, keys)
    }

    /// Applies an intent to memory: invalidated records go, their `dev_id`s
    /// are tombstoned in their slots, promoted records become active.
    /// Records already gone are skipped.
    fn apply_intent(&mut self, intent: &Intent) {
        for id in &intent.invalidate {
            if let Some(i) = self.records.iter().position(|r| r.dev_id == *id) {
                let r = self.records.remove(i);
                self.add_tombstone(r.slot_id, r.dev_id);
            }
        }
        for id in &intent.promote {
            if let Some(r) = self.records.iter_mut().find(|r| r.dev_id == *id) {
                r.state = RecordState::Active;
                r.replaces = None;
            }
        }
    }

    fn add_tombstone(&mut self, slot: Id16, dev_id: Id16) {
        let mut list = match self.tombstones.iter().position(|(s, _)| *s == slot) {
            Some(i) => self.tombstones.remove(i).expect("index is valid").1,
            None => VecDeque::new(),
        };
        if !list.contains(&dev_id) {
            list.push_back(dev_id);
        }
        while list.len() > MAX_TOMBSTONES_PER_SLOT {
            list.pop_front();
        }
        // Most recently changed last.
        self.tombstones.push_back((slot, list));
        while self.tombstones.len() > MAX_TOMBSTONE_SLOTS {
            // Forget the slot that changed longest ago and holds no record.
            let records = &self.records;
            match self
                .tombstones
                .iter()
                .position(|(s, _)| !records.iter().any(|r| r.slot_id == *s))
            {
                Some(i) => {
                    self.tombstones.remove(i);
                }
                None => break,
            }
        }
    }

    fn usable(&self) -> Result<(), StoreError> {
        if self.needs_reopen {
            Err(StoreError::NeedsReopen)
        } else {
            Ok(())
        }
    }

    fn is_retired(&self, dev_id: &Id16) -> bool {
        self.tombstones.iter().any(|(_, ids)| ids.contains(dev_id))
    }

    fn active_in(&self, slot: &Id16) -> Option<Id16> {
        self.records
            .iter()
            .find(|r| r.slot_id == *slot && r.state == RecordState::Active)
            .map(|r| r.dev_id)
    }

    /// A plain write of memory under the committed generation. On failure,
    /// `undo` puts memory back: the change is simply not stored.
    fn plain_write(&mut self, undo: impl FnOnce(&mut Self)) -> Result<(), StoreError> {
        let keys = GenerationKeys::new(&self.w, self.generation);
        let g = self.generation;
        let bytes = self.encode(&keys, g);
        if let Err(e) = self.write_db(&bytes) {
            undo(self);
            return Err(e.into());
        }
        self.refresh_snapshot();
        Ok(())
    }

    /// The invalidation procedure, steps 1 to 4.
    fn rotate(&mut self, invalidate: Vec<Id16>, promote: Vec<Id16>) -> Result<(), StoreError> {
        self.usable()?;
        let next_g = self.generation + 1;
        let next_w = random_key(self.rng.as_mut());
        let mut op_id = [0u8; 16];
        self.rng.fill(&mut op_id);
        let intent = Intent {
            op_id,
            invalidate,
            promote,
        };
        // Step 1. The store is atomic, but a failure may still have stored
        // it: read it back to tell.
        if let Err(e) = self.keyring.store(&encode_keyring(&KeyringState {
            committed: (self.generation, self.w.clone()),
            staged: Some((next_g, next_w.clone(), intent.clone())),
        })) {
            match self.keyring.load() {
                Ok(blob) => {
                    let recorded = blob
                        .and_then(|b| parse_keyring(&b).ok())
                        .and_then(|k| k.staged)
                        .is_some_and(|(_, _, i)| i.op_id == op_id);
                    if !recorded {
                        // Nothing happened.
                        return Err(e.into());
                    }
                }
                Err(_) => {
                    // Unknown: invalid in memory now, settled on reopen.
                    self.apply_intent(&intent);
                    self.refresh_snapshot();
                    self.needs_reopen = true;
                    return Err(e.into());
                }
            }
        }
        // The intent is recorded: memory follows it at once.
        self.apply_intent(&intent);
        self.refresh_snapshot();
        // Steps 2 and 3. A failure leaves the rotation for `open` to finish.
        if let Err(e) = self.finish_rotation(next_g, next_w, &mut OpenReport::default()) {
            self.needs_reopen = true;
            return Err(e);
        }
        Ok(())
    }

    /// Adds a pending record (pairing step 5): one plain write. Refused
    /// with [`StoreError::TooMany`] when it would exceed a cap; existing
    /// records are never changed to make room.
    pub fn add_pending(&mut self, record: NewRecord, now_s: u64) -> Result<(), StoreError> {
        self.usable()?;
        if self.records.iter().any(|r| r.dev_id == record.dev_id) || self.is_retired(&record.dev_id)
        {
            return Err(StoreError::Duplicate);
        }
        if self.records.len() >= MAX_RECORDS {
            return Err(StoreError::TooMany);
        }
        let pending_in_slot = self
            .records
            .iter()
            .filter(|r| r.slot_id == record.slot_id && r.state == RecordState::Pending)
            .count();
        if pending_in_slot >= MAX_PENDING_PER_SLOT {
            return Err(StoreError::TooMany);
        }
        if let Some(old) = record.replaces {
            if self.active_in(&record.slot_id) != Some(old) {
                return Err(StoreError::InvalidReplaces);
            }
        }
        let dev_id = record.dev_id;
        self.records.push(Record {
            dev_id,
            slot_id: record.slot_id,
            state: RecordState::Pending,
            replaces: record.replaces,
            created_at_s: now_s,
            committed_timestamp_us: 0,
            device_name: truncated_name(&record.device_name),
            key: record.key,
            wrapped: None,
        });
        self.plain_write(|s| s.records.retain(|r| r.dev_id != dev_id))
    }

    /// Whether finalizing this record would conflict, from memory alone.
    /// The session table decides commit or refuse with this, quickly.
    pub fn would_conflict(&self, dev_id: &Id16) -> bool {
        let Some(r) = self.records.iter().find(|r| r.dev_id == *dev_id) else {
            return true;
        };
        if r.state == RecordState::Active {
            return false;
        }
        let active = self.active_in(&r.slot_id);
        let allowed = match r.replaces {
            Some(old) => active == Some(old),
            None => active.is_none(),
        };
        !allowed
    }

    /// Finalize, at the commit of a session under `dev_id`.
    pub fn finalize(&mut self, dev_id: &Id16) -> Result<Finalize, StoreError> {
        self.usable()?;
        let Some(i) = self.records.iter().position(|r| r.dev_id == *dev_id) else {
            return Err(StoreError::Unknown);
        };
        if self.records[i].state == RecordState::Active {
            return Ok(Finalize::AlreadyActive);
        }
        if self.would_conflict(dev_id) {
            self.rotate(vec![*dev_id], Vec::new())?;
            return Ok(Finalize::Conflict);
        }
        match self.records[i].replaces {
            None => {
                self.records[i].state = RecordState::Active;
                let id = *dev_id;
                self.plain_write(|s| {
                    if let Some(r) = s.records.iter_mut().find(|r| r.dev_id == id) {
                        r.state = RecordState::Pending;
                    }
                })?;
                Ok(Finalize::Promoted { retired: None })
            }
            Some(old) => {
                self.rotate(vec![old], vec![*dev_id])?;
                Ok(Finalize::Promoted { retired: Some(old) })
            }
        }
    }

    /// Remove, by the host user. Records not held are skipped.
    pub fn remove(&mut self, dev_ids: &[Id16]) -> Result<(), StoreError> {
        self.usable()?;
        let mut held: Vec<Id16> = Vec::new();
        for id in dev_ids {
            if self.records.iter().any(|r| r.dev_id == *id) && !held.contains(id) {
                held.push(*id);
            }
        }
        if held.is_empty() {
            return Ok(());
        }
        self.rotate(held, Vec::new())
    }

    /// Expire: retires pending records 7 days old or older. Returns them.
    pub fn expire(&mut self, now_s: u64) -> Result<Vec<Id16>, StoreError> {
        self.usable()?;
        let old: Vec<Id16> = self
            .records
            .iter()
            .filter(|r| {
                r.state == RecordState::Pending
                    && now_s.saturating_sub(r.created_at_s) >= PENDING_LIFETIME_S
            })
            .map(|r| r.dev_id)
            .collect();
        if !old.is_empty() {
            self.rotate(old.clone(), Vec::new())?;
        }
        Ok(old)
    }

    /// Persists the committed timestamp for `dev_id` (one plain write). It
    /// only ever grows.
    pub fn persist_timestamp(
        &mut self,
        dev_id: &Id16,
        timestamp_us: u64,
    ) -> Result<(), StoreError> {
        self.usable()?;
        let Some(r) = self.records.iter_mut().find(|r| r.dev_id == *dev_id) else {
            return Err(StoreError::Unknown);
        };
        let before = r.committed_timestamp_us;
        if timestamp_us <= before {
            return Ok(());
        }
        r.committed_timestamp_us = timestamp_us;
        let id = *dev_id;
        self.plain_write(|s| {
            if let Some(r) = s.records.iter_mut().find(|r| r.dev_id == id) {
                r.committed_timestamp_us = before;
            }
        })
    }

    /// Every record, for the host's list.
    pub fn records(&self) -> Vec<RecordInfo> {
        self.records
            .iter()
            .map(|r| RecordInfo {
                dev_id: r.dev_id,
                slot_id: r.slot_id,
                state: r.state,
                replaces: r.replaces,
                device_name: r.device_name.clone(),
                created_at_s: r.created_at_s,
            })
            .collect()
    }

    /// Every slot's tombstones, oldest first.
    pub fn tombstones(&self) -> Vec<(Id16, Vec<Id16>)> {
        self.tombstones
            .iter()
            .map(|(slot, ids)| (*slot, ids.iter().copied().collect()))
            .collect()
    }

    /// The committed generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The current in-memory view, for the session table.
    pub fn snapshot(&self) -> Arc<CredentialSnapshot> {
        self.snapshot.clone()
    }

    fn refresh_snapshot(&mut self) {
        let mut snapshot = CredentialSnapshot::default();
        for r in &self.records {
            snapshot.keys.insert(r.dev_id, r.key.clone());
            snapshot
                .timestamps
                .insert(r.dev_id, r.committed_timestamp_us);
            snapshot.slot_of.insert(r.dev_id, r.slot_id);
        }
        for (slot, ids) in &self.tombstones {
            for id in ids {
                snapshot.slot_of.insert(*id, *slot);
            }
        }
        let mut slots: Vec<Id16> = snapshot.slot_of.values().copied().collect();
        slots.sort_unstable();
        slots.dedup();
        for slot in slots {
            let held = self
                .records
                .iter()
                .filter(|r| r.slot_id == slot)
                .map(|r| (r.dev_id, r.state))
                .collect();
            let tombstones = self
                .tombstones
                .iter()
                .find(|(s, _)| *s == slot)
                .map(|(_, ids)| ids.iter().copied().collect())
                .unwrap_or_default();
            snapshot
                .statuses
                .insert(slot, SlotStatus { held, tombstones });
        }
        self.snapshot = Arc::new(snapshot);
    }

    /// The disk and keyring back, for tests.
    pub fn into_parts(self) -> (D, K) {
        (self.disk, self.keyring)
    }
}

/// An immutable view of the store: what the message-1 path reads. Building
/// one does no I/O; reading one never waits for the writer.
#[derive(Default)]
pub struct CredentialSnapshot {
    keys: HashMap<Id16, Zeroizing<Key32>>,
    timestamps: HashMap<Id16, u64>,
    slot_of: HashMap<Id16, Id16>,
    statuses: HashMap<Id16, SlotStatus>,
}

impl CredentialSnapshot {
    pub fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        self.keys.get(dev_id).cloned()
    }

    pub fn last_committed_timestamp(&self, dev_id: &Id16) -> u64 {
        self.timestamps.get(dev_id).copied().unwrap_or(0)
    }

    /// The status of the slot `dev_id` belongs or belonged to; empty for a
    /// `dev_id` the store never held.
    pub fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
        self.slot_of
            .get(dev_id)
            .and_then(|slot| self.statuses.get(slot))
            .cloned()
            .unwrap_or_default()
    }

    /// Records held.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// A snapshot plus the host's HelloAck fields: the credentials
/// [`HostSessions`] takes.
///
/// [`HostSessions`]: eternal_wire::v3::host::HostSessions
pub struct StoreCredentials {
    pub snapshot: Arc<CredentialSnapshot>,
    pub accept: Accept,
}

impl HostCredentials for StoreCredentials {
    fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        self.snapshot.key(dev_id)
    }

    fn last_committed_timestamp(&self, dev_id: &Id16) -> u64 {
        self.snapshot.last_committed_timestamp(dev_id)
    }

    fn accept_template(&self, dev_id: &Id16) -> Accept {
        let mut accept = self.accept.clone();
        accept.slot_status = self.snapshot.slot_status(dev_id);
        accept
    }

    fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
        self.snapshot.slot_status(dev_id)
    }
}
