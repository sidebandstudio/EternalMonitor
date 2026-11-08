//! Times the commit path's write: persisting a committed timestamp, one
//! plain database write, with the store full (64 records).
//!
//! `cargo run --release -p eternal-credstore --features test-hooks --example
//! credstore_bench -- <dir> [iterations]`. The directory is created and
//! removed.

use std::time::Instant;

use eternal_credstore::keyring::MemKeyring;
use eternal_credstore::{CredentialStore, NewRecord, StdDisk};
use eternal_wire::v3::{SystemRandom, Zeroizing};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next()
            .expect("usage: credstore_bench <dir> [iterations]"),
    );
    let iterations: usize = args.next().map_or(1000, |n| n.parse().expect("iterations"));
    let disk = StdDisk::new(&dir).expect("directory");
    let (mut store, _) =
        CredentialStore::open(disk, MemKeyring::new(), Box::new(SystemRandom::default()))
            .expect("open");
    for i in 0..64u8 {
        store
            .add_pending(
                NewRecord {
                    dev_id: [i; 16],
                    slot_id: [i; 16],
                    key: Box::new(Zeroizing::new([i; 32])),
                    replaces: None,
                    device_name: "iPad Pro 13-inch (M5)".into(),
                },
                1_000,
            )
            .expect("add");
    }
    let mut times = Vec::with_capacity(iterations);
    for n in 0..iterations {
        let start = Instant::now();
        store
            .persist_timestamp(&[(n % 64) as u8; 16], 1_000 + n as u64)
            .expect("persist");
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() as f64 * q) as usize).min(times.len() - 1)];
    println!(
        "persist_timestamp, 64 records, {iterations} runs: p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        at(0.50),
        at(0.95),
        at(0.99),
        times[times.len() - 1]
    );
    drop(store);
    std::fs::remove_dir_all(&dir).expect("clean up");
}
