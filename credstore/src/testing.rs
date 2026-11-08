//! Crash-testing helpers for tests and fuzz targets: a disk and keyring
//! that fail at one shared step count, and the store's logical state for
//! before/after comparisons.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io;
use std::rc::Rc;

use eternal_wire::v3::wire::RecordState;
use eternal_wire::v3::{Id16, Key32, SeededRandom, Zeroizing};

use crate::disk::{Disk, MemDisk};
use crate::keyring::{Durability, Keyring, MemKeyring};
use crate::store::{CredentialStore, OpenReport, StoreError};

/// Counts every write to the disk and the keyring together; the one at
/// `crash_at` and every later one fail. An interrupted file write leaves
/// half its bytes.
#[derive(Clone, Default)]
pub struct Steps {
    pub count: Rc<Cell<usize>>,
    pub crash_at: Rc<Cell<Option<usize>>>,
    /// The failing keyring store is made visible anyway (not durable), then
    /// reports the error.
    pub ambiguous: Rc<Cell<bool>>,
    /// Keyring loads fail.
    pub fail_load: Rc<Cell<bool>>,
    /// A failing keyring store leaves a blob that can't be parsed visible.
    pub garble: Rc<Cell<bool>>,
    /// Database files read.
    pub db_reads: Rc<Cell<usize>>,
}

impl Steps {
    fn step(&self) -> io::Result<()> {
        let n = self.count.get();
        self.count.set(n + 1);
        if self.crash_at.get().is_some_and(|at| n >= at) {
            return Err(io::Error::other("simulated crash"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct SharedDisk {
    pub inner: Rc<RefCell<MemDisk>>,
    pub steps: Steps,
}

impl Disk for SharedDisk {
    fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        if name.starts_with("credentials-") {
            self.steps.db_reads.set(self.steps.db_reads.get() + 1);
        }
        self.inner.borrow().read(name)
    }
    fn list(&self) -> io::Result<Vec<String>> {
        self.inner.borrow().list()
    }
    fn write_synced(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        if let Err(e) = self.steps.step() {
            self.inner.borrow_mut().write_torn(name, bytes);
            return Err(e);
        }
        self.inner.borrow_mut().write_synced(name, bytes)
    }
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        self.steps.step()?;
        self.inner.borrow_mut().rename(from, to)
    }
    fn sync_dir(&mut self) -> io::Result<()> {
        self.steps.step()?;
        self.inner.borrow_mut().sync_dir()
    }
    fn remove(&mut self, name: &str) -> io::Result<()> {
        self.steps.step()?;
        self.inner.borrow_mut().remove(name)
    }
}

#[derive(Clone)]
pub struct SharedKeyring {
    pub inner: Rc<RefCell<MemKeyring>>,
    pub steps: Steps,
}

impl Keyring for SharedKeyring {
    fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        if self.steps.fail_load.get() {
            return Err(io::Error::other("simulated load failure"));
        }
        self.inner.borrow().load()
    }
    fn store(&mut self, blob: &[u8]) -> io::Result<()> {
        if let Err(e) = self.steps.step() {
            if self.steps.garble.get() {
                self.inner.borrow_mut().store_visible(b"garbled", false);
            } else if self.steps.ambiguous.get() {
                self.inner.borrow_mut().store_visible(blob, false);
            }
            return Err(e);
        }
        self.inner.borrow_mut().store(blob)
    }
    fn flush(&mut self) -> io::Result<Durability> {
        self.inner.borrow_mut().flush()
    }
}

/// A disk and keyring at rest, and how to open a store on them.
#[derive(Clone, Default)]
pub struct World {
    pub disk: MemDisk,
    pub keyring: MemKeyring,
}

pub type Store = CredentialStore<SharedDisk, SharedKeyring>;

pub struct Live {
    pub disk: Rc<RefCell<MemDisk>>,
    pub keyring: Rc<RefCell<MemKeyring>>,
    pub steps: Steps,
}

impl Live {
    /// What a restart finds: durable files (unsynced renames kept or lost)
    /// and keyring choice `keyring` of what may have survived (see
    /// [`MemKeyring::after_crash`]).
    pub fn after_crash(&self, keep_unsynced: bool, keyring: usize) -> World {
        World {
            disk: self.disk.borrow().after_crash(keep_unsynced),
            keyring: self.keyring.borrow().after_crash(keyring),
        }
    }

    /// Everything synced and the last keyring blob kept: a clean shutdown
    /// and the next boot.
    pub fn at_rest(&self) -> World {
        self.after_crash(false, usize::MAX)
    }

    /// The process restarts in the same boot: nothing lost, nothing more
    /// durable.
    pub fn same_boot(&self) -> World {
        World {
            disk: self.disk.borrow().restart(),
            keyring: self.keyring.borrow().restart(),
        }
    }

    /// How many keyring blobs a crash now may leave.
    pub fn keyring_choices(&self) -> usize {
        self.keyring.borrow().crash_choices()
    }
}

impl World {
    /// A seed for an open of this world: `seed` mixed with everything the
    /// open will read, so runs stay reproducible while opens of different
    /// states never draw the same keys (real keys come from the system's
    /// random source).
    fn seed(&self, seed: u64) -> u64 {
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        ctx.update(&seed.to_le_bytes());
        for name in self.disk.list().unwrap_or_default() {
            ctx.update(name.as_bytes());
            if let Some(bytes) = self.disk.get(&name) {
                ctx.update(bytes);
            }
        }
        if let Some(blob) = self.keyring.get() {
            ctx.update(blob);
        }
        ctx.update(&self.keyring.boot().to_le_bytes());
        let digest = ctx.finish();
        u64::from_le_bytes(digest.as_ref()[..8].try_into().expect("8 bytes"))
    }

    pub fn live(&self) -> Live {
        Live {
            disk: Rc::new(RefCell::new(self.disk.clone())),
            keyring: Rc::new(RefCell::new(self.keyring.clone())),
            steps: Steps::default(),
        }
    }

    /// Opens with a crash at step `crash_at`; with `ambiguous`, a failing
    /// keyring store happens anyway.
    pub fn open_live(
        &self,
        seed: u64,
        crash_at: Option<usize>,
        ambiguous: bool,
    ) -> (Live, Result<(Store, OpenReport), StoreError>) {
        self.open_live_with(seed, |steps| {
            steps.crash_at.set(crash_at);
            steps.ambiguous.set(ambiguous);
        })
    }

    /// Opens after `setup` has set the failure hooks.
    pub fn open_live_with(
        &self,
        seed: u64,
        setup: impl FnOnce(&Steps),
    ) -> (Live, Result<(Store, OpenReport), StoreError>) {
        let live = self.live();
        setup(&live.steps);
        let disk = SharedDisk {
            inner: live.disk.clone(),
            steps: live.steps.clone(),
        };
        let keyring = SharedKeyring {
            inner: live.keyring.clone(),
            steps: live.steps.clone(),
        };
        let opened =
            CredentialStore::open(disk, keyring, Box::new(SeededRandom::new(self.seed(seed))));
        (live, opened)
    }

    pub fn open(&self) -> (Store, OpenReport, Live) {
        let (live, opened) = self.open_live(7, None, false);
        let (store, report) = opened.expect("open");
        (store, report, live)
    }
}

/// dev_id, slot, state, replaces, name, created, timestamp, key.
pub type LogicalRecord = (Id16, Id16, u8, Option<Id16>, String, u64, u64, Key32);

/// The store's logical content: what must be exactly "before" or "after".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logical {
    pub records: Vec<LogicalRecord>,
    pub tombstones: Vec<(Id16, Vec<Id16>)>,
}

pub fn logical(store: &Store) -> Logical {
    let snapshot = store.snapshot();
    let mut records: Vec<_> = store
        .records()
        .into_iter()
        .map(|r| {
            (
                r.dev_id,
                r.slot_id,
                r.state as u8,
                r.replaces,
                r.device_name,
                r.created_at_s,
                snapshot.last_committed_timestamp(&r.dev_id),
                *snapshot.key(&r.dev_id).expect("every record has its key"),
            )
        })
        .collect();
    records.sort();
    let mut tombstones = store.tombstones();
    tombstones.sort();
    Logical {
        records,
        tombstones,
    }
}

/// The store's invariants.
pub fn check_invariants(store: &Store) {
    let records = store.records();
    let snapshot = store.snapshot();
    assert!(records.len() <= 64);
    assert_eq!(snapshot.len(), records.len());
    let mut ids = HashSet::new();
    let tombstoned: HashSet<Id16> = store
        .tombstones()
        .into_iter()
        .flat_map(|(_, ids)| ids)
        .collect();
    for r in &records {
        assert!(ids.insert(r.dev_id), "unique dev_id");
        assert!(
            !tombstoned.contains(&r.dev_id),
            "a held record is never tombstoned"
        );
        let in_slot: Vec<_> = records.iter().filter(|o| o.slot_id == r.slot_id).collect();
        assert!(
            in_slot
                .iter()
                .filter(|o| o.state == RecordState::Active)
                .count()
                <= 1
        );
        assert!(
            in_slot
                .iter()
                .filter(|o| o.state == RecordState::Pending)
                .count()
                <= 3
        );
        if r.state == RecordState::Active {
            assert_eq!(r.replaces, None);
        }
        let status = snapshot.slot_status(&r.dev_id);
        assert!(status.held.contains(&(r.dev_id, r.state)));
        // What the session table decides from the snapshot is the store's.
        assert_eq!(
            snapshot.would_conflict(&r.dev_id),
            store.would_conflict(&r.dev_id)
        );
        assert_eq!(snapshot.replaces(&r.dev_id), r.replaces);
        assert_eq!(snapshot.slot_of(&r.dev_id), Some(r.slot_id));
        let active = records
            .iter()
            .find(|o| o.slot_id == r.slot_id && o.state == RecordState::Active)
            .map(|o| o.dev_id);
        assert_eq!(snapshot.active_in(&r.slot_id), active);
        assert_eq!(
            snapshot.is_pending(&r.dev_id),
            r.state == RecordState::Pending
        );
    }
    for id in &tombstoned {
        assert!(snapshot.would_conflict(id) && store.would_conflict(id));
    }
    // Expiry, at each record's boundary and just before it.
    for r in &records {
        let at = r.created_at_s + crate::store::PENDING_LIFETIME_S;
        for now_s in [at - 1, at] {
            let mut expected: Vec<Id16> = records
                .iter()
                .filter(|o| {
                    o.state == RecordState::Pending
                        && now_s.saturating_sub(o.created_at_s) >= crate::store::PENDING_LIFETIME_S
                })
                .map(|o| o.dev_id)
                .collect();
            expected.sort_unstable();
            assert_eq!(snapshot.expiring(now_s), expected);
        }
    }
    for (_, ids) in store.tombstones() {
        assert!(ids.len() <= 8);
    }
}
