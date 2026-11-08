//! The store's operations, caps, snapshot and restore semantics.

mod common;

use common::*;
use eternal_credstore::format::{
    encode_db, encode_keyring, DbFile, GenerationKeys, KeyringState, MAX_GENERATION,
};
use eternal_credstore::format::{parse_db, parse_keyring};
use eternal_credstore::store::{db_file, DB_TMP};
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
    let (store, report, _) = live.at_rest().open();
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
    let (store, _, _) = live.at_rest().open();
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
    let (store, _, _) = live.at_rest().open();
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
                    key: Box::new(Zeroizing::new([1; 32])),
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
    let name = db_file(store.generation());
    drop(store);
    let disk = live.disk.borrow();
    let bytes = disk.get(&name).unwrap();
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
    // The next boot confirms the rotation and moves the database to its key.
    let (store, _, live) = live.at_rest().open();
    assert_eq!(store.base_generation(), store.generation());
    drop(store);
    let mut restored = live.at_rest();
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
    let mut restored = live.at_rest();
    restored.disk = base.disk.clone();
    let (store, report, _) = restored.open();
    assert!(report.recovery.is_empty());
    assert_eq!(logical(&store), logical(&base.open().0));
    // Losing a committed timestamp leaves it older, never newer.
    assert_eq!(store.snapshot().last_committed_timestamp(&id(0x11)), 5_000);
}

#[test]
fn a_keyring_rolled_back_past_an_observed_rotation_discards() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.expire(NOW).unwrap();
    drop(store);
    // A later open sees the new generation and removes the old file.
    let (store, _, live) = live.at_rest().open();
    drop(store);
    let mut rolled_back = live.at_rest();
    assert_eq!(db_generations(&rolled_back).len(), 1);
    rolled_back.keyring = base.keyring.clone();
    let (store, report, _) = rolled_back.open();
    assert_eq!(report.recovery, vec![Recovery::Discarded]);
    assert!(store.records().is_empty());
    assert!(is_tombstoned(&store, 0x31));
}

/// A rotation whose keyring stores are all lost keeps its data (the
/// database is under the confirmed key), and the open rotates again so the
/// invalidation still gets a new key.
#[test]
fn a_lost_rotation_keyring_keeps_the_rotation_and_rotates_again() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.expire(NOW).unwrap();
    let after = logical(&store);
    drop(store);
    let mut lost = live.at_rest();
    assert_eq!(db_generations(&lost).len(), 1, "one file");
    lost.keyring = base.keyring.clone();
    let (store, report, _) = lost.open();
    assert_eq!(report.recovery, vec![Recovery::RotatedAgain]);
    assert_eq!(logical(&store), after);
    assert!(is_tombstoned(&store, 0x31));
}

/// macOS: the database moves to a rotation's key only at the first open in
/// a later boot; a reopen in the same boot changes nothing. A keyring that
/// flushes moves it at once.
#[test]
fn promotion_waits_for_a_later_boot_without_a_flush() {
    let base = base_world();
    let g = db_generations(&base);
    assert_eq!(g.len(), 1);
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    assert_eq!(store.base_generation(), g[0]);
    assert_eq!(store.generation(), g[0] + 1);
    drop(store);
    for _ in 0..3 {
        let world = live.same_boot();
        assert_eq!(db_generations(&world), g, "same boot: nothing moves");
        let (store, _, _) = world.open();
        assert_eq!(store.base_generation(), g[0]);
    }
    let (store, _, live) = live.at_rest().open();
    assert_eq!(
        store.base_generation(),
        g[0] + 1,
        "a later boot confirms it"
    );
    drop(store);
    assert_eq!(db_generations(&live.at_rest()), vec![g[0] + 1]);

    let base = base_world_with(MemKeyring::flushing());
    let g = db_generations(&base);
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    assert_eq!(
        store.base_generation(),
        store.generation(),
        "flushed: at once"
    );
    drop(store);
    assert_eq!(db_generations(&live.same_boot()), vec![g[0] + 1]);
}

/// Each keyring blob records the boot that stored it. A blob from this boot
/// confirms nothing, however often the store reopens; the first open in a
/// later boot finds one from an earlier boot, which survived it.
#[test]
fn the_boot_id_in_the_blob_tells_a_later_boot_from_a_reopen() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    let rotated = store.generation();
    drop(store);
    let boot = live.keyring.borrow().boot().to_le_bytes().to_vec();
    let blob = live.keyring.borrow().get().cloned().unwrap();
    assert_eq!(parse_keyring(&blob).unwrap().boot, boot);
    for _ in 0..3 {
        let (store, _, _) = live.same_boot().open();
        assert!(store.base_generation() < rotated, "same boot");
    }
    let (store, _, _) = live.at_rest().open();
    assert_eq!(store.base_generation(), rotated, "a later boot");
}

/// A keyring blob restored on its own (an older one, from an earlier boot)
/// can't confirm the current one: whatever is then lost to a power cut,
/// every acknowledged write and untouched record is still there.
#[test]
fn a_restored_older_keyring_blob_confirms_nothing_unsafe() {
    let base = base_world();
    let old_blob = base.keyring.get().cloned();
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    store.add_pending(new_record(0x51, 5, None), NOW).unwrap();
    store.persist_timestamp(&id(0x11), 9_000).unwrap();
    let after = logical(&store);
    drop(store);
    let mut restored = live.same_boot();
    restored.keyring.put(old_blob);
    let (store, _, live) = restored.open();
    assert_eq!(logical(&store), after);
    drop(store);
    for choice in 0..live.keyring_choices() {
        let (store, _, _) = live.after_crash(false, choice).open();
        assert_eq!(logical(&store), after, "keyring choice {choice}");
    }
}

/// With this boot's id unreadable the store still opens and serves every
/// write, and confirms nothing: the database stays under its key.
#[test]
fn an_unreadable_boot_id_confirms_nothing() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    let after = logical(&store);
    let base_g = store.base_generation();
    drop(store);
    let mut world = live.at_rest();
    world.keyring.set_boot_unreadable(true);
    let (mut store, _, live) = world.open();
    assert_eq!(logical(&store), after);
    assert_eq!(store.base_generation(), base_g, "nothing promoted");
    store.persist_timestamp(&id(0x11), 9_000).unwrap();
    let after = logical(&store);
    drop(store);
    // A power cut that loses every keyring store since the last confirmed
    // one: still every write.
    for choice in 0..live.keyring_choices() {
        let mut world = live.after_crash(false, choice);
        world.keyring.set_boot_unreadable(true);
        let (store, _, _) = world.open();
        assert_eq!(logical(&store), after, "keyring choice {choice}");
    }
}

/// A database file restored from before a rotation (its head predates the
/// committed generation) is never used: in the same boot, or after a
/// reboot before the first open, the removed record doesn't come back, the
/// discard is reported, and every listed record is tombstoned.
#[test]
fn a_database_restored_before_promotion_is_discarded() {
    for reboot in [false, true] {
        let base = base_world();
        let (mut store, _, live) = base.open();
        let g = store.base_generation();
        let backup = live.disk.borrow().get(&db_file(g)).unwrap().clone();
        store.remove(&[id(0x11)]).unwrap();
        assert!(store.snapshot().key(&id(0x11)).is_none());
        drop(store);
        let mut restored = if reboot {
            live.at_rest()
        } else {
            live.same_boot()
        };
        restored.disk.put(&db_file(g), backup.clone());
        let (store, report, live) = restored.open();
        assert!(
            report.recovery.contains(&Recovery::Discarded),
            "reboot {reboot}"
        );
        assert!(store.snapshot().key(&id(0x11)).is_none(), "reboot {reboot}");
        assert!(is_tombstoned(&store, 0x11), "reboot {reboot}");
        assert!(store.records().is_empty());
        drop(store);
        // And never later either.
        let (store, _, _) = live.at_rest().open();
        assert!(store.snapshot().key(&id(0x11)).is_none());
    }
}

/// A startup discard leaves only its own file: nothing else could be opened
/// by any keyring blob that may survive, so no later rollback can open an
/// old copy.
#[test]
fn a_discard_leaves_only_its_own_file() {
    let mut world = base_world();
    let g = *db_generations(&world).last().unwrap();
    let mut bytes = world.disk.get(&db_file(g)).unwrap().clone();
    *bytes.last_mut().unwrap() ^= 1;
    world.disk.put(&db_file(g), bytes.clone());
    world.disk.put(&db_file(g + 7), bytes);
    let (store, report, live) = world.open();
    assert!(report.recovery.contains(&Recovery::Discarded));
    drop(store);
    assert_eq!(db_generations(&live.same_boot()), vec![g]);
}

/// A promotion interrupted after its file and keyring store, before the old
/// file goes, again and again: files never pile up.
#[test]
fn interrupted_promotions_never_pile_up_files() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    drop(store);
    let mut world = live.at_rest();
    for dev in 0x61..0x6Bu8 {
        // Kill the promoting open at each step; carry on from the latest kill
        // that left the most files (after the keyring store, before the old
        // file goes).
        let steps = {
            let (live, opened) = world.open_live(7, None, false);
            opened.unwrap();
            live.steps.count.get()
        };
        let mut worst = None;
        for k in 0..steps {
            let (live, opened) = world.open_live(7, Some(k), false);
            assert!(opened.is_err());
            let left = live.same_boot();
            let n = db_generations(&left).len();
            assert!(n <= 2, "killed at {k}: {n} files");
            if worst.as_ref().is_none_or(|(m, _)| n >= *m) {
                worst = Some((n, left));
            }
        }
        let (_, left) = worst.unwrap();
        let (mut store, _, live) = left.open();
        store.add_pending(new_record(dev, dev, None), NOW).unwrap();
        store.remove(&[id(dev)]).unwrap();
        drop(store);
        world = live.at_rest();
        assert!(db_generations(&world).len() <= 2);
    }
}

/// Unconfirmed rotations add no files, and an open reads at most two.
#[test]
fn files_stay_bounded_however_many_rotations_wait() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    for dev in 0..100u8 {
        let dev = 0x60 + dev % 0x40;
        store.add_pending(new_record(dev, 9, None), NOW).ok();
        store.remove(&[id(dev)]).unwrap();
    }
    drop(store);
    for world in [live.same_boot(), live.at_rest()] {
        assert!(db_generations(&world).len() <= 2);
        let (live, opened) = world.open_live(1, None, false);
        opened.unwrap();
        assert!(
            live.steps.db_reads.get() <= 2,
            "{}",
            live.steps.db_reads.get()
        );
    }
}

#[test]
fn files_that_aren_t_a_generation_are_ignored() {
    let mut world = base_world();
    for name in [
        "credentials-01.db",
        "credentials-x.db",
        "credentials-.db",
        "other",
    ] {
        world.disk.put(name, b"junk".to_vec());
    }
    world.disk.put(&db_file(MAX_GENERATION), b"junk".to_vec());
    let (store, report, live) = world.open();
    assert!(report.recovery.is_empty());
    assert_eq!(store.records().len(), 5);
    drop(store);
    assert!(live.disk.borrow().get("other").is_some(), "left alone");
}

#[test]
fn an_unreadable_keyring_is_never_replaced() {
    let base = base_world();
    let blob = base.keyring.get().unwrap().clone();
    let mut newer = blob.clone();
    newer[8] = 2; // a later version
    for bad in [newer, b"garbage".to_vec(), Vec::new()] {
        let mut world = base.clone();
        world.keyring.put(Some(bad.clone()));
        let (live, opened) = world.open_live(1, None, false);
        assert!(matches!(opened, Err(StoreError::KeyringUnreadable(_))));
        assert_eq!(live.keyring.borrow().get(), Some(&bad), "untouched");
        assert_eq!(db_generations(&live.at_rest()), db_generations(&base));
    }
    // A keyring that can't be read at all: the same.
    let (live, opened) = base.open_live_with(1, |steps| steps.fail_load.set(true));
    assert!(matches!(opened, Err(StoreError::Io(_))));
    assert_eq!(live.keyring.borrow().get(), base.keyring.get());
}

/// A failed step 1 whose outcome can't be read back: the records are
/// invalid in memory, writes are refused, and a reopen settles it either
/// way, never touching another record.
#[test]
fn an_unknown_step_1_outcome_fails_closed() {
    let base = base_world();
    let before = logical(&base.open().0);
    for ambiguous in [false, true] {
        let (mut store, _, live) = base.open();
        live.steps.crash_at.set(Some(live.steps.count.get()));
        live.steps.ambiguous.set(ambiguous);
        live.steps.fail_load.set(true);
        assert!(store.remove(&[id(0x21)]).is_err());
        assert!(
            store.snapshot().key(&id(0x21)).is_none(),
            "invalid in memory"
        );
        assert!(matches!(
            store.add_pending(new_record(0x51, 5, None), NOW),
            Err(StoreError::NeedsReopen)
        ));
        drop(store);
        for choice in 0..live.keyring_choices() {
            let (store, _, _) = live.after_crash(false, choice).open();
            let reached = logical(&store);
            let mut after = before.clone();
            after.records.retain(|r| r.0 != id(0x21));
            if live.after_crash(false, choice).keyring.get() == base.keyring.get() {
                assert_eq!(reached, before);
            } else {
                assert_eq!(reached.records, after.records);
            }
        }
    }
}

/// A failed step 1 that leaves a blob which doesn't parse: unknown, so the
/// records are invalid in memory and writes are refused.
#[test]
fn an_unparseable_read_back_is_unknown() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    live.steps.crash_at.set(Some(live.steps.count.get()));
    live.steps.garble.set(true);
    assert!(store.remove(&[id(0x21)]).is_err());
    assert!(
        store.snapshot().key(&id(0x21)).is_none(),
        "invalid in memory"
    );
    assert!(matches!(
        store.persist_timestamp(&id(0x11), 6_000),
        Err(StoreError::NeedsReopen)
    ));
}

fn world_at(generation: u64) -> World {
    let w = [9u8; 32];
    let mut world = World::default();
    world.keyring.put(Some(
        encode_keyring(&KeyringState {
            committed: (generation, eternal_wire::v3::Zeroizing::new(w)),
            base: (generation, eternal_wire::v3::Zeroizing::new(w)),
            boot: Vec::new(),
            staged: None,
        })
        .to_vec(),
    ));
    let db = DbFile {
        generation,
        head: generation,
        records: Vec::new(),
        tombstones: Vec::new(),
    };
    world.disk.put(
        &db_file(generation),
        encode_db(&db, &GenerationKeys::new(&w, generation)),
    );
    world
}

#[test]
fn a_rotation_past_the_generation_bound_is_refused_before_any_change() {
    let world = world_at(MAX_GENERATION - 1);
    let (mut store, report, live) = world.open();
    assert!(report.recovery.is_empty());
    store.add_pending(new_record(1, 1, None), NOW).unwrap();
    let before = logical(&store);
    let keyring = live.keyring.borrow().get().cloned();
    let files = db_generations(&live.at_rest());
    assert!(matches!(store.remove(&[id(1)]), Err(StoreError::Exhausted)));
    assert!(matches!(
        store.expire(NOW + 30 * DAY),
        Err(StoreError::Exhausted)
    ));
    assert_eq!(logical(&store), before);
    assert_eq!(live.keyring.borrow().get().cloned(), keyring);
    assert_eq!(db_generations(&live.at_rest()), files);
    // Not stuck: plain writes still work.
    store.persist_timestamp(&id(1), 5).unwrap();
}

#[test]
fn a_fresh_keyring_is_seeded_only_with_room_to_rotate() {
    for (on_disk, fresh) in [
        (MAX_GENERATION - 3, MAX_GENERATION - 2),
        (MAX_GENERATION - 2, 1),
        (MAX_GENERATION - 1, 1),
        (5, 6),
    ] {
        let mut world = world_at(on_disk);
        world.keyring.put(None);
        let (store, report, _) = world.open();
        assert_eq!(report.recovery[0], Recovery::FreshKeyring);
        // The startup discard rotates once from the fresh generation.
        assert_eq!(store.generation(), fresh + 1, "{on_disk}");
    }
}

#[test]
fn a_corrupt_database_is_discarded() {
    let base = base_world();
    // A flipped MAC byte: still listed and tombstoned.
    let name = db_file(*db_generations(&base).last().unwrap());
    let mut bad_mac = base.clone();
    let mut bytes = bad_mac.disk.get(&name).unwrap().clone();
    *bytes.last_mut().unwrap() ^= 1;
    assert!(parse_db(&bytes).is_ok());
    bad_mac.disk.put(&name, bytes.clone());
    let (store, report, _) = bad_mac.open();
    assert_eq!(report.recovery, vec![Recovery::Discarded]);
    assert_eq!(report.discarded.len(), 5);
    assert!(store.records().is_empty());
    // Unparseable: nothing to list, still discarded.
    let mut garbage = base.clone();
    garbage.disk.put(&name, bytes[..40].to_vec());
    let (store, report, live) = garbage.open();
    assert_eq!(report.recovery, vec![Recovery::Discarded]);
    assert!(report.discarded.is_empty());
    assert!(store.records().is_empty());
    drop(store);
    // The next start is clean.
    let (_, report, _) = live.at_rest().open();
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
