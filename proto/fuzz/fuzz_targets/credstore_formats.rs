//! The credential store's two on-disk parsers: the database file and the
//! keyring blob. Arbitrary bytes must parse or fail, never panic; whatever
//! parses must encode back to bytes that parse to the same value.
#![no_main]

use eternal_credstore::format::{
    encode_db, encode_keyring, parse_db, parse_keyring, valid_generation, DbFile, GenerationKeys,
};
use eternal_wire::v3::wire::RecordState;
use libfuzzer_sys::fuzz_target;

const W: [u8; 32] = [0x5A; 32];

/// The store's invariants, checked independently of the parser on every
/// database it accepts.
fn check_store_invariants(db: &DbFile) {
    assert!(valid_generation(db.generation));
    assert!(db.records.len() <= 64 && db.tombstones.len() <= 1024);
    for r in &db.records {
        let same = |state| {
            db.records
                .iter()
                .filter(|o| o.slot_id == r.slot_id && o.state == state)
                .count()
        };
        assert!(same(RecordState::Active) <= 1 && same(RecordState::Pending) <= 3);
        assert_eq!(
            db.records.iter().filter(|o| o.dev_id == r.dev_id).count(),
            1
        );
        assert!(!db.tombstones.iter().any(|(_, t)| t.contains(&r.dev_id)));
        assert!(r.device_name.len() <= 64);
        match (r.state, r.replaces) {
            (RecordState::Active, replaces) => assert!(replaces.is_none()),
            (RecordState::Pending, Some(old)) => {
                assert_ne!(old, r.dev_id);
                if let Some(o) = db.records.iter().find(|o| o.dev_id == old) {
                    assert!(o.slot_id == r.slot_id && o.state == RecordState::Active);
                }
            }
            _ => {}
        }
    }
    let mut seen = Vec::new();
    for (slot, ids) in &db.tombstones {
        assert!(!ids.is_empty() && ids.len() <= 8);
        assert_eq!(db.tombstones.iter().filter(|(s, _)| s == slot).count(), 1);
        for id in ids {
            assert!(!seen.contains(id));
            seen.push(*id);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(db) = parse_db(data) {
        let keys = GenerationKeys::new(&W, db.generation);
        let again = encode_db(&db, &keys);
        assert!(keys.verify(&again));
        assert_eq!(parse_db(&again).unwrap(), db);
        // Bytes that parse but weren't signed under W don't verify, unless
        // they are exactly what W signs.
        assert_eq!(keys.verify(data), data == again.as_slice());
        for record in &db.records {
            // Arbitrary wrapped bytes never open under W.
            let _ = keys.unwrap(record);
        }
        check_store_invariants(&db);
    }
    if let Ok(state) = parse_keyring(data) {
        let again = encode_keyring(&state);
        assert_eq!(parse_keyring(&again).unwrap(), state);
        assert_eq!(again.as_slice(), data, "one encoding per value");
    }
});
