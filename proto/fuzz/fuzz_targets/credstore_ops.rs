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
//! - the store's tombstones are those an independent model of the
//!   operations expects (the last 8 per slot), and an add of a tombstoned
//!   `dev_id` is refused;
//! - a retired record (removed, expired, conflicted, discarded) is never
//!   served again, also when an earlier authentic copy of the database
//!   files, or an older keyring blob, is restored on its own; the only
//!   exception is the out-of-scope keyring rollback below its retirement,
//!   and only from a file that rolled-back keyring's history vouches for;
//! - what a reopen after a restore serves survives a further power cut.
#![no_main]

use eternal_credstore::format::{key_fingerprint, parse_db, parse_keyring, GenerationKeys};
use eternal_credstore::keyring::MemKeyring;
use eternal_credstore::testing::{check_invariants, logical, Live, Logical, Store, World};
use eternal_credstore::{Disk, NewRecord, Recovery, StoreError};
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
    /// Every retired `dev_id`, with the committed generation it was retired
    /// under.
    gone: Vec<(Id16, u64)>,
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
    fn step(&mut self, new: Logical, generation: u64, added: Option<Id16>) {
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
            if !self.gone.iter().any(|(g, _)| g == id) {
                self.gone.push((*id, generation));
            }
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

    /// After damage or an injected restore the store may serve other
    /// tombstones (a discard, an earlier copy): they become the model, in the
    /// store's order. (What it may serve is checked at the open.)
    fn resync(&mut self, new: Logical) {
        self.tombs = new
            .tombstones
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
        self.acked = new;
    }
}

/// What the keyring and files of `world` let an open load, worked out
/// independently: the keyring's committed generation, and whether the file
/// it will load is one that keyring's history vouches for (its head is the
/// committed or staged generation with that key's fingerprint, or above the
/// committed one: the documented keyring-rollback limit).
fn vouched(world: &World) -> Option<(u64, bool)> {
    let k = parse_keyring(world.keyring.get()?).ok()?;
    let mut candidates = vec![(k.committed.0, k.committed.1.clone())];
    if k.base.0 != k.committed.0 {
        candidates.push((k.base.0, k.base.1.clone()));
    }
    for (g, w) in candidates {
        let Some(bytes) = world.disk.get(&format!("credentials-{g}.db")) else {
            continue;
        };
        let Ok(db) = parse_db(bytes) else { continue };
        if !GenerationKeys::new(&w, g).verify(bytes) {
            continue;
        }
        let at = |g: u64, w: &[u8; 32]| db.head == g && db.head_fp == key_fingerprint(w, g);
        let ok = db.head > k.committed.0
            || at(k.committed.0, &k.committed.1)
            || k.staged.as_ref().is_some_and(|(sg, sw, _)| at(*sg, sw));
        return Some((k.committed.0, ok));
    }
    None
}

/// Opens `world` and checks it serves no retired record, except one the
/// out-of-scope keyring rollback allows: the keyring is below the generation
/// it was retired under, and the file loaded is one that keyring's history
/// vouches for. Such a record is forgiven.
/// `adding` is a `dev_id` the operation being recovered may have added,
/// which the model allows: it may be served.
fn open_checked(world: &World, oracle: &mut Oracle, adding: Option<Id16>) -> (Store, Live) {
    let pre = vouched(world);
    let (store, live) = open(world);
    for r in store.records() {
        if Some(r.dev_id) == adding {
            continue;
        }
        let Some(&(_, g)) = oracle.gone.iter().find(|(id, _)| *id == r.dev_id) else {
            continue;
        };
        let allowed = pre.is_some_and(|(committed, ok)| committed < g && ok);
        assert!(allowed, "a retired record never comes back");
        oracle.gone.retain(|(id, _)| *id != r.dev_id);
    }
    (store, live)
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
        steps: 0,
    };
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
            (store, live) = open_checked(&restored, &mut oracle, None);
            let served = logical(&store);
            oracle.resync(served.clone());
            // What the reopen serves survives a further power cut.
            drop(store);
            (store, live) = open_checked(
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
            (store, live) = open_checked(&damaged, &mut oracle, None);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            oracle.resync(logical(&store));
            continue;
        }
        if kind >= 0xE0 {
            // A restart with every write acknowledged: exactly that state.
            drop(store);
            (store, live) = open_checked(&restart(&live, c, d), &mut oracle, None);
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
            let _ = run(&mut clean, kind, a, b, &mut clean_now);
            logical(&clean)
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
            oracle.step(after, store.generation(), added);
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
            (store, live) = open_checked(&next, &mut oracle, None);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            oracle.resync(logical(&store));
            continue;
        }
        // A killed add the model allows may have landed.
        let adding = added.filter(|id| {
            !oracle.tombstoned(id) && !oracle.acked.records.iter().any(|r| r.0 == *id)
        });
        (store, live) = open_checked(&next, &mut oracle, adding);
        let reached = logical(&store);
        if result.is_ok() {
            assert_eq!(reached, after, "it returned Ok");
        } else {
            assert!(
                reached == before || reached == after,
                "exactly before or after"
            );
        }
        oracle.step(reached, store.generation(), added);
    }
});
