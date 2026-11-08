//! Crash recovery: every record operation is killed at every disk and
//! keyring step, with unsynced renames either surviving or lost. A restart
//! must find exactly the state before or the state after, with the
//! invariants holding; once the keyring holds the intent, exactly the state
//! after. A keyring store that fails may also have happened. Every recovery
//! is itself killed at every step, and must still reach the same state.

mod common;

use common::*;
use eternal_credstore::{Recovery, StoreError};
use eternal_wire::v3::Id16;

type Op = fn(&mut Store) -> Result<(), StoreError>;

const OPS: &[(&str, Op)] = &[
    ("add_pending", |s| {
        s.add_pending(new_record(0x51, 5, None), NOW)
    }),
    ("add_pending_replacing", |s| {
        s.add_pending(new_record(0x14, 1, Some(0x11)), NOW)
    }),
    ("finalize_plain", |s| s.finalize(&id(0x21)).map(drop)),
    ("finalize_replacing", |s| s.finalize(&id(0x12)).map(drop)),
    ("finalize_conflict", |s| s.finalize(&id(0x13)).map(drop)),
    ("remove", |s| s.remove(&[id(0x11), id(0x21)])),
    ("expire", |s| s.expire(NOW).map(drop)),
    ("persist_timestamp", |s| {
        s.persist_timestamp(&id(0x11), 9_000)
    }),
];

/// Reopens `world`, checks it, then kills that recovery at every step and
/// checks the next restart reaches the same state. Returns the state.
fn recover(world: &World, context: &str) -> Logical {
    let (store, _, live) = world.open();
    check_invariants(&store);
    let reached = logical(&store);
    let recovery_steps = live.steps.count.get();
    drop(store);
    for k in 0..recovery_steps {
        for (keep, ambiguous) in [(false, false), (true, false), (false, true), (true, true)] {
            let (live, opened) = world.open_live(11, Some(k), ambiguous);
            assert!(opened.is_err(), "{context}: recovery step {k} was a write");
            let (again, _, _) = live.after_crash(keep).open();
            check_invariants(&again);
            assert_eq!(
                logical(&again),
                reached,
                "{context}: recovery killed at step {k} (keep unsynced {keep}, ambiguous keyring {ambiguous})"
            );
        }
    }
    reached
}

#[test]
fn every_operation_survives_a_crash_at_every_step() {
    let base = base_world();
    let before = recover(&base, "base");
    for (name, op) in OPS {
        let (mut store, _, live) = base.open();
        let opened_at = live.steps.count.get();
        op(&mut store).unwrap_or_else(|e| panic!("{name}: {e}"));
        let steps = live.steps.count.get() - opened_at;
        let after = logical(&store);
        assert_ne!(after, before, "{name} changes something");
        drop(store);
        assert_eq!(
            recover(&live.after_crash(false), name),
            after,
            "{name}: clean restart"
        );
        let rotates = live.keyring.borrow().get() != base.keyring.get();

        for (k, ambiguous) in (0..steps).flat_map(|k| [(k, false), (k, true)]) {
            let (mut store, _, live) = base.open();
            live.steps.crash_at.set(Some(opened_at + k));
            live.steps.ambiguous.set(ambiguous);
            assert!(op(&mut store).is_err(), "{name}: step {k} failed");
            check_invariants(&store);
            let intent_recorded = live.keyring.borrow().get() != base.keyring.get();
            if intent_recorded {
                // Memory follows the recorded intent, and the store asks to
                // be reopened.
                assert_eq!(logical(&store), after, "{name}: memory after step {k}");
                assert!(matches!(store.expire(NOW), Err(StoreError::NeedsReopen)));
            } else {
                assert_eq!(logical(&store), before, "{name}: memory after step {k}");
            }
            drop(store);
            for keep in [false, true] {
                let context = format!(
                    "{name} killed at step {k} (keep unsynced {keep}, ambiguous keyring {ambiguous})"
                );
                let reached = recover(&live.after_crash(keep), &context);
                if intent_recorded {
                    assert_eq!(reached, after, "{context}: intent recorded");
                } else {
                    assert!(reached == before || reached == after, "{context}");
                }
            }
        }
        assert!(
            rotates
                || !matches!(
                    *name,
                    "finalize_replacing" | "finalize_conflict" | "remove" | "expire"
                ),
            "{name} rotates"
        );
    }
}

/// A database that can't be opened is discarded through the full
/// procedure; killed anywhere, the next restart finishes the same discard
/// and never loads a record from it.
#[test]
fn a_startup_discard_survives_a_crash_at_every_step() {
    let base = base_world();
    let held: Vec<Id16> = {
        let (store, _, _) = base.open();
        store.records().iter().map(|r| r.dev_id).collect()
    };
    // The same database with another host's keyring, and with none.
    let other = World::default().open().2.after_crash(false).keyring;
    let mut foreign = base.clone();
    foreign.keyring = other;
    let mut no_keyring = base.clone();
    no_keyring.keyring.put(None);
    for (name, world, first) in [
        ("foreign keyring", foreign, Recovery::Discarded),
        ("no keyring", no_keyring, Recovery::FreshKeyring),
    ] {
        let (store, report, _) = world.open();
        assert_eq!(report.recovery[0], first, "{name}");
        assert!(report.recovery.contains(&Recovery::Discarded), "{name}");
        let mut listed: Vec<Id16> = report.discarded.iter().map(|d| d.dev_id).collect();
        listed.sort();
        let mut expected = held.clone();
        expected.sort();
        assert_eq!(listed, expected, "{name}: every record listed");
        assert!(store.records().is_empty());
        drop(store);
        let reached = recover(&world, name);
        assert!(reached.records.is_empty(), "{name}");
        for dev_id in &held {
            assert!(
                reached
                    .tombstones
                    .iter()
                    .any(|(_, ids)| ids.contains(dev_id)),
                "{name}: every discarded record is tombstoned"
            );
        }
        // The tombstone from before the restore is kept.
        assert!(reached
            .tombstones
            .iter()
            .any(|(_, ids)| ids.contains(&id(0x41))));
    }
}
