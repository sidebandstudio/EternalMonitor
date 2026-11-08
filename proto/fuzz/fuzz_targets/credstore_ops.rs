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
//! - a retired record (removed, expired, conflicted, discarded) never comes
//!   back, also when an earlier authentic copy of
//!   the database files, or an older keyring blob, is restored on its own;
//!   and what such a reopen serves survives a further power cut.
#![no_main]

use eternal_credstore::format::parse_keyring;
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

/// Moves `acked` to `new`. With `retiring`, the step was an operation (or
/// a discard): every `dev_id` it retired is remembered with the committed
/// generation it was retired under. The fuzzer reuses `dev_id`s, so a
/// retired one is forgiven only when this step was an add of that id that
/// landed (returned Ok, or was killed after it landed) and it wasn't
/// tombstoned before the step.
fn advance(
    acked: &mut Logical,
    new: Logical,
    gone: &mut Vec<(Id16, u64)>,
    generation: u64,
    retiring: bool,
    added: Option<Id16>,
) {
    let had = |id: &Id16| acked.tombstones.iter().any(|(_, ids)| ids.contains(id));
    if retiring {
        let left = acked
            .records
            .iter()
            .map(|r| r.0)
            .filter(|id| !new.records.iter().any(|n| n.0 == *id));
        // Tombstones this step added (not ones a restore brought back).
        let tombstoned = new
            .tombstones
            .iter()
            .flat_map(|(_, ids)| ids.iter().copied())
            .filter(|id| !had(id));
        for id in left.chain(tombstoned).collect::<Vec<_>>() {
            if !gone.iter().any(|(g, _)| *g == id) {
                gone.push((id, generation));
            }
        }
    }
    if let Some(id) = added {
        let landed = new.records.iter().any(|n| n.0 == id);
        let was_held = acked.records.iter().any(|r| r.0 == id);
        if landed && !was_held && !had(&id) {
            gone.retain(|(g, _)| *g != id);
        }
    }
    *acked = new;
}

/// A step that rolled the keyring back (stores lost to a power cut, an
/// injected restore of an older blob, or a dropped keyring) can undo the
/// rotations that retired records: §4.3 puts a keyring rollback out of
/// scope, so retirements under generations above the keyring's committed
/// one are forgotten. Nothing else forgives a retirement.
fn rolled_back(gone: &mut Vec<(Id16, u64)>, keyring: Option<&Vec<u8>>) {
    let committed = keyring
        .and_then(|b| parse_keyring(b).ok())
        .map_or(0, |k| k.committed.0);
    gone.retain(|(_, g)| *g <= committed);
}

/// The restart world `next` lost keyring stores that `live` had visible.
fn lost_keyring_stores(live: &Live, next: &World) -> bool {
    live.keyring.borrow().get() != next.keyring.get()
}

/// No retired record is served again.
fn assert_none_back(store: &Store, gone: &[(Id16, u64)]) {
    for r in store.records() {
        assert!(
            !gone.iter().any(|(g, _)| *g == r.dev_id),
            "a retired record never comes back"
        );
    }
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
    // The state every acknowledged write adds up to, and every record that
    // ever left it.
    let mut acked: Logical = logical(&store);
    let mut gone: Vec<(Id16, u64)> = Vec::new();
    // Earlier authentic copies, for restores.
    let mut snapshots: Vec<Vec<(String, Vec<u8>)>> = Vec::new();
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    let mut now = 400 * DAY;
    for chunk in data.chunks_exact(5).take(40) {
        let (kind, a, b, c, d) = (chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]);
        assert_none_back(&store, &gone);
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
                let blob = blobs[usize::from(a) % blobs.len()].clone();
                rolled_back(&mut gone, Some(&blob));
                restored.keyring.put(Some(blob));
            }
            (store, live) = open(&restored);
            assert_none_back(&store, &gone);
            let served = logical(&store);
            advance(
                &mut acked,
                served.clone(),
                &mut gone,
                store.generation(),
                false,
                None,
            );
            // What the reopen serves survives a further power cut.
            drop(store);
            let next = live.after_crash(c & 2 != 0, usize::from(d));
            if lost_keyring_stores(&live, &next) {
                rolled_back(&mut gone, next.keyring.get());
            }
            (store, live) = open(&next);
            assert_none_back(&store, &gone);
            assert_eq!(logical(&store), served, "what a reopen serves survives");
            continue;
        }
        if kind >= 0xF0 {
            // Damage at rest, then a restart.
            drop(store);
            let mut damaged = live.at_rest();
            let held: Vec<Id16> = acked.records.iter().map(|r| r.0).collect();
            let touched = damage(&mut damaged, kind, a, b);
            if damaged.keyring.get().is_none() {
                rolled_back(&mut gone, None);
            }
            (store, live) = open(&damaged);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            assert_none_back(&store, &gone);
            let served = logical(&store);
            advance(
                &mut acked,
                served,
                &mut gone,
                store.generation(),
                true,
                None,
            );
            continue;
        }
        if kind >= 0xE0 {
            // A restart with every write acknowledged: exactly that state.
            drop(store);
            let next = restart(&live, c, d);
            if lost_keyring_stores(&live, &next) {
                rolled_back(&mut gone, next.keyring.get());
            }
            (store, live) = open(&next);
            assert_eq!(logical(&store), acked, "every acknowledged write survives");
            assert_none_back(&store, &gone);
            continue;
        }
        // An add's `dev_id`, for forgiving a reused one.
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
            advance(
                &mut acked,
                after,
                &mut gone,
                store.generation(),
                true,
                added.filter(|_| result.is_ok()),
            );
            assert_none_back(&store, &gone);
            continue;
        }
        let visible = live.keyring.borrow().get().cloned() != keyring_before;
        let before = acked.clone();
        match result {
            Ok(()) => assert_eq!(logical(&store), after),
            Err(_) if visible => assert_eq!(logical(&store), after),
            Err(_) => assert_eq!(logical(&store), before),
        }
        // The process died here: restart.
        drop(store);
        let mut next = restart(&live, c >> 4, d);
        if lost_keyring_stores(&live, &next) {
            rolled_back(&mut gone, next.keyring.get());
        }
        if c & 0x10 != 0 && d & 1 == 1 {
            // Damage in the middle: nothing held may come back.
            let held: Vec<Id16> = before.records.iter().map(|r| r.0).collect();
            let touched = damage(&mut next, d >> 1, a, b);
            if next.keyring.get().is_none() {
                rolled_back(&mut gone, None);
            }
            (store, live) = open(&next);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            assert_none_back(&store, &gone);
            let served = logical(&store);
            advance(
                &mut acked,
                served,
                &mut gone,
                store.generation(),
                true,
                None,
            );
            continue;
        }
        (store, live) = open(&next);
        let reached = logical(&store);
        if result.is_ok() {
            assert_eq!(reached, after, "it returned Ok");
        } else {
            assert!(
                reached == before || reached == after,
                "exactly before or after"
            );
        }
        advance(
            &mut acked,
            reached,
            &mut gone,
            store.generation(),
            true,
            added,
        );
        assert_none_back(&store, &gone);
    }
});
