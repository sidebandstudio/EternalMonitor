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
//! - damage never brings back a record.
#![no_main]

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
    // The state every acknowledged write adds up to.
    let mut acked: Logical = logical(&store);
    let mut now = 400 * DAY;
    for chunk in data.chunks_exact(5).take(40) {
        let (kind, a, b, c, d) = (chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]);
        if kind >= 0xF0 {
            // Damage at rest, then a restart.
            drop(store);
            let mut damaged = live.at_rest();
            let held: Vec<Id16> = acked.records.iter().map(|r| r.0).collect();
            let touched = damage(&mut damaged, kind, a, b);
            (store, live) = open(&damaged);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            acked = logical(&store);
            continue;
        }
        if kind >= 0xE0 {
            // A restart with every write acknowledged: exactly that state.
            drop(store);
            (store, live) = open(&restart(&live, c, d));
            assert_eq!(logical(&store), acked, "every acknowledged write survives");
            continue;
        }
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
            acked = after;
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
        if c & 0x10 != 0 && d & 1 == 1 {
            // Damage in the middle: nothing held may come back.
            let held: Vec<Id16> = before.records.iter().map(|r| r.0).collect();
            let touched = damage(&mut next, d >> 1, a, b);
            (store, live) = open(&next);
            if touched {
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            acked = logical(&store);
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
        acked = reached;
    }
});
