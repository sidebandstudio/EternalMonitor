//! Crash recovery. Operations, alone and in unsettled sequences on one open
//! store, are killed at every disk and keyring step, or allowed to finish.
//! Then either the process restarts in the same boot (nothing lost), or the
//! power fails and the next boot finds:
//! - unsynced renames survived or lost;
//! - interrupted writes as half a file;
//! - any keyring blob that was visible but not durable, kept or lost: a
//!   failed store may have happened, and without a flush any store since
//!   the last confirmed one may be lost.
//!
//! What must hold (R1):
//! - every operation that returned Ok is still there after any restart;
//!   the one that was killed is exactly "before" or "after"; timestamps
//!   never go backwards and nothing untouched is discarded;
//! - in memory, a failed operation leaves exactly "before", or exactly
//!   "after" (refusing further writes) once its intent is visible;
//! - every recovery is itself killed at every step, under every outcome,
//!   and the next restart reaches the same state;
//! - the invariants, always;
//! - everything runs with a keyring that flushes (Windows) and one that
//!   doesn't (macOS).

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

/// Unsettled sequences on one open store, mixing rotations and plain writes.
/// The first two are a rotation, then an acknowledged plain write.
const SEQUENCES: &[(&str, &[Op])] = &[
    (
        "remove, then add",
        &[
            |s| s.remove(&[id(0x21)]),
            |s| s.add_pending(new_record(0x51, 5, None), NOW),
        ],
    ),
    (
        "remove, then persist a timestamp",
        &[
            |s| s.remove(&[id(0x21)]),
            |s| s.persist_timestamp(&id(0x11), 9_000),
        ],
    ),
    (
        "mixed",
        &[
            |s| s.expire(NOW).map(drop),
            |s| s.add_pending(new_record(0x51, 5, None), NOW),
            |s| s.finalize(&id(0x12)).map(drop),
            |s| s.persist_timestamp(&id(0x12), 7_000),
            |s| s.remove(&[id(0x51)]),
            |s| s.persist_timestamp(&id(0x12), 8_000),
        ],
    ),
];

fn keyrings() -> [(&'static str, MemKeyring); 2] {
    [
        ("no flush", MemKeyring::new()),
        ("flushing", MemKeyring::flushing()),
    ]
}

/// Every world a restart of `live` may find: the same boot, or each power
/// loss outcome.
fn restarts(live: &Live) -> Vec<(String, World)> {
    let mut worlds = vec![("same boot".to_string(), live.same_boot())];
    for keep in [false, true] {
        for choice in 0..live.keyring_choices() {
            worlds.push((
                format!("power loss (keep unsynced {keep}, keyring choice {choice})"),
                live.after_crash(keep, choice),
            ));
        }
    }
    worlds
}

/// Reopens `world`, checks it reaches one of `allowed`, then kills that
/// recovery at every step under every outcome and checks the next restart
/// does too. (A killed operation's unsynced write may be seen by a reopen in
/// the same boot and still be lost to a later power failure: both states are
/// allowed for it.) Returns the state reached.
fn recover_to(world: &World, context: &str, allowed: &[&Logical]) -> Logical {
    let (store, _, live) = world.open();
    check_invariants(&store);
    let reached = logical(&store);
    assert!(allowed.contains(&&reached), "{context}");
    let recovery_steps = live.steps.count.get();
    drop(store);
    for k in 0..recovery_steps {
        for ambiguous in [false, true] {
            let (live, opened) = world.open_live(11, Some(k), ambiguous);
            assert!(opened.is_err(), "{context}: recovery step {k} was a write");
            for (how, next) in restarts(&live) {
                let (again, _, _) = next.open();
                check_invariants(&again);
                assert!(
                    allowed.contains(&&logical(&again)),
                    "{context}: recovery killed at step {k} (ambiguous {ambiguous}), then {how}"
                );
            }
        }
    }
    reached
}

/// [`recover_to`] where exactly one state is allowed: the one reached.
fn recover(world: &World, context: &str) -> Logical {
    let (store, _, _) = world.open();
    let reached = logical(&store);
    drop(store);
    recover_to(world, context, &[&reached])
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

            // Killed at step k, or (k == steps) finished.
            for k in 0..=steps {
                for ambiguous in [false, true] {
                    if k == steps && ambiguous {
                        continue;
                    }
                    let (mut store, _, live) = base.open();
                    let finished = k == steps;
                    if !finished {
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
                    for (how, world) in restarts(&live) {
                        let context = format!(
                            "{name} {} (ambiguous {ambiguous}), then {how}",
                            if finished {
                                "finished".to_string()
                            } else {
                                format!("killed at step {k}")
                            }
                        );
                        if finished {
                            // It returned Ok.
                            recover_to(&world, &context, &[&after]);
                        } else {
                            recover_to(&world, &context, &[&before, &after]);
                        }
                    }
                }
            }
        }
    }
}

/// Runs `ops` on one open store from `base`, killed at global step `kill`
/// (or never), with a same-boot reopen after operation `reopen_after` if
/// given. Returns how many returned Ok, and the live disk and keyring.
fn run_sequence(
    base: &World,
    ops: &[Op],
    kill: Option<usize>,
    reopen_after: Option<usize>,
) -> (usize, Live) {
    let (mut store, _, mut live) = base.open();
    let start = live.steps.count.get();
    live.steps.crash_at.set(kill.map(|k| start + k));
    let mut acked = 0;
    for (i, op) in ops.iter().enumerate() {
        if op(&mut store).is_err() {
            return (acked, live);
        }
        acked += 1;
        if reopen_after == Some(i) {
            drop(store);
            let crash_at = live.steps.crash_at.get();
            let used = live.steps.count.get();
            let (s, _, l) = live.same_boot().open();
            store = s;
            live = l;
            // Keep counting the same global steps.
            live.steps
                .crash_at
                .set(crash_at.map(|at| at.saturating_sub(used) + live.steps.count.get()));
        }
    }
    (acked, live)
}

/// The states after 0, 1, ... n operations of `ops`, run cleanly.
fn sequence_states(base: &World, ops: &[Op]) -> Vec<Logical> {
    let (mut store, _, _) = base.open();
    let mut states = vec![logical(&store)];
    for op in ops {
        op(&mut store).unwrap();
        states.push(logical(&store));
    }
    states
}

#[test]
fn acknowledged_writes_survive_unsettled_sequences() {
    for (mode, keyring) in keyrings() {
        let base = base_world_with(keyring);
        for (name, ops) in SEQUENCES {
            let states = sequence_states(&base, ops);
            let total = {
                let (acked, live) = run_sequence(&base, ops, None, None);
                assert_eq!(acked, ops.len());
                let (s, _, l) = base.open();
                drop(s);
                live.steps.count.get() - l.steps.count.get()
            };
            for reopen_after in [None, Some(0)] {
                for kill in (0..total).map(Some).chain([None]) {
                    let (acked, live) = run_sequence(&base, ops, kill, reopen_after);
                    for (how, world) in restarts(&live) {
                        let context = format!(
                            "{mode}: {name}, reopen after {reopen_after:?}, killed at {kill:?} \
                             ({acked} acknowledged), then {how}"
                        );
                        let (store, _, _) = world.open();
                        check_invariants(&store);
                        let reached = logical(&store);
                        drop(store);
                        if kill.is_none() || acked == ops.len() {
                            assert_eq!(reached, states[ops.len()], "{context}");
                        } else {
                            assert!(
                                reached == states[acked] || reached == states[acked + 1],
                                "{context}"
                            );
                        }
                    }
                }
            }
            // The finished sequence, nested kills of its first recovery.
            let (_, live) = run_sequence(&base, ops, None, None);
            for (how, world) in restarts(&live) {
                recover_to(
                    &world,
                    &format!("{mode}: {name}, then {how}"),
                    &[&states[ops.len()]],
                );
            }
        }
    }
}

/// A Remove, then acknowledged plain writes (a pairing and a timestamp),
/// then a power loss that keeps only the pre-rotation keyring: every write
/// is still there.
#[test]
fn a_lost_rotation_keyring_keeps_later_writes() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    store.add_pending(new_record(0x51, 5, None), NOW).unwrap();
    store.persist_timestamp(&id(0x11), 9_000).unwrap();
    let after = logical(&store);
    drop(store);
    // Choice 0: every keyring store since the last confirmed one lost.
    let lost = live.after_crash(false, 0);
    assert_eq!(lost.keyring.get(), base.keyring.get());
    let (store, report, _) = lost.open();
    assert_eq!(
        logical(&store),
        after,
        "the pairing and the timestamp survive"
    );
    assert_eq!(store.snapshot().last_committed_timestamp(&id(0x11)), 9_000);
    // The Remove still gets a new key.
    assert_eq!(report.recovery, vec![Recovery::RotatedAgain]);
}

/// A Remove, reopens in the same boot, then a power loss that keeps only
/// the pre-rotation keyring: the untouched records are still there.
#[test]
fn a_same_boot_reopen_never_drops_the_fallback() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    store.remove(&[id(0x21)]).unwrap();
    let after = logical(&store);
    drop(store);
    let (store, _, live) = live.same_boot().open();
    assert_eq!(logical(&store), after);
    drop(store);
    let (store, _, live) = live.same_boot().open();
    drop(store);
    for choice in 0..live.keyring_choices() {
        let (store, _, _) = live.after_crash(false, choice).open();
        assert_eq!(logical(&store), after, "keyring choice {choice}");
        assert_eq!(store.records().len(), 4, "the four untouched records");
    }
}

/// What an open loads it serves, so it's durable from then on: a killed
/// write whose rename a reopen in the same boot saw survives a later power
/// loss.
#[test]
fn what_an_open_sees_survives_a_later_power_loss() {
    for (mode, keyring) in keyrings() {
        let base = base_world_with(keyring);
        let (mut store, _, live) = base.open();
        // Kill the add at its directory sync: the rename is visible only.
        live.steps.crash_at.set(Some(live.steps.count.get() + 2));
        assert!(store.add_pending(new_record(0x51, 5, None), NOW).is_err());
        drop(store);
        let (store, _, live) = live.same_boot().open();
        let seen = logical(&store);
        assert!(
            store.records().iter().any(|r| r.dev_id == id(0x51)),
            "{mode}"
        );
        drop(store);
        for choice in 0..live.keyring_choices() {
            let (store, _, _) = live.after_crash(false, choice).open();
            assert_eq!(logical(&store), seen, "{mode}: keyring choice {choice}");
        }
    }
}

/// Two rotations, then a power loss that keeps only the first one's step-1
/// blob: the database already reflects both, so the open finishes the first
/// and owes a rotation past the second. Killed anywhere in that recovery,
/// the debt is never forgotten: once an open completes, the committed
/// generation is past both.
#[test]
fn an_owed_rotation_survives_a_killed_recovery() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    let start = store.generation();
    store.remove(&[id(0x21)]).unwrap();
    store.expire(NOW).unwrap();
    assert_eq!(store.generation(), start + 2);
    drop(store);
    // History: the base blob, then step 1 and step 3 of each rotation.
    assert_eq!(live.keyring_choices(), 5);
    let world = live.after_crash(false, 1);
    let (store, report, _) = world.open();
    assert!(report.recovery.contains(&Recovery::RotatedAgain));
    assert!(store.generation() > start + 2);
    let steps = {
        let (live, _) = world.open_live(1, None, false);
        live.steps.count.get()
    };
    drop(store);
    for k in 0..steps {
        let (live, opened) = world.open_live(11, Some(k), false);
        assert!(opened.is_err());
        for (how, next) in restarts(&live) {
            let (store, _, _) = next.open();
            assert!(
                store.generation() > start + 2,
                "recovery killed at step {k}, then {how}: generation {}",
                store.generation()
            );
        }
    }
}

/// A staged rotation's blob survives while the database has moved past it
/// (a startup discard and later writes): its intent isn't replayed over
/// them. Here the discard couldn't read the file, so a `dev_id` comes back
/// in a later pairing, which a replay would wrongly remove.
#[test]
fn a_stale_staged_intent_is_not_replayed() {
    let base = base_world();
    let (mut store, _, live) = base.open();
    // A Remove of 0x21 stopped right after step 1.
    live.steps.crash_at.set(Some(live.steps.count.get() + 1));
    assert!(store.remove(&[id(0x21)]).is_err());
    drop(store);
    let mut world = live.at_rest();
    for g in db_generations(&world) {
        world
            .disk
            .put(&format!("credentials-{g}.db"), b"unreadable".to_vec());
    }
    let (mut store, report, live) = world.open();
    assert!(report.recovery.contains(&Recovery::Discarded));
    assert!(report.discarded.is_empty(), "nothing could be listed");
    store.add_pending(new_record(0x21, 2, None), NOW).unwrap();
    store.add_pending(new_record(0x51, 5, None), NOW).unwrap();
    store.remove(&[id(0x51)]).unwrap();
    let after = logical(&store);
    drop(store);
    // Keep only the blob from before this boot's writes: the staged Remove.
    let (store, _, _) = live.after_crash(false, 0).open();
    assert_eq!(logical(&store), after);
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

/// A rotation stopped after step 1, then the database is damaged: the
/// recorded intent is folded into a startup discard, which survives a
/// crash at every step like any other.
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
        assert_eq!(gens.len(), 1, "{mode}: one database file");
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
