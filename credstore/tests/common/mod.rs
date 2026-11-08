//! Shared test fixtures.

#![allow(dead_code)]

pub use eternal_credstore::testing::*;
use eternal_credstore::NewRecord;
use eternal_wire::v3::{Id16, Zeroizing};

pub fn id(n: u8) -> Id16 {
    [n; 16]
}

pub fn new_record(dev: u8, slot: u8, replaces: Option<u8>) -> NewRecord {
    NewRecord {
        dev_id: id(dev),
        slot_id: id(slot),
        key: Zeroizing::new([dev; 32]),
        replaces: replaces.map(id),
        device_name: format!("iPad {dev}"),
    }
}

pub const DAY: u64 = 24 * 60 * 60;
/// "Now" in the tests.
pub const NOW: u64 = 100 * DAY;

/// Slot 1: active 0x11, pending 0x12 (replaces 0x11), pending 0x13 (no
/// replaces: conflicts). Slot 2: pending 0x21. Slot 3: pending 0x31, 8
/// days old. Slot 4: 0x41 removed earlier, so tombstoned.
pub fn base_world() -> World {
    let (mut store, _, live) = World::default().open();
    store.add_pending(new_record(0x11, 1, None), NOW).unwrap();
    store.finalize(&id(0x11)).unwrap();
    store.persist_timestamp(&id(0x11), 5_000).unwrap();
    store
        .add_pending(new_record(0x12, 1, Some(0x11)), NOW)
        .unwrap();
    store.add_pending(new_record(0x13, 1, None), NOW).unwrap();
    store.add_pending(new_record(0x21, 2, None), NOW).unwrap();
    store
        .add_pending(new_record(0x31, 3, None), NOW - 8 * DAY)
        .unwrap();
    store.add_pending(new_record(0x41, 4, None), NOW).unwrap();
    store.remove(&[id(0x41)]).unwrap();
    drop(store);
    live.after_crash(false)
}
