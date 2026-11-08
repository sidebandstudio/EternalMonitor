//! The credential store under a fuzzer-chosen sequence of operations, with
//! crashes at fuzzer-chosen steps, a keyring that flushes or doesn't, and
//! fuzzer-chosen damage to the files or keyring, at rest or in the middle of
//! a rotation.
//!
//! After a crash the restart may keep or lose unsynced renames and any
//! keyring blob that was visible but not durable. What must hold:
//! - the invariants, always;
//! - a failed operation leaves memory exactly "before", or exactly "after"
//!   once its intent is visible;
//! - a restart finds exactly "before" or "after"; a rotation comes back
//!   exactly "before" if its keyring blobs were lost, exactly "after"
//!   otherwise;
//! - damage never brings back a record.
#![no_main]

use eternal_credstore::keyring::MemKeyring;
use eternal_credstore::testing::{check_invariants, logical, Store, World};
use eternal_credstore::{Disk, NewRecord, StoreError};
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
                key: Zeroizing::new([dev; 32]),
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

/// Flips one byte of a database file (`which` picks it), or drops the
/// keyring. True if anything held could be affected.
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
    // Every generation's file: the one in use is among them.
    for name in &names {
        let mut bytes = world.disk.get(name).unwrap().clone();
        let at = usize::from(a) * bytes.len() / 256;
        bytes[at] ^= b | 1;
        world.disk.put(name, bytes);
    }
    true
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, data)) = data.split_first() else {
        return;
    };
    let mut world = World {
        keyring: if mode & 1 == 0 {
            MemKeyring::new()
        } else {
            MemKeyring::flushing()
        },
        ..World::default()
    };
    let mut now = 400 * DAY;
    for chunk in data.chunks_exact(5).take(40) {
        let (kind, a, b, c, d) = (chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]);
        // Settle whatever the last restart's recovery wrote, so the keyring
        // the operation starts from is durable. (Losing a recovery's own
        // writes is covered by the crash tests.)
        let (settle, _, live) = world.open();
        drop(settle);
        world = live.after_crash(false, usize::MAX);
        let (mut store, _, live) = world.open();
        check_invariants(&store);
        let before = logical(&store);
        if kind >= 0xF0 {
            // Damage at rest, then a restart.
            drop(store);
            let mut damaged = live.after_crash(false, usize::MAX);
            let held = !before.records.is_empty();
            let touched = damage(&mut damaged, kind, a, b);
            let (store, _, live) = damaged.open();
            check_invariants(&store);
            if touched && held {
                assert!(
                    logical(&store).records.is_empty(),
                    "damage never brings a record back"
                );
            }
            drop(store);
            world = live.after_crash(false, usize::MAX);
            continue;
        }
        let keyring_before = live.keyring.borrow().get().cloned();
        let crash = c & 0x80 != 0;
        if crash {
            live.steps
                .crash_at
                .set(Some(live.steps.count.get() + usize::from(c & 0x0F)));
            live.steps.ambiguous.set(c & 0x20 != 0);
        }
        // The same operation, uncrashed, from the same state.
        let (after, rotates) = {
            let (mut clean, _, clean_live) = world.open();
            let mut clean_now = now;
            let _ = run(&mut clean, kind, a, b, &mut clean_now);
            let rotates = clean_live.keyring.borrow().get().cloned() != keyring_before;
            (logical(&clean), rotates)
        };
        let result = run(&mut store, kind, a, b, &mut now);
        check_invariants(&store);
        let visible = live.keyring.borrow().get().cloned() != keyring_before;
        match result {
            Ok(()) => assert_eq!(logical(&store), after),
            // Memory is exactly before, or exactly after once the intent
            // is visible.
            Err(_) if visible => assert_eq!(logical(&store), after),
            Err(_) => assert_eq!(logical(&store), before),
        }
        drop(store);
        let mut next = live.after_crash(c & 0x40 != 0, usize::from(d));
        let kept_old_keyring = next.keyring.get().cloned() == keyring_before;
        if crash && c & 0x10 != 0 && rotates {
            // Damage in the middle of a rotation: nothing may come back.
            let touched = damage(&mut next, d >> 1, a, b);
            let (store, _, live) = next.open();
            check_invariants(&store);
            if touched {
                let held: Vec<Id16> = before.records.iter().map(|r| r.0).collect();
                for r in logical(&store).records {
                    assert!(!held.contains(&r.0), "damage never brings a record back");
                }
            }
            drop(store);
            world = live.after_crash(false, usize::MAX);
            continue;
        }
        let (store, _, live) = next.open();
        check_invariants(&store);
        let reached = logical(&store);
        if !rotates {
            assert!(reached == before || reached == after);
        } else if kept_old_keyring {
            assert_eq!(reached, before, "the rotation's keyring blobs were lost");
        } else {
            assert_eq!(reached, after, "a keyring blob of the rotation survived");
        }
        drop(store);
        world = live.after_crash(false, usize::MAX);
    }
});
