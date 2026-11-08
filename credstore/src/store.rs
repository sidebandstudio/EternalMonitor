//! The host's credential store: pending and active records per slot,
//! tombstones, the caps, and the three operations that change records
//! (Finalize, Remove, Expire), with crash recovery.
//!
//! # Durability
//!
//! The keyring holds a committed generation and key, and a *base*: the
//! newest generation known to be durable (on Windows, its store and flush
//! succeeded; on macOS, a later boot saw it). The database is written under
//! the base's key, to `credentials-<base>.db`. Every keyring blob stored
//! since the base was confirmed carries the base key, so whichever of them
//! survives a crash, the database opens, with every write that returned.
//!
//! - **Additions** (a new pending record, a Finalize that only promotes, a
//!   committed timestamp) are one plain write of the base's file: temporary
//!   file, sync, rename, sync the directory.
//! - **Every invalidation** goes through one procedure, which rotates the
//!   committed key:
//!   1. record the intent: store the keyring with the next generation's key
//!      staged and the intent; from here the records it invalidates are
//!      invalid in memory;
//!   2. write the database without them, their `dev_id`s tombstoned;
//!   3. commit the keyring: the new generation, with the same base.
//! - **Promotion.** Once the committed generation is durable (at once on
//!   Windows; at the first open in a later boot on macOS, from a boot
//!   marker), the database moves to its key and the old base's file is
//!   removed. From then on a database restored from before the rotation no
//!   longer opens.
//!
//! The database header records the newest committed generation its contents
//! need. If the keyring that survived is older (its rotation's stores were
//! lost), the open rotates again, so the invalidation still gets a new key.
//!
//! At most two database files exist (the base's, and the committed one's
//! during a promotion), and an open reads at most two.
//!
//! [`CredentialStore::open`] finishes or discards whatever a crash left, by
//! the design's startup recovery table, and runs a startup discard (through
//! the same procedure) when no file opens under the keyring. A keyring blob
//! that exists but can't be read is never replaced.
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
pub use crate::format::MAX_PENDING_PER_SLOT;
use crate::format::{
    encode_db, encode_keyring, parse_db, parse_keyring, valid_generation, DbFile, DiskRecord,
    FormatError, GenerationKeys, Intent, KeyringState, MAX_GENERATION, MAX_NAME_LEN, MAX_RECORDS,
    MAX_TOMBSTONES_PER_SLOT, MAX_TOMBSTONE_SLOTS,
};
use crate::keyring::{Durability, Keyring};

/// The temporary file of a database write.
pub const DB_TMP: &str = "credentials.tmp";
/// The id of the boot of the last open (macOS).
pub const BOOT_MARKER: &str = "credentials.boot";
/// Its temporary file.
const BOOT_TMP: &str = "credentials.boot.tmp";

/// The database file of generation `g`.
pub fn db_file(g: u64) -> String {
    format!("credentials-{g}.db")
}

/// The generation a file name is the database of, if it is one.
fn generation_of(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("credentials-")?.strip_suffix(".db")?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|g| valid_generation(*g))
}

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
    /// An earlier operation may have recorded its intent and didn't
    /// finish. Reads still work; reopen the store to settle it.
    NeedsReopen,
    /// The keyring holds a blob this build can't read (damaged, or from a
    /// newer version). It is left untouched; the store can't open.
    KeyringUnreadable(FormatError),
    /// The generation counter is at its bound; nothing was changed.
    Exhausted,
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
            Self::KeyringUnreadable(e) => write!(f, "keyring unreadable: {e}"),
            Self::Exhausted => write!(f, "generation limit reached"),
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
    /// Boxed, so handing it over never copies the key.
    pub key: Box<Zeroizing<Key32>>,
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
    /// The keyring that survived predates a rotation the database already
    /// reflects; the open rotated again.
    RotatedAgain,
    /// The database couldn't be authenticated (restored, rolled back,
    /// corrupt or from elsewhere) and was discarded.
    Discarded,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenReport {
    /// Empty when nothing needed doing (promotion and cleanup aren't
    /// listed).
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
    /// Boxed, so moving a record never leaves a copy of the key behind.
    key: Box<Zeroizing<Key32>>,
    /// The wrap under the base key, once computed.
    wrapped: Option<([u8; 12], [u8; 48])>,
}

pub struct CredentialStore<D: Disk, K: Keyring> {
    disk: D,
    keyring: K,
    rng: Box<dyn Random + Send>,
    /// The generation and key the database is written under.
    base: (u64, Zeroizing<Key32>),
    /// The newest committed generation and key.
    committed: (u64, Zeroizing<Key32>),
    /// The newest committed generation the database's contents need (its
    /// header's head).
    need: u64,
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
            base: (0, Zeroizing::new([0; 32])),
            committed: (0, Zeroizing::new([0; 32])),
            need: 0,
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

    /// Whether the keyring blob now visible is durable: flushed, or (macOS)
    /// written in an earlier boot than this one. Records this boot first, so
    /// a reopen in the same boot never counts as confirmation.
    fn confirm_at_open(&mut self) -> Result<bool, StoreError> {
        match self.keyring.flush()? {
            Durability::Durable => Ok(true),
            Durability::LaterBoot(boot) => {
                let seen = self.disk.read(BOOT_MARKER)?;
                if seen.as_deref() == Some(&boot[..]) {
                    return Ok(false);
                }
                self.disk.write_synced(BOOT_TMP, &boot)?;
                self.disk.rename(BOOT_TMP, BOOT_MARKER)?;
                self.disk.sync_dir()?;
                // No marker at all (first open, or it was lost): not proof.
                Ok(seen.is_some())
            }
        }
    }

    fn recover(&mut self, report: &mut OpenReport) -> Result<(), StoreError> {
        // A stray temporary file is never the database. What this open
        // reads it then serves, so make any rename still pending durable
        // first: a reopen in the same boot never sees what a power cut
        // could take away.
        self.disk.remove(DB_TMP)?;
        self.disk.sync_dir()?;
        let confirmed = self.confirm_at_open()?;
        let mut names: Vec<u64> = self
            .disk
            .list()?
            .iter()
            .filter_map(|n| generation_of(n))
            .collect();
        names.sort_unstable();
        // Only a missing keyring makes a fresh one; one that can't be read
        // is left as it is.
        let Some(blob) = self.keyring.load()? else {
            return self.fresh_keyring(&names, report);
        };
        let state = parse_keyring(&blob).map_err(StoreError::KeyringUnreadable)?;
        self.base = (state.base.0, state.base.1.clone());
        self.committed = (state.committed.0, state.committed.1.clone());

        // The committed generation's file (a promotion got that far) is
        // newer than the base's, if both open. Nothing else is read.
        let mut head = None;
        let mut candidates = vec![self.committed.0];
        if self.base.0 != self.committed.0 {
            candidates.push(self.base.0);
        }
        for g in candidates {
            if !names.contains(&g) {
                continue;
            }
            let w = if g == self.committed.0 {
                self.committed.1.clone()
            } else {
                self.base.1.clone()
            };
            let Some(bytes) = self.disk.read(&db_file(g))? else {
                continue;
            };
            if let Some(h) = self.load_verified(&bytes, &GenerationKeys::new(&w, g)) {
                // Every blob that may survive holds this key.
                self.base = (g, w);
                head = Some(h);
                break;
            }
        }

        if head.is_none() && !names.is_empty() {
            // No file opens under the keyring: restored from before a
            // promotion, rolled back, corrupt, or from elsewhere. Fail
            // closed, folding any recorded intent into the same rotation.
            let intent = state.staged.as_ref().map(|(_, _, i)| i.clone());
            return self.discard(&names, intent.as_ref(), report);
        }
        if let Some(h) = head {
            self.need = h;
        }
        if confirmed {
            // The visible blob is durable: so is its committed key.
            if self.base.0 < self.committed.0 {
                let target = (self.committed.0, self.committed.1.clone());
                self.promote(target, state.staged.clone())?;
            }
            // Every other file is older than a durable base, or from
            // elsewhere.
            self.remove_files_but(&names, self.base.0)?;
        }
        if let Some((next_g, next_w, intent)) = state.staged {
            // Redo step 2 from memory, then step 3. A database whose head
            // already reaches the staged generation has the intent applied
            // (and maybe later writes on top): it isn't replayed.
            if head.is_none_or(|h| h < next_g) {
                self.apply_intent(&intent);
            }
            self.finish_rotation(next_g, next_w)?;
            report.recovery.push(Recovery::FinishedRotation);
        }
        if head.is_some_and(|h| h > self.committed.0) {
            // The rotation's keyring stores were lost; its data wasn't.
            self.rotate(Vec::new(), Vec::new())?;
            report.recovery.push(Recovery::RotatedAgain);
        }
        Ok(())
    }

    /// No keyring (a new machine, or the OS store was wiped): make a fresh
    /// one past every generation on disk, then discard what's there.
    fn fresh_keyring(&mut self, names: &[u64], report: &mut OpenReport) -> Result<(), StoreError> {
        let newest = names.last().copied().unwrap_or(0);
        // Room for the fresh generation and the discard's rotation.
        let generation = if newest + 2 < MAX_GENERATION {
            newest + 1
        } else {
            1
        };
        let w = random_key(self.rng.as_mut());
        self.keyring.store(&encode_keyring(&KeyringState {
            committed: (generation, w.clone()),
            base: (generation, w.clone()),
            staged: None,
        }))?;
        self.base = (generation, w.clone());
        self.committed = (generation, w);
        report.recovery.push(Recovery::FreshKeyring);
        if names.is_empty() {
            return Ok(());
        }
        self.discard(names, None, report)
    }

    /// Parses and authenticates `bytes` under `keys`, then loads it.
    /// Returns its head.
    fn load_verified(&mut self, bytes: &[u8], keys: &GenerationKeys) -> Option<u64> {
        let db = parse_db(bytes).ok()?;
        (keys.verify(bytes) && self.load(&db, keys)).then_some(db.head)
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
                key: Box::new(key),
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

    /// The startup discard: every record in the newest file that parses is
    /// retired through the full procedure, so its tombstones arrive with a
    /// generation change. Tombstones already in the file are kept. Reads at
    /// most two files.
    fn discard(
        &mut self,
        names: &[u64],
        recorded: Option<&Intent>,
        report: &mut OpenReport,
    ) -> Result<(), StoreError> {
        self.records.clear();
        self.tombstones.clear();
        let mut invalidate: Vec<Id16> = recorded.map(|i| i.invalidate.clone()).unwrap_or_default();
        let mut newest = None;
        for g in names.iter().rev().take(2) {
            if let Some(Ok(db)) = self.disk.read(&db_file(*g))?.map(|b| parse_db(&b)) {
                newest = Some(db);
                break;
            }
        }
        if let Some(db) = newest {
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
        let next_g = self.next_generation()?;
        let next_w = random_key(self.rng.as_mut());
        let mut op_id = [0u8; 16];
        self.rng.fill(&mut op_id);
        self.keyring.store(&encode_keyring(&KeyringState {
            committed: (self.committed.0, self.committed.1.clone()),
            base: (self.base.0, self.base.1.clone()),
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
        // The unreadable file is replaced by the base's new one.
        self.finish_rotation(next_g, next_w)
    }

    /// Steps 2 and 3 of the procedure, from what's in memory. Promotes at
    /// once if the keyring says its stores are durable (Windows).
    fn finish_rotation(&mut self, next_g: u64, next_w: Zeroizing<Key32>) -> Result<(), StoreError> {
        self.write_base(next_g)?;
        self.keyring.store(&encode_keyring(&KeyringState {
            committed: (next_g, next_w.clone()),
            base: (self.base.0, self.base.1.clone()),
            staged: None,
        }))?;
        self.committed = (next_g, next_w);
        if self.keyring.flush()? == Durability::Durable {
            let target = (self.committed.0, self.committed.1.clone());
            self.promote(target, None)?;
        }
        Ok(())
    }

    /// Moves the database to `target`, a committed generation and key known
    /// to be durable: write its file, record it as the base (keeping any
    /// staged rotation), remove the old file. Every blob that may survive
    /// holds the target key, so the old file is never needed again.
    fn promote(
        &mut self,
        target: (u64, Zeroizing<Key32>),
        staged: Option<(u64, Zeroizing<Key32>, Intent)>,
    ) -> Result<(), StoreError> {
        let old = self.base.0;
        for r in &mut self.records {
            r.wrapped = None;
        }
        let keys = GenerationKeys::new(&target.1, target.0);
        let head = self.committed.0.max(self.need);
        self.need = head;
        let bytes = self.encode(&keys, target.0, head);
        self.write_db(&db_file(target.0), &bytes)?;
        self.base = target;
        self.keyring.store(&encode_keyring(&KeyringState {
            committed: (self.committed.0, self.committed.1.clone()),
            base: (self.base.0, self.base.1.clone()),
            staged,
        }))?;
        self.disk.remove(&db_file(old))?;
        self.disk.sync_dir()?;
        Ok(())
    }

    /// The generation a rotation moves to: past the committed one and past
    /// every generation the database already needs.
    fn next_generation(&self) -> Result<u64, StoreError> {
        let next = self.committed.0.max(self.need) + 1;
        if valid_generation(next) {
            Ok(next)
        } else {
            Err(StoreError::Exhausted)
        }
    }

    /// Removes every database file but generation `keep`'s.
    fn remove_files_but(&mut self, names: &[u64], keep: u64) -> Result<(), StoreError> {
        let mut removed = false;
        for g in names.iter().filter(|g| **g != keep) {
            self.disk.remove(&db_file(*g))?;
            removed = true;
        }
        if removed {
            self.disk.sync_dir()?;
        }
        Ok(())
    }

    /// Writes memory to the base's file, recording `head` (or a newer one
    /// it already needs) as the newest committed generation it needs.
    fn write_base(&mut self, head: u64) -> io::Result<()> {
        let keys = GenerationKeys::new(&self.base.1, self.base.0);
        let g = self.base.0;
        let head = head.max(self.need);
        self.need = head;
        let bytes = self.encode(&keys, g, head);
        self.write_db(&db_file(g), &bytes)
    }

    fn write_db(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.disk.write_synced(DB_TMP, bytes)?;
        self.disk.rename(DB_TMP, name)?;
        self.disk.sync_dir()
    }

    /// Encodes memory as a database at `generation`, wrapping any record
    /// that has no wrap for it yet.
    fn encode(&mut self, keys: &GenerationKeys, generation: u64, head: u64) -> Vec<u8> {
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
            head,
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
        let need = self.need;
        if let Err(e) = self.write_base(self.committed.0) {
            self.need = need;
            undo(self);
            return Err(e.into());
        }
        self.refresh_snapshot();
        Ok(())
    }

    /// The invalidation procedure, steps 1 to 4.
    fn rotate(&mut self, invalidate: Vec<Id16>, promote: Vec<Id16>) -> Result<(), StoreError> {
        self.usable()?;
        let next_g = self.next_generation()?;
        let next_w = random_key(self.rng.as_mut());
        let mut op_id = [0u8; 16];
        self.rng.fill(&mut op_id);
        let intent = Intent {
            op_id,
            invalidate,
            promote,
        };
        // Step 1. Step 2 starts only after a store that succeeded (on
        // Windows, flushed).
        if let Err(e) = self.keyring.store(&encode_keyring(&KeyringState {
            committed: (self.committed.0, self.committed.1.clone()),
            base: (self.base.0, self.base.1.clone()),
            staged: Some((next_g, next_w.clone(), intent.clone())),
        })) {
            // A failed store may still have replaced the blob: read it back.
            // `Some(false)` only for a readable blob without our intent;
            // anything else is unknown.
            let ours = match self.keyring.load() {
                Ok(Some(b)) => match parse_keyring(&b) {
                    Ok(k) => Some(k.staged.is_some_and(|(_, _, i)| i.op_id == op_id)),
                    Err(_) => None,
                },
                Ok(None) | Err(_) => None,
            };
            match ours {
                // The old blob: nothing happened.
                Some(false) => return Err(e.into()),
                // Ours is visible, maybe not durable; or we can't tell. The
                // records are invalid in memory from now on, and a reopen
                // settles which way it went.
                _ => {
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
        if let Err(e) = self.finish_rotation(next_g, next_w) {
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
        self.committed.0
    }

    /// The generation the database is written under: the newest one known
    /// to be durable.
    pub fn base_generation(&self) -> u64 {
        self.base.0
    }

    /// The current in-memory view, for the session table.
    pub fn snapshot(&self) -> Arc<CredentialSnapshot> {
        self.snapshot.clone()
    }

    fn refresh_snapshot(&mut self) {
        let n = self.records.len();
        let mut snapshot = CredentialSnapshot {
            keys: HashMap::with_capacity(n),
            timestamps: HashMap::with_capacity(n),
            ..Default::default()
        };
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
    /// Boxed, so the map never moves a key.
    keys: HashMap<Id16, Box<Zeroizing<Key32>>>,
    timestamps: HashMap<Id16, u64>,
    slot_of: HashMap<Id16, Id16>,
    statuses: HashMap<Id16, SlotStatus>,
}

impl CredentialSnapshot {
    pub fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        self.keys.get(dev_id).map(|k| Zeroizing::new(***k))
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
