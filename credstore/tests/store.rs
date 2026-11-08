//! The store's operations, caps, snapshot and restore semantics.

mod common;

use common::*;
use eternal_credstore::format::{parse_db, parse_keyring};
use eternal_credstore::store::{DB_FILE, DB_TMP};
use eternal_credstore::{Finalize, NewRecord, Recovery, StoreCredentials, StoreError};
use eternal_wire::v3::host::HostCredentials;
use eternal_wire::v3::wire::{Accept, RecordState};
use eternal_wire::v3::Zeroizing;

fn state_of(store: &Store, dev: u8) -> Option<RecordState> {
    store
        .records()
        .into_iter()
        .find(|r| r.dev_id == id(dev))
        .map(|r| r.state)
}

fn is_tombstoned(store: &Store, dev: u8) -> bool {
    store
        .tombstones()
        .iter()
        .any(|(_, ids)| ids.contains(&id(dev)))
}

#[test]
fn a_new_store_is_empty_with_a_fresh_keyring() {
    let (store, report, live) = World::default().open();
    assert_eq!(report.recovery, vec![Recovery::FreshKeyring]);
    assert!(report.discarded.is_empty());
    assert!(store.records().is_empty());
    assert_eq!(store.generation(), 1);
    drop(store);
    let (store, report, _) = live.after_crash(false).open();
    assert!(report.recovery.is_empty(), "nothing to do: {report:?}");
    assert_eq!(store.generation(), 1);
}

#[test]
fn caps_are_enforced_at_pairing() {
    let (mut store, _, _) = World::default().open();
    // 3 pending per slot.
    for dev in 1..=3 {
        store.add_pending(new_record(dev, 1, None), NOW).unwrap();
    }
    assert!(matches!(
        store.add_pending(new_record(4, 1, None), NOW),
        Err(StoreError::TooMany)
    ));
    // Plus 1 active.
    store.finalize(&id(1)).unwrap();
    store.add_pending(new_record(4, 1, Some(1)), NOW).unwrap();
    assert!(matches!(
        store.add_pending(new_record(5, 1, Some(1)), NOW),
        Err(StoreError::TooMany)
    ));
    // 64 in all; existing records are never changed to make room.
    for dev in 5..=64u8 {
        store.add_pending(new_record(dev, dev, None), NOW).unwrap();
    }
    assert_eq!(store.records().len(), 64);
    let before = logical(&store);
    assert!(matches!(
        store.add_pending(new_record(65, 65, None), NOW),
        Err(StoreError::TooMany)
    ));
    assert_eq!(logical(&store), before);
    check_invariants(&store);
}

#[test]
fn duplicates_retired_ids_and_bad_replaces_are_refused() {
    let (mut store, _, _) = World::default().open();
    store.add_pending(new_record(1, 1, None), NOW).unwrap();
    assert!(matches!(
        store.add_pending(new_record(1, 2, None), NOW),
        Err(StoreError::Duplicate)
    ));
    store.remove(&[id(1)]).unwrap();
    assert!(matches!(
        store.add_pending(new_record(1, 1, None), NOW),
        Err(StoreError::Duplicate)
    ));
    // replaces must be the slot's active record.
    assert!(matches!(
        store.add_pending(new_record(2, 1, Some(9)), NOW),
        Err(StoreError::InvalidReplaces)
    ));
    store.add_pending(new_record(3, 1, None), NOW).unwrap();
    assert!(matches!(
        store.add_pending(new_record(4, 1, Some(3)), NOW),
        Err(StoreError::InvalidReplaces),
    ));
    assert!(matches!(store.finalize(&id(9)), Err(StoreError::Unknown)));
    assert!(matches!(
        store.persist_timestamp(&id(9), 1),
        Err(StoreError::Unknown)
    ));
}

#[test]
fn finalize_promotes_replaces_and_retires_conflicts() {
    let (mut store, _, _) = base_world().open();
    let g = store.generation();
    // A pending record with no replaces in a slot with no active one: a
    // plain write, no rotation.
    assert!(!store.would_conflict(&id(0x21)));
    assert_eq!(
        store.finalize(&id(0x21)).unwrap(),
        Finalize::Promoted { retired: None }
    );
    assert_eq!(store.generation(), g);
    assert_eq!(store.finalize(&id(0x21)).unwrap(), Finalize::AlreadyActive);
    // With no replaces in a slot that has an active record: conflict,
    // retired through a rotation.
    assert!(store.would_conflict(&id(0x13)));
    assert_eq!(store.finalize(&id(0x13)).unwrap(), Finalize::Conflict);
    assert_eq!(store.generation(), g + 1);
    assert_eq!(state_of(&store, 0x13), None);
    assert!(is_tombstoned(&store, 0x13));
    // replaces the active record: the old one retired, the new one active.
    assert!(!store.would_conflict(&id(0x12)));
    assert_eq!(
        store.finalize(&id(0x12)).unwrap(),
        Finalize::Promoted {
            retired: Some(id(0x11))
        }
    );
    assert_eq!(store.generation(), g + 2);
    assert_eq!(state_of(&store, 0x12), Some(RecordState::Active));
    assert_eq!(state_of(&store, 0x11), None);
    assert!(is_tombstoned(&store, 0x11));
    assert!(store.records().iter().all(|r| r.replaces.is_none()));
    check_invariants(&store);
    // An unknown dev_id would conflict.
    assert!(store.would_conflict(&id(0x99)));
}

#[test]
fn a_pending_record_whose_replaced_record_is_gone_conflicts() {
    let (mut store, _, _) = base_world().open();
    store.remove(&[id(0x11)]).unwrap();
    assert!(store.would_conflict(&id(0x12)));
    assert_eq!(store.finalize(&id(0x12)).unwrap(), Finalize::Conflict);
    // With the slot empty, the one with no replaces can become active.
    assert_eq!(
        store.finalize(&id(0x13)).unwrap(),
        Finalize::Promoted { retired: None }
    );
    check_invariants(&store);
}

#[test]
fn expire_retires_pending_records_at_7_days() {
    let (mut store, _, _) = World::default().open();
    store.add_pending(new_record(1, 1, None), NOW).unwrap();
    store.finalize(&id(1)).unwrap();
    store.add_pending(new_record(2, 2, None), NOW).unwrap();
    let g = store.generation();
    assert!(store.expire(NOW + 7 * DAY - 1).unwrap().is_empty());
    assert_eq!(store.generation(), g, "nothing expired, no rotation");
    assert_eq!(store.expire(NOW + 7 * DAY).unwrap(), vec![id(2)]);
    assert!(is_tombstoned(&store, 2));
    assert_eq!(
        state_of(&store, 1),
        Some(RecordState::Active),
        "active records never expire"
    );
}

#[test]
fn committed_timestamps_only_grow_and_survive_a_restart() {
    let (mut store, _, live) = World::default().open();
    store.add_pending(new_record(1, 1, None), NOW).unwrap();
    store.persist_timestamp(&id(1), 500).unwrap();
    store.persist_timestamp(&id(1), 400).unwrap();
    let snapshot = store.snapshot();
    assert_eq!(snapshot.last_committed_timestamp(&id(1)), 500);
    assert_eq!(snapshot.last_committed_timestamp(&id(2)), 0);
    // Through a rotation too.
    store.add_pending(new_record(2, 2, None), NOW).unwrap();
    store.remove(&[id(2)]).unwrap();
    drop(store);
    let (store, _, _) = live.after_crash(false).open();
    assert_eq!(store.snapshot().last_committed_timestamp(&id(1)), 500);
    assert_eq!(*store.snapshot().key(&id(1)).unwrap(), [1; 32]);
}

#[test]
fn the_snapshot_answers_the_session_table() {
    let (mut store, _, _) = base_world().open();
    let old = store.snapshot();
    let creds = StoreCredentials {
        snapshot: store.snapshot(),
        accept: Accept {
            host_name: "PC".into(),
            ..Default::default()
        },
    };
    assert_eq!(*creds.key(&id(0x11)).unwrap(), [0x11; 32]);
    assert_eq!(creds.last_committed_timestamp(&id(0x11)), 5_000);
    assert!(creds.key(&id(0x41)).is_none(), "removed");
    let status = creds.slot_status(&id(0x12));
    assert_eq!(
        status.held,
        vec![
            (id(0x11), RecordState::Active),
            (id(0x12), RecordState::Pending),
            (id(0x13), RecordState::Pending)
        ]
    );
    assert!(status.tombstones.is_empty());
    // A retired dev_id still gets its old slot's status.
    let removed = creds.slot_status(&id(0x41));
    assert!(removed.held.is_empty());
    assert_eq!(removed.tombstones, vec![id(0x41)]);
    let accept = creds.accept_template(&id(0x11));
    assert_eq!(accept.host_name, "PC");
    assert_eq!(accept.slot_status, status);
    assert_eq!(creds.slot_status(&id(0x99)), Default::default());
    // A snapshot is immutable: later operations publish a new one.
    store.remove(&[id(0x11)]).unwrap();
    assert!(old.key(&id(0x11)).is_some());
    assert!(store.snapshot().key(&id(0x11)).is_none());
}

#[test]
fn tombstones_keep_the_last_8_per_slot() {
    let (mut store, _, live) = World::default().open();
    for dev in 1..=10u8 {
        store.add_pending(new_record(dev, 1, None), NOW).unwrap();
        store.remove(&[id(dev)]).unwrap();
    }
    let tombstones = store.tombstones();
    assert_eq!(tombstones, vec![(id(1), (3..=10).map(id).collect())]);
    drop(store);
    let (store, _, _) = live.after_crash(false).open();
    assert_eq!(store.tombstones(), tombstones);
}

#[test]
fn tombstones_forget_the_oldest_empty_slot_past_1024() {
    let (mut store, _, _) = World::default().open();
    // A slot with a record stays, however old its tombstones.
    store.add_pending(new_record(0xF0, 0, None), NOW).unwrap();
    store.add_pending(new_record(0xF1, 0, None), NOW).unwrap();
    store.remove(&[id(0xF1)]).unwrap();
    let slot = |n: u16| {
        let mut s = [0u8; 16];
        s[..2].copy_from_slice(&n.to_le_bytes());
        s[2] = 1;
        s
    };
    for n in 0..1025u16 {
        let mut dev = slot(n);
        dev[3] = 1;
        store
            .add_pending(
                NewRecord {
                    dev_id: dev,
                    slot_id: slot(n),
                    key: Zeroizing::new([1; 32]),
                    replaces: None,
                    device_name: String::new(),
                },
                NOW,
            )
            .unwrap();
        store.remove(&[dev]).unwrap();
    }
    let tombstones = store.tombstones();
    assert_eq!(tombstones.len(), 1024);
    assert!(
        tombstones.iter().any(|(s, _)| *s == id(0)),
        "slot 0 holds a record"
    );
    // 1 + 1025 slots: the two oldest empty ones go.
    assert!(!tombstones
        .iter()
        .any(|(s, _)| *s == slot(0) || *s == slot(1)));
    assert!(tombstones.iter().any(|(s, _)| *s == slot(2)));
}

#[test]
fn device_names_are_cut_to_64_bytes_on_a_character_boundary() {
    let (mut store, _, _) = World::default().open();
    let mut record = new_record(1, 1, None);
    record.device_name = format!("{}é", "a".repeat(63));
    store.add_pending(record, NOW).unwrap();
    assert_eq!(store.records()[0].device_name, "a".repeat(63));
}

#[test]
fn a_failed_plain_write_changes_nothing() {
    let (mut store, _, live) = base_world().open();
    let before = logical(&store);
    live.steps.crash_at.set(Some(live.steps.count.get()));
    assert!(store.add_pending(new_record(0x51, 5, None), NOW).is_err());
    assert!(store.persist_timestamp(&id(0x11), 9_999).is_err());
    assert!(store.finalize(&id(0x21)).is_err());
    assert_eq!(logical(&store), before);
    // Not a rotation: the store stays usable once writes work again.
    live.steps.crash_at.set(None);
    store.add_pending(new_record(0x51, 5, None), NOW).unwrap();
}

#[test]
fn the_database_holds_no_plaintext_key() {
    let (mut store, _, live) = World::default().open();
    store.add_pending(new_record(0xAB, 1, None), NOW).unwrap();
    drop(store);
    let disk = live.disk.borrow();
    let bytes = disk.get(DB_FILE).unwrap();
    assert!(!bytes.windows(32).any(|w| w == [0xAB; 32]));
    let blob = live.keyring.borrow().get().cloned().unwrap();
    assert!(parse_keyring(&blob).is_ok());
    assert!(disk.get(DB_TMP).is_none());
}

// Restore semantics (§4.3): a database the current generation can't open
// is discarded; its records are listed as needing pairing again and
// tombstoned, and tombstones are never lost.

#[test]
fn a_database_restored_from_before_an_invalidation_is_discarded() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x11)]).unwrap();
    drop(store);
    let mut restored = live.after_crash(false);
    restored.disk = base.disk.clone();
    let (store, report, _) = restored.open();
    assert_eq!(report.recovery, vec![Recovery::Discarded]);
    assert_eq!(report.discarded.len(), 5);
    assert!(store.records().is_empty());
    assert!(is_tombstoned(&store, 0x11));
    assert!(is_tombstoned(&store, 0x41), "older tombstones kept");
    check_invariants(&store);
}

#[test]
fn a_restored_database_from_the_same_generation_only_loses_additions() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.add_pending(new_record(0x51, 5, None), NOW).unwrap();
    store.persist_timestamp(&id(0x11), 9_000).unwrap();
    drop(store);
    let mut restored = live.after_crash(false);
    restored.disk = base.disk.clone();
    let (store, report, _) = restored.open();
    assert!(report.recovery.is_empty());
    assert_eq!(logical(&store), logical(&base.open().0));
    // Losing a committed timestamp leaves it older, never newer.
    assert_eq!(store.snapshot().last_committed_timestamp(&id(0x11)), 5_000);
}

#[test]
fn a_keyring_rolled_back_past_a_rotation_discards() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.expire(NOW).unwrap();
    drop(store);
    let mut rolled_back = live.after_crash(false);
    rolled_back.keyring = base.keyring.clone();
    let (store, report, _) = rolled_back.open();
    assert_eq!(report.recovery, vec![Recovery::Discarded]);
    assert!(store.records().is_empty());
    assert!(is_tombstoned(&store, 0x31));
}

#[test]
fn a_corrupt_database_is_discarded() {
    let base = base_world();
    // A flipped MAC byte: still listed and tombstoned.
    let mut bad_mac = base.clone();
    let mut bytes = bad_mac.disk.get(DB_FILE).unwrap().clone();
    *bytes.last_mut().unwrap() ^= 1;
    assert!(parse_db(&bytes).is_ok());
    bad_mac.disk.put(DB_FILE, bytes.clone());
    let (store, report, _) = bad_mac.open();
    assert_eq!(report.recovery, vec![Recovery::Discarded]);
    assert_eq!(report.discarded.len(), 5);
    assert!(store.records().is_empty());
    // Unparseable: nothing to list, still discarded.
    let mut garbage = base.clone();
    garbage.disk.put(DB_FILE, bytes[..40].to_vec());
    let (store, report, live) = garbage.open();
    assert_eq!(report.recovery, vec![Recovery::Discarded]);
    assert!(report.discarded.is_empty());
    assert!(store.records().is_empty());
    drop(store);
    // The next start is clean.
    let (_, report, _) = live.after_crash(false).open();
    assert!(report.recovery.is_empty());
}

#[test]
fn a_lost_database_with_a_keyring_is_an_empty_store() {
    let mut world = base_world();
    world.disk = Default::default();
    let (store, report, _) = world.open();
    assert!(report.recovery.is_empty());
    assert!(store.records().is_empty());
}

#[test]
fn a_stray_temporary_file_is_removed() {
    let mut world = base_world();
    world.disk.put(DB_TMP, b"partial".to_vec());
    let (store, _, live) = world.open();
    assert_eq!(store.records().len(), 5);
    assert!(live.disk.borrow().get(DB_TMP).is_none());
}
