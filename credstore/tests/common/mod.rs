//! Shared test fixtures.

#![allow(dead_code)]

pub use eternal_credstore::keyring::MemKeyring;
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
        key: Box::new(Zeroizing::new([dev; 32])),
        replaces: replaces.map(id),
        device_name: format!("iPad {dev}"),
    }
}

pub const DAY: u64 = 24 * 60 * 60;
/// "Now" in the tests.
pub const NOW: u64 = 100 * DAY;

/// Slot 1: active 0x11, pending 0x12 (replaces 0x11), pending 0x13 (no
/// replaces: conflicts). Slot 2: pending 0x21. Slot 3: pending 0x31, 8
/// days old. Slot 4: 0x41 removed earlier, so tombstoned. At rest, with
/// only the current generation's file, and a keyring whose stores may be
/// lost on a crash (as on macOS).
pub fn base_world() -> World {
    base_world_with(MemKeyring::new())
}

/// [`base_world`] on `keyring`: [`MemKeyring::flushing`] behaves as on
/// Windows.
pub fn base_world_with(keyring: MemKeyring) -> World {
    let empty = World {
        keyring,
        ..World::default()
    };
    let (mut store, _, live) = empty.open();
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
    // One more open removes the previous generation's file.
    let (store, _, live) = live.at_rest().open();
    drop(store);
    live.at_rest()
}

/// The database files a world holds, by generation.
pub fn db_generations(world: &World) -> Vec<u64> {
    use eternal_credstore::Disk;
    let mut gens: Vec<u64> = world
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
    gens.sort_unstable();
    gens
}
