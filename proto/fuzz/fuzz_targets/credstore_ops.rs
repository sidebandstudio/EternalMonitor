//! The credential store under a fuzzer-chosen sequence of operations, with
//! crashes at fuzzer-chosen steps (unsynced renames kept or lost, keyring
//! failures that may have stored anyway) and fuzzer-chosen damage to the
//! database or keyring. After every restart the invariants hold; after a
//! crash the state is exactly the one before or after the operation, and
//! exactly after once the keyring held its intent; damage never brings a
//! record back.
#![no_main]

use eternal_credstore::testing::{check_invariants, logical, Store, World};
use eternal_credstore::{NewRecord, StoreError};
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

fuzz_target!(|data: &[u8]| {
    let mut world = World::default();
    let mut now = 400 * DAY;
    for chunk in data.chunks_exact(4).take(48) {
        let (kind, a, b, c) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        let (mut store, _, live) = world.open();
        check_invariants(&store);
        let before = logical(&store);
        if kind >= 0xF0 {
            // Damage at rest, then a restart.
            drop(store);
            let mut damaged = live.after_crash(false);
            let changed = if kind & 1 == 0 {
                let Some(mut bytes) = damaged.disk.get("credentials.db").cloned() else {
                    world = damaged;
                    continue;
                };
                let at = usize::from(a) * bytes.len() / 256;
                bytes[at] ^= b | 1;
                damaged.disk.put("credentials.db", bytes);
                true
            } else {
                damaged.keyring.put(None);
                !before.records.is_empty()
            };
            let (store, _, live) = damaged.open();
            check_invariants(&store);
            let after = logical(&store);
            if changed {
                assert!(
                    after.records.is_empty(),
                    "damage never brings a record back"
                );
            }
            drop(store);
            world = live.after_crash(false);
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
        let after = {
            let (mut clean, _, _) = world.open();
            let mut clean_now = now;
            let _ = run(&mut clean, kind, a, b, &mut clean_now);
            logical(&clean)
        };
        let result = run(&mut store, kind, a, b, &mut now);
        check_invariants(&store);
        if result.is_ok() {
            assert_eq!(logical(&store), after);
        }
        drop(store);
        let intent_recorded = live.keyring.borrow().get().cloned() != keyring_before;
        let next = live.after_crash(c & 0x40 != 0);
        let (store, _, live) = next.open();
        check_invariants(&store);
        let reached = logical(&store);
        if intent_recorded {
            assert_eq!(reached, after, "the intent was recorded");
        } else {
            assert!(reached == before || reached == after);
        }
        drop(store);
        world = live.after_crash(false);
    }
});
