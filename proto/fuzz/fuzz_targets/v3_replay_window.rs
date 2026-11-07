//! The replay window against a plain model, on fuzzer-chosen counter streams.
#![no_main]

use std::collections::BTreeSet;

use eternal_wire::v3::replay::{ReplayWindow, WINDOW};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut window = ReplayWindow::new();
    let mut seen = BTreeSet::new();
    let mut base = 0u64;
    for chunk in data.chunks(3) {
        let op = chunk[0];
        let arg = u64::from(u16::from_le_bytes([
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ]));
        let counter = match op % 4 {
            0 => base + (arg % 128),
            1 => base.saturating_sub(arg % 8192),
            2 => base + arg * 64,
            _ => arg.wrapping_mul(0x9E37_79B9) % (1 << 40),
        };
        let expected = match seen.last() {
            Some(&top) if counter <= top && top - counter >= WINDOW => false,
            _ => !seen.contains(&counter),
        };
        assert_eq!(window.check(counter), expected, "check {counter}");
        let marked = window.check_and_mark(counter);
        assert_eq!(marked, expected, "mark {counter}");
        if marked {
            seen.insert(counter);
            base = base.max(counter);
        }
        assert_eq!(window.top(), seen.last().copied());
    }
});
