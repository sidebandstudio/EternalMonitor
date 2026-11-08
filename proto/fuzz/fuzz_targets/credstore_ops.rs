//! The credential store under fuzzer-chosen sessions: one store open across
//! several operations (rotations and plain writes mixed), crashes at
//! fuzzer-chosen steps, restarts in the same boot or after a power loss
//! that keeps a fuzzer-chosen keyring outcome, settling only when the input
//! says so, a keyring that flushes or doesn't, and damage to the files or
//! keyring.
//!
//! What must hold:
//! - the invariants, always;
//! - an operation that returned Ok left memory exactly as the same
//!   operation does uncrashed;
//! - a failed operation leaves memory exactly "before", or exactly "after"
//!   once its intent is visible;
//! - after any restart, every acknowledged write is there (R1): the state
//!   is exactly the acknowledged one, or, if an operation was killed, that
//!   or the state after it;
//! - damage never brings back a record;
//! - an operation that returned Ok did what it promises: a Remove leaves
//!   neither id held, an expire no pending record 7 days old;
//! - the store's tombstones are those an independent model of the
//!   operations expects (the last 8 per slot), a discard's those its files
//!   leave, and an add of a tombstoned `dev_id` is refused;
//! - a retired record (removed, expired, conflicted, discarded) is never
//!   served again, also when an earlier authentic copy of the database
//!   files, or an older keyring blob, is restored on its own; the only
//!   exception is the out-of-scope keyring rollback: a file that a keyring
//!   from before its retirement vouches for, or, once the keyring has
//!   rolled back, one whose head is above the keyring's generation;
//! - what a reopen after a restore serves survives a further power cut.
#![no_main]

use eternal_credstore::format::{key_fingerprint, parse_db, parse_keyring, GenerationKeys};
use eternal_credstore::keyring::MemKeyring;
use eternal_credstore::testing::{check_invariants, logical, Live, Logical, Store, World};
use eternal_credstore::{Disk, NewRecord, Recovery, StoreError};
use eternal_wire::v3::wire::RecordState;
use eternal_wire::v3::{Id16, Zeroizing};
use libfuzzer_sys::fuzz_target;

const DAY: u64 = 24 * 60 * 60;

fn id(n: u8) -> Id16 {
    [n; 16]
}

fn run(store: &mut Store, kind: u8, a: u8, b: u8, now: &mut u64) -> Result<(), StoreError> {
    let dev = 1 + a % 12;
    match kind % 6 {
        0 => store.add_pending(
            NewRecord {
                dev_id: id(dev),
                slot_id: id(100 + b % 4),
                key: Box::new(Zeroizing::new([dev; 32])),
                replaces: (b & 0x80 != 0).then(|| id(1 + (b >> 2) % 12)),
                device_name: "x".repeat(usize::from(b % 70)),
            },
            *now,
        ),
        1 => store.finalize(&id(dev)).map(drop),
        2 => store.remove(&[id(dev), id(1 + b % 12)]),
        3 => {
            *now += u64::from(b) * DAY / 16;
            store.expire(*now).map(drop)
        }
        4 => store.persist_timestamp(&id(dev), u64::from(b) << 8),
        _ => {
            let _ = store.would_conflict(&id(dev));
            Ok(())
        }
    }
}

/// Flips one byte of every database file, or drops the keyring. True if
/// anything held could be affected.
fn damage(world: &mut World, which: u8, a: u8, b: u8) -> bool {
    let mut names: Vec<String> = world
        .disk
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.starts_with("credentials-"))
        .collect();
    names.sort();
    if which & 1 == 1 || names.is_empty() {
        world.keyring.put(None);
        return !names.is_empty();
    }
    for name in &names {
        let mut bytes = world.disk.get(name).unwrap().clone();
        let at = usize::from(a) * bytes.len() / 256;
        bytes[at] ^= b | 1;
        world.disk.put(name, bytes);
    }
    true
}

/// A restart of `live`: the same boot, a clean shutdown and the next boot,
/// or a power loss keeping keyring outcome `d`.
fn restart(live: &Live, c: u8, d: u8) -> World {
    match c & 0x3 {
        0 => live.same_boot(),
        1 => live.at_rest(),
        _ => live.after_crash(c & 0x4 != 0, usize::from(d)),
    }
}

/// Every database file of `live`, as a restore would put them back.
fn db_files(live: &Live) -> Vec<(String, Vec<u8>)> {
    let disk = live.disk.borrow();
    let mut files: Vec<(String, Vec<u8>)> = disk
        .list()
        .unwrap()
        .into_iter()
        .filter(|n| n.starts_with("credentials-"))
        .map(|n| {
            let bytes = disk.get(&n).unwrap().clone();
            (n, bytes)
        })
        .collect();
    files.sort();
    files
}

/// What the oracle expects, kept from the operations alone (never from the
/// store's own output, except after damage or an injected restore).
struct Oracle {
    /// The state every acknowledged write adds up to.
    acked: Logical,
    /// The tombstones the operations so far leave, per slot, each with the
    /// step that added it (records retired in one step have no order the
    /// model can know), the last 8 kept.
    tombs: Vec<(Id16, Vec<(Id16, u64)>)>,
    /// Every retired `dev_id`, with the step it was retired at.
    gone: Vec<(Id16, u64)>,
    /// Every committed key the keyring has held, with the step it was first
    /// current at: a keyring whose key was current before a retirement is
    /// from before it. (Generation numbers can't tell: a keyring rollback
    /// reuses them.)
    keys: Vec<([u8; 32], u64)>,
    /// A keyring rollback happened: an open found a committed key older
    /// than the newest. From then on a file from an abandoned branch may
    /// pass as lost stores (the documented limit).
    rolled_back: bool,
    /// Steps so far.
    steps: u64,
}

/// Tombstones as comparable sets: per slot, sorted.
fn tomb_sets<'a>(tombs: impl Iterator<Item = (Id16, Vec<Id16>)> + 'a) -> Vec<(Id16, Vec<Id16>)> {
    let mut out: Vec<(Id16, Vec<Id16>)> = tombs
        .filter(|(_, ids)| !ids.is_empty())
        .map(|(s, mut ids)| {
            ids.sort();
            (s, ids)
        })
        .collect();
    out.sort();
    out
}

impl Oracle {
    fn tombstoned(&self, id: &Id16) -> bool {
        self.tombs
            .iter()
            .any(|(_, ids)| ids.iter().any(|(t, _)| t == id))
    }

    /// An operation took the store from `acked` to `new`. Every record that
    /// left is retired into its slot; an add of a tombstoned `dev_id` must
    /// have been refused; each slot keeps its last 8 (older steps go first;
    /// among records retired in one step, the store may keep any); and the
    /// store's tombstones must be the model's.
    fn step(&mut self, new: Logical, key: Option<[u8; 32]>, added: Option<Id16>) {
        self.steps += 1;
        let step = self.steps;
        let batch: Vec<(Id16, Id16)> = self
            .acked
            .records
            .iter()
            .filter(|r| !new.records.iter().any(|n| n.0 == r.0))
            .map(|r| (r.1, r.0))
            .collect();
        for (slot, id) in &batch {
            let i = match self.tombs.iter().position(|(s, _)| s == slot) {
                Some(i) => i,
                None => {
                    self.tombs.push((*slot, Vec::new()));
                    self.tombs.len() - 1
                }
            };
            let list = &mut self.tombs[i].1;
            if !list.iter().any(|(t, _)| t == id) {
                list.push((*id, step));
            }
        }
        if batch.is_empty() {
            self.see(key);
        } else {
            self.retire(batch.iter().map(|(_, id)| *id), key);
        }
        if let Some(id) = added {
            let landed = new.records.iter().any(|n| n.0 == id);
            let was_held = self.acked.records.iter().any(|r| r.0 == id);
            if landed && !was_held {
                assert!(
                    !self.tombstoned(&id),
                    "an add of a tombstoned dev_id is refused"
                );
                self.gone.retain(|(g, _)| *g != id);
            }
        }
        for (slot, list) in &mut self.tombs {
            if list.len() <= 8 {
                continue;
            }
            list.sort_by_key(|(_, st)| *st);
            let cut = list.len() - 8;
            let tie = list[cut].1;
            if list[cut - 1].1 != tie {
                list.drain(..cut);
                continue;
            }
            // The boundary falls inside one step: the store's choice among
            // those, if the counts fit.
            let theirs: Vec<Id16> = new
                .tombstones
                .iter()
                .find(|(s, _)| s == slot)
                .map(|(_, ids)| ids.clone())
                .unwrap_or_default();
            let newer: Vec<(Id16, u64)> =
                list.iter().filter(|(_, st)| *st > tie).copied().collect();
            let chosen: Vec<(Id16, u64)> = list
                .iter()
                .filter(|(t, st)| *st == tie && theirs.contains(t))
                .copied()
                .collect();
            assert_eq!(newer.len() + chosen.len(), 8, "the last 8 per slot");
            *list = newer.into_iter().chain(chosen).collect();
        }
        assert_eq!(
            tomb_sets(new.tombstones.iter().cloned()),
            tomb_sets(
                self.tombs
                    .iter()
                    .map(|(s, ids)| (*s, ids.iter().map(|(t, _)| *t).collect()))
            ),
            "the store's tombstones are the model's"
        );
        self.acked = new;
    }

    /// Notes `key` as current from this step on, if it's new.
    fn see(&mut self, key: Option<[u8; 32]>) {
        if let Some(key) = key {
            if !self.keys.iter().any(|(k, _)| *k == key) {
                self.keys.push((key, self.steps));
            }
        }
    }

    /// Retires `ids` at this step, whose rotation made `key` current: that
    /// key is current only from this step on.
    fn retire(&mut self, ids: impl Iterator<Item = Id16>, key: Option<[u8; 32]>) {
        if let Some(key) = key {
            self.keys.retain(|(k, _)| *k != key);
            self.keys.push((key, self.steps));
        }
        for id in ids {
            if !self.gone.iter().any(|(g, _)| *g == id) {
                self.gone.push((id, self.steps));
            }
        }
    }

    /// Takes `tombs` (per slot, oldest first) as the model, in that order.
    fn adopt(&mut self, tombs: &[(Id16, Vec<Id16>)]) {
        self.tombs = tombs
            .iter()
            .map(|(s, ids)| {
                let list = ids
                    .iter()
                    .map(|t| {
                        self.steps += 1;
                        (*t, self.steps)
                    })
                    .collect();
                (*s, list)
            })
            .collect();
    }

    /// After damage or an injected restore. Unless the open loaded a file,
    /// its tombstones were modelled there; an earlier copy that loaded
    /// brings its own, which become the model in the store's order. (What
    /// it may serve is checked at the open.)
    fn resync(&mut self, new: Logical, loaded: bool) {
        if loaded {
            self.adopt(&new.tombstones);
        }
        self.acked = new;
    }
}

/// What an open of `world` does, worked out from its keyring and files
/// alone.
enum Expect {
    /// It loads a file the keyring vouches for: the keyring's committed
    /// key, and whether the file's head is above its generation.
    Load([u8; 32], bool),
    /// No file at all: an empty store, with nothing retired.
    Empty,
    /// Nothing loads, so it discards: it serves no record, and these
    /// tombstones; it retires these records.
    Discard(Vec<(Id16, Vec<Id16>)>, Vec<Id16>),
}

fn expect(world: &World) -> Expect {
    let mut names: Vec<u64> = world
        .disk
        .list()
        .unwrap()
        .iter()
        .filter_map(|n| {
            n.strip_prefix("credentials-")?
                .strip_suffix(".db")?
                .parse()
                .ok()
        })
        .collect();
    names.sort_unstable();
    if let Some(blob) = world.keyring.get() {
        let k = parse_keyring(blob).expect("the fuzzer never garbles a keyring blob");
        // The committed generation's file, then the base's: the first that
        // verifies under its key and that this keyring's history vouches
        // for. Its head is at least the committed generation, and a head
        // equal to the committed or staged generation carries that key's
        // fingerprint. (A higher head is the documented keyring-rollback
        // limit.)
        let mut candidates = vec![(k.committed.0, &k.committed.1)];
        if k.base.0 != k.committed.0 {
            candidates.push((k.base.0, &k.base.1));
        }
        for (g, w) in candidates {
            let Some(bytes) = world.disk.get(&format!("credentials-{g}.db")) else {
                continue;
            };
            let Ok(db) = parse_db(bytes) else { continue };
            if !GenerationKeys::new(w, g).verify(bytes) {
                continue;
            }
            let foreign =
                |g: u64, w: &[u8; 32]| db.head == g && db.head_fp != key_fingerprint(w, g);
            let ok = db.head >= k.committed.0
                && !foreign(k.committed.0, &k.committed.1)
                && !k
                    .staged
                    .as_ref()
                    .is_some_and(|(sg, sw, _)| foreign(*sg, sw));
            if ok {
                return Expect::Load(*k.committed.1, db.head > k.committed.0);
            }
        }
    }
    if names.is_empty() {
        return Expect::Empty;
    }
    // The discard: the newest of the two newest files that parses keeps its
    // tombstones, and each of its records is retired into its slot (the last
    // 8 kept). The fuzzer's slots stay far below the slot cap.
    let mut tombs: Vec<(Id16, Vec<Id16>)> = Vec::new();
    let mut retired = Vec::new();
    for g in names.iter().rev().take(2) {
        let Some(Ok(db)) = world
            .disk
            .get(&format!("credentials-{g}.db"))
            .map(|b| parse_db(b))
        else {
            continue;
        };
        tombs = db.tombstones.clone();
        for r in &db.records {
            retired.push(r.dev_id);
            let i = match tombs.iter().position(|(s, _)| *s == r.slot_id) {
                Some(i) => i,
                None => {
                    tombs.push((r.slot_id, Vec::new()));
                    tombs.len() - 1
                }
            };
            let mut list = tombs.remove(i).1;
            if !list.contains(&r.dev_id) {
                list.push(r.dev_id);
            }
            if list.len() > 8 {
                list.drain(..list.len() - 8);
            }
            tombs.push((r.slot_id, list));
        }
        break;
    }
    Expect::Discard(tombs, retired)
}

/// Opens `world` and checks it against [`expect`]. With no file, it's empty.
/// A discard serves no record and the modelled tombstones, which become the
/// model, and every record it drops (held, or in the file it read) is
/// retired. A load serves no retired record, except one the out-of-scope
/// keyring rollback allows: the keyring is from before its retirement, or,
/// after a rollback, the file's head is above the keyring's generation.
/// Such a record is forgiven.
/// `adding` is a `dev_id` the operation being recovered may have added,
/// which the model allows: it may be served. Returns whether it loaded a
/// file.
fn open_checked(world: &World, oracle: &mut Oracle, adding: Option<Id16>) -> (Store, Live, bool) {
    let pre = expect(world);
    if let Some(blob) = world.keyring.get() {
        let key = *parse_keyring(blob).unwrap().committed.1;
        if let Some(&(_, from)) = oracle.keys.iter().find(|(k, _)| *k == key) {
            oracle.rolled_back |= oracle.keys.iter().any(|(_, at)| *at > from);
        }
    }
    let held: Vec<Id16> = oracle.acked.records.iter().map(|r| r.0).collect();
    let (store, live) = open(world);
    let (committed, above) = match pre {
        Expect::Load(committed, above) => (committed, above),
        Expect::Empty => {
            assert!(store.records().is_empty(), "no file, no record");
            assert!(store.tombstones().is_empty(), "no file, no tombstone");
            oracle.adopt(&[]);
            oracle.see(committed_key(&live));
            return (store, live, false);
        }
        Expect::Discard(tombs, retired) => {
            assert!(store.records().is_empty(), "a discard serves no record");
            assert_eq!(
                tomb_sets(store.tombstones().into_iter()),
                tomb_sets(tombs.iter().cloned()),
                "the store's tombstones are the discard's"
            );
            oracle.adopt(&tombs);
            oracle.steps += 1;
            oracle.retire(held.into_iter().chain(retired), committed_key(&live));
            return (store, live, false);
        }
    };
    let current_from = oracle
        .keys
        .iter()
        .find(|(k, _)| *k == committed)
        .map(|(_, at)| *at);
    for r in store.records() {
        if Some(r.dev_id) == adding {
            continue;
        }
        let Some(&(_, at)) = oracle.gone.iter().find(|(id, _)| *id == r.dev_id) else {
            continue;
        };
        assert!(
            current_from.is_some_and(|from| from < at) || (oracle.rolled_back && above),
            "a retired record never comes back"
        );
        oracle.gone.retain(|(id, _)| *id != r.dev_id);
    }
    oracle.see(committed_key(&live));
    (store, live, true)
}

/// The committed key of `live`'s keyring.
fn committed_key(live: &Live) -> Option<[u8; 32]> {
    let keyring = live.keyring.borrow();
    parse_keyring(keyring.get()?).ok().map(|k| *k.committed.1)
}

/// Opens `world`. A fresh keyring (first run, or after the keyring was
/// dropped) is settled first: losing it to a power cut before it's durable
/// is the documented first-run case, outside R1.
fn open(world: &World) -> (Store, Live) {
    let (store, report, live) = world.open();
    check_invariants(&store);
    if report.recovery.contains(&Recovery::FreshKeyring) {
        drop(store);
        let (store, _, live) = live.at_rest().open();
        check_invariants(&store);
        return (store, live);
    }
    (store, live)
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, data)) = data.split_first() else {
        return;
    };
    let world = World {
        keyring: if mode & 1 == 0 {
            MemKeyring::new()
        } else {
            MemKeyring::flushing()
        },
        ..World::default()
    };
    let (mut store, mut live) = open(&world);
    let mut oracle = Oracle {
        acked: logical(&store),
        tombs: Vec::new(),
        gone: Vec::new(),
        keys: Vec::new(),
        rolled_back: false,
        steps: 0,
    };
    oracle.see(committed_key(&live));
    // Earlier authentic copies, for restores.
    let mut snapshots: Vec<Vec<(String, Vec<u8>)>> = Vec::new();
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    let mut now = 400 * DAY;
    for chunk in data.chunks_exact(5).take(40) {
        let (kind, a, b, c, d) = (chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]);
        if snapshots.len() < 8 {
            snapshots.push(db_files(&live));
            if let Some(blob) = live.keyring.borrow().get() {
                blobs.push(blob.clone());
            }
        }
        if kind >= 0xF0 && kind & 2 == 2 {
            // Restore an earlier copy on its own, in the same boot or after
            // a reboot: the database files, or the keyring blob.
            drop(store);
            let mut restored = if c & 1 == 0 {
                live.same_boot()
            } else {
                live.at_rest()
            };
            if kind & 1 == 0 {
                let snapshot = &snapshots[usize::from(a) % snapshots.len()];
                for (name, _) in db_files(&live) {
                    restored.disk.remove_now(&name);
                }
                for (name, bytes) in snapshot {
                    restored.disk.put(name, bytes.clone());
                }
            } else if !blobs.is_empty() {
                restored
                    .keyring
                    .put(Some(blobs[usize::from(a) % blobs.len()].clone()));
            }
            let loaded;
            (store, live, loaded) = open_checked(&restored, &mut oracle, None);
            let served = logical(&store);
            oracle.resync(served.clone(), loaded);
            // What the reopen serves survives a further power cut.
            drop(store);
            (store, live, _) = open_checked(
                &live.after_crash(c & 2 != 0, usize::from(d)),
                &mut oracle,
                None,
            );
            assert_eq!(logical(&store), served, "what a reopen serves survives");
            continue;
        }
        if kind >= 0xF0 {
            // Damage at rest, then a restart.
            drop(store);
            let mut damaged = live.at_rest();
            let held: Vec<Id16> = oracle.acked.records.iter().map(|r| r.0).collect();
            let touched = damage(&mut damaged, kind, a, b);
            let loaded;
            (store, live, loaded) = open_checked(&damaged, &mut oracle, None);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            oracle.resync(logical(&store), loaded);
            continue;
        }
        if kind >= 0xE0 {
            // A restart with every write acknowledged: exactly that state.
            drop(store);
            (store, live, _) = open_checked(&restart(&live, c, d), &mut oracle, None);
            assert_eq!(
                logical(&store),
                oracle.acked,
                "every acknowledged write survives"
            );
            continue;
        }
        // An add's `dev_id`, for the model's re-add rule.
        let added = (kind % 6 == 0).then(|| id(1 + a % 12));
        // The same operation, uncrashed, from the same state.
        let after = {
            let (mut clean, _) = open(&live.same_boot());
            let mut clean_now = now;
            let done = run(&mut clean, kind, a, b, &mut clean_now).is_ok();
            let after = logical(&clean);
            if done {
                // What the operation promises, independently of the store.
                let held = |dev: Id16| after.records.iter().any(|r| r.0 == dev);
                match kind % 6 {
                    2 => assert!(
                        !held(id(1 + a % 12)) && !held(id(1 + b % 12)),
                        "a Remove leaves neither id held"
                    ),
                    3 => assert!(
                        !after
                            .records
                            .iter()
                            .any(|r| r.2 == RecordState::Pending as u8
                                && clean_now.saturating_sub(r.5) >= 7 * DAY),
                        "an expire leaves no pending record 7 days old"
                    ),
                    _ => {}
                }
            }
            after
        };
        let keyring_before = live.keyring.borrow().get().cloned();
        let crash_at = (c & 0x80 != 0).then(|| live.steps.count.get() + usize::from(c & 0x0F));
        live.steps.crash_at.set(crash_at);
        live.steps.ambiguous.set(c & 0x20 != 0);
        let result = run(&mut store, kind, a, b, &mut now);
        check_invariants(&store);
        let crashed = crash_at.is_some_and(|at| live.steps.count.get() > at);
        live.steps.crash_at.set(None);
        if !crashed {
            // Ok or a refusal: exactly what the uncrashed run did.
            assert_eq!(logical(&store), after);
            oracle.step(after, committed_key(&live), added);
            continue;
        }
        let visible = live.keyring.borrow().get().cloned() != keyring_before;
        let before = oracle.acked.clone();
        match result {
            Ok(()) => assert_eq!(logical(&store), after),
            Err(_) if visible => assert_eq!(logical(&store), after),
            Err(_) => assert_eq!(logical(&store), before),
        }
        // The process died here: restart.
        drop(store);
        let mut next = restart(&live, c >> 4, d);
        if c & 0x10 != 0 && d & 1 == 1 {
            // Damage in the middle: nothing held may come back.
            let held: Vec<Id16> = before.records.iter().map(|r| r.0).collect();
            let touched = damage(&mut next, d >> 1, a, b);
            let loaded;
            (store, live, loaded) = open_checked(&next, &mut oracle, None);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            oracle.resync(logical(&store), loaded);
            continue;
        }
        // A killed add the model allows may have landed.
        let adding = added.filter(|id| {
            !oracle.tombstoned(id) && !oracle.acked.records.iter().any(|r| r.0 == *id)
        });
        (store, live, _) = open_checked(&next, &mut oracle, adding);
        let reached = logical(&store);
        if result.is_ok() {
            assert_eq!(reached, after, "it returned Ok");
        } else {
            assert!(
                reached == before || reached == after,
                "exactly before or after"
            );
        }
        oracle.step(reached, committed_key(&live), added);
    }
});
