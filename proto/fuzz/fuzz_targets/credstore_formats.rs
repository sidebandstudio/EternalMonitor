//! The credential store's two on-disk parsers: the database file and the
//! keyring blob. Arbitrary bytes must parse or fail, never panic; whatever
//! parses must encode back to bytes that parse to the same value.
#![no_main]

use eternal_credstore::format::{
    encode_db, encode_keyring, parse_db, parse_keyring, GenerationKeys,
};
use libfuzzer_sys::fuzz_target;

const W: [u8; 32] = [0x5A; 32];

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
    }
    if let Ok(state) = parse_keyring(data) {
        let again = encode_keyring(&state);
        assert_eq!(parse_keyring(&again).unwrap(), state);
        assert_eq!(again.as_slice(), data, "one encoding per value");
    }
});
