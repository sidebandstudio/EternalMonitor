//! Crash recovery. Every record operation is killed at every disk and
//! keyring step, and also allowed to finish and then lose power. Each
//! restart then varies three things:
//! - unsynced renames survive or are lost;
//! - interrupted writes leave half a file;
//! - the keyring keeps any blob that was visible but not durable: a failed
//!   keyring store may have happened, and on a keyring without a flush any
//!   store may be lost.
//!
//! What must hold:
//! - the restart finds exactly the state before or after the operation,
//!   with the invariants holding;
//! - a rotation whose keyring blobs were all lost comes back exactly
//!   "before", and one with any of them kept exactly "after";
//! - in memory, a failed operation leaves exactly "before", or exactly
//!   "after" (refusing further writes) once its intent is visible;
//! - every recovery is itself killed at every step, under every keyring
//!   outcome, and the next restart reaches the same state;
//! - the whole matrix runs with a keyring that flushes (Windows) and one
//!   that doesn't (macOS).

mod common;

use common::*;
use eternal_credstore::{Recovery, StoreError};
use eternal_wire::v3::Id16;

type Op = fn(&mut Store) -> Result<(), StoreError>;

const OPS: &[(&str, bool, Op)] = &[
    ("add_pending", false, |s| {
        s.add_pending(new_record(0x51, 5, None), NOW)
    }),
    ("add_pending_replacing", false, |s| {
        s.add_pending(new_record(0x14, 1, Some(0x11)), NOW)
    }),
    ("finalize_plain", false, |s| s.finalize(&id(0x21)).map(drop)),
    ("finalize_replacing", true, |s| {
        s.finalize(&id(0x12)).map(drop)
    }),
    ("finalize_conflict", true, |s| {
        s.finalize(&id(0x13)).map(drop)
    }),
    ("remove", true, |s| s.remove(&[id(0x11), id(0x21)])),
    ("expire", true, |s| s.expire(NOW).map(drop)),
    ("persist_timestamp", false, |s| {
        s.persist_timestamp(&id(0x11), 9_000)
    }),
];

fn keyrings() -> [(&'static str, MemKeyring); 2] {
    [
        ("no flush", MemKeyring::new()),
        ("flushing", MemKeyring::flushing()),
    ]
}

/// Reopens `world`, checks it, then kills that recovery at every step under
/// every outcome, and checks the next restart reaches the same state.
/// Returns the state.
fn recover(world: &World, context: &str) -> Logical {
    let (store, _, live) = world.open();
    check_invariants(&store);
    let reached = logical(&store);
    let recovery_steps = live.steps.count.get();
    drop(store);
    for k in 0..recovery_steps {
        for ambiguous in [false, true] {
            let (live, opened) = world.open_live(11, Some(k), ambiguous);
            assert!(opened.is_err(), "{context}: recovery step {k} was a write");
            for keep in [false, true] {
                for choice in 0..live.keyring_choices() {
                    let (again, _, _) = live.after_crash(keep, choice).open();
                    check_invariants(&again);
                    assert_eq!(
                        logical(&again),
                        reached,
                        "{context}: recovery killed at step {k} (ambiguous {ambiguous}, keep \
                         unsynced {keep}, keyring choice {choice})"
                    );
                }
            }
        }
    }
    reached
}

#[test]
fn every_operation_survives_a_crash_at_every_step() {
    for (mode, keyring) in keyrings() {
        let base = base_world_with(keyring);
        let before = recover(&base, mode);
        for (name, rotates, op) in OPS {
            let name = format!("{mode}: {name}");
            let (mut store, _, live) = base.open();
            let opened_at = live.steps.count.get();
            op(&mut store).unwrap_or_else(|e| panic!("{name}: {e}"));
            let steps = live.steps.count.get() - opened_at;
            let after = logical(&store);
            assert_ne!(after, before, "{name} changes something");
            assert_eq!(
                live.keyring.borrow().get() != base.keyring.get(),
                *rotates,
                "{name} rotates"
            );
            drop(store);

            // Killed at step k, or (k == steps) finished and then lost power.
            for k in 0..=steps {
                for ambiguous in [false, true] {
                    if k == steps && ambiguous {
                        continue;
                    }
                    let (mut store, _, live) = base.open();
                    if k < steps {
                        live.steps.crash_at.set(Some(opened_at + k));
                        live.steps.ambiguous.set(ambiguous);
                        assert!(op(&mut store).is_err(), "{name}: step {k} failed");
                        check_invariants(&store);
                        let visible = live.keyring.borrow().get() != base.keyring.get();
                        if visible {
                            // Memory follows a visible intent, and the store
                            // refuses writes until reopened.
                            assert_eq!(logical(&store), after, "{name}: memory after step {k}");
                            assert!(matches!(store.expire(NOW), Err(StoreError::NeedsReopen)));
                        } else {
                            assert_eq!(logical(&store), before, "{name}: memory after step {k}");
                        }
                    } else {
                        op(&mut store).unwrap();
                    }
                    drop(store);
                    for keep in [false, true] {
                        for choice in 0..live.keyring_choices() {
                            let world = live.after_crash(keep, choice);
                            let context = format!(
                                "{name} killed at step {k} (ambiguous {ambiguous}, keep unsynced \
                                 {keep}, keyring choice {choice})"
                            );
                            let kept_old_keyring = world.keyring.get() == base.keyring.get();
                            let reached = recover(&world, &context);
                            if !rotates {
                                assert!(reached == before || reached == after, "{context}");
                            } else if kept_old_keyring {
                                assert_eq!(reached, before, "{context}: keyring blobs lost");
                            } else {
                                assert_eq!(reached, after, "{context}: keyring blob kept");
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A database that can't be opened is discarded through the full
/// procedure. Killed anywhere, the next restart finishes the same discard
/// and never loads a record from it.
#[test]
fn a_startup_discard_survives_a_crash_at_every_step() {
    for (mode, keyring) in keyrings() {
        let base = base_world_with(keyring.clone());
        let held: Vec<Id16> = {
            let (store, _, _) = base.open();
            store.records().iter().map(|r| r.dev_id).collect()
        };
        // The same database with another host's keyring, and with none.
        let other = World {
            keyring: keyring.clone(),
            ..World::default()
        }
        .open()
        .2
        .at_rest()
        .keyring;
        let mut foreign = base.clone();
        foreign.keyring = other;
        let mut no_keyring = base.clone();
        no_keyring.keyring.put(None);
        for (name, world, first) in [
            ("foreign keyring", foreign, Recovery::Discarded),
            ("no keyring", no_keyring, Recovery::FreshKeyring),
        ] {
            let name = format!("{mode}: {name}");
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
            let reached = recover(&world, &name);
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
}

/// A rotation stopped after step 1, then neither generation's file opens
/// (damaged): the recorded intent is folded into a startup discard, which
/// survives a crash at every step like any other.
#[test]
fn a_staged_rotation_with_no_readable_file_is_discarded() {
    for (mode, keyring) in keyrings() {
        let base = base_world_with(keyring);
        let (mut store, _, live) = base.open();
        // Stop right after step 1 of a Remove.
        live.steps.crash_at.set(Some(live.steps.count.get() + 1));
        assert!(store.remove(&[id(0x11)]).is_err());
        drop(store);
        let mut world = live.after_crash(false, usize::MAX);
        let gens = db_generations(&world);
        assert_eq!(gens.len(), 1, "{mode}: only the committed file");
        for g in &gens {
            let name = format!("credentials-{g}.db");
            let mut bytes = world.disk.get(&name).unwrap().clone();
            *bytes.last_mut().unwrap() ^= 1;
            world.disk.put(&name, bytes);
        }
        let (store, report, _) = world.open();
        assert!(report.recovery.contains(&Recovery::Discarded), "{mode}");
        assert_eq!(report.discarded.len(), 5, "{mode}");
        assert!(store.records().is_empty());
        drop(store);
        let reached = recover(&world, mode);
        assert!(reached.records.is_empty(), "{mode}");
        assert!(reached
            .tombstones
            .iter()
            .any(|(_, ids)| ids.contains(&id(0x11))));
    }
}
