//! The record opener on arbitrary bytes, and on real records with
//! fuzzer-chosen bit flips and delivery order: a changed record never opens,
//! an unchanged one opens exactly once while inside the window.
#![no_main]

use eternal_wire::v3::record::{Opener, Sealer};
use libfuzzer_sys::fuzz_target;

const KEY: [u8; 32] = [0x5C; 32];

fuzz_target!(|data: &[u8]| {
    let opener = Opener::new(&KEY, 9);
    assert!(opener.open(data).is_err(), "no forgery from arbitrary bytes");

    let Some((&count, rest)) = data.split_first() else {
        return;
    };
    let count = usize::from(count % 32) + 1;
    let mut sealer = Sealer::new(&KEY, 9);
    let records: Vec<Vec<u8>> = (0..count)
        .map(|i| {
            let body = rest.get(i..(i + 64).min(rest.len())).unwrap_or(&[]);
            sealer.seal(rest.first().copied().unwrap_or(0), body).unwrap()
        })
        .collect();
    let mut opened = vec![false; count];
    for op in rest.chunks(4) {
        let index = usize::from(op[0]) % count;
        let flip = op.get(1).copied().unwrap_or(0);
        let at = usize::from(u16::from_le_bytes([
            op.get(2).copied().unwrap_or(0),
            op.get(3).copied().unwrap_or(0),
        ]));
        let mut record = records[index].clone();
        let changed = flip != 0;
        if changed {
            let len = record.len();
            record[at % len] ^= flip;
        }
        let result = opener.open(&record);
        if changed {
            assert!(result.is_err(), "a changed record opened");
        } else if opened[index] {
            assert!(result.is_err(), "a record opened twice");
        } else {
            let opened_now = result.expect("a fresh record inside the window opens");
            assert_eq!(opened_now.counter, index as u64);
            opened[index] = true;
        }
    }
});
