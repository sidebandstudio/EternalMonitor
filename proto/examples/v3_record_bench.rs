//! Seal and open throughput of v3 records at video datagram sizes, compared
//! with the per-frame budget at 120 fps.
//!
//! `cargo run --release -p eternal-wire --example v3_record_bench [seconds]`

use std::hint::black_box;
use std::time::{Duration, Instant};

use eternal_wire::v3::record::{Opener, Sealer, RECORD_OVERHEAD};

const STREAM_BPS: f64 = 50_000_000.0;
const FPS: f64 = 120.0;

fn measure(body: usize, budget: Duration) -> (f64, f64) {
    let key = [0x42u8; 32];
    let payload = vec![0xA5u8; body];

    // Seal: one buffer reused, as a sender would.
    let mut sealer = Sealer::new(&key, 7);
    let mut out = Vec::with_capacity(body + RECORD_OVERHEAD);
    let mut sealed = 0u64;
    let start = Instant::now();
    while start.elapsed() < budget {
        for _ in 0..1000 {
            sealer
                .seal_into(0x01, black_box(&payload), &mut out)
                .unwrap();
            black_box(&out);
        }
        sealed += 1000;
    }
    let seal_ns = start.elapsed().as_nanos() as f64 / sealed as f64;

    // Open: in place, every record fresh to the replay window.
    let mut sealer = Sealer::new(&key, 7);
    let records: Vec<Vec<u8>> = (0..4096)
        .map(|_| sealer.seal(0x01, &payload).unwrap())
        .collect();
    let mut opened = 0u64;
    let mut buf = Vec::with_capacity(body + RECORD_OVERHEAD);
    let mut total = Duration::ZERO;
    while total < budget {
        let opener = Opener::new(&key, 7);
        let start = Instant::now();
        for record in &records {
            buf.clear();
            buf.extend_from_slice(record);
            let (_, _, plain) = opener.open_in_place(&mut buf).unwrap();
            black_box(plain);
        }
        total += start.elapsed();
        opened += records.len() as u64;
    }
    let open_ns = total.as_nanos() as f64 / opened as f64;
    (seal_ns, open_ns)
}

fn main() {
    let seconds: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2.0);
    let budget = Duration::from_secs_f64(seconds);
    let frame_budget_us = 1e6 / FPS;
    println!(
        "v3 record bench: ChaCha20-Poly1305 (ring), {:.0} Mbps stream, {FPS} fps, frame budget {frame_budget_us:.1} µs",
        STREAM_BPS / 1e6
    );
    println!(
        "{:>6} {:>9} {:>9} {:>10} {:>10} {:>8} {:>13} {:>13}",
        "body",
        "seal ns",
        "open ns",
        "seal MB/s",
        "open MB/s",
        "pkt/fr",
        "seal µs/fr",
        "open µs/fr"
    );
    for body in [64usize, 576, 1200, 1344] {
        let (seal_ns, open_ns) = measure(body, budget);
        let packets_per_frame = STREAM_BPS / 8.0 / body as f64 / FPS;
        println!(
            "{body:>6} {seal_ns:>9.0} {open_ns:>9.0} {:>10.0} {:>10.0} {packets_per_frame:>8.1} {:>13.1} {:>13.1}",
            body as f64 / seal_ns * 1e3,
            body as f64 / open_ns * 1e3,
            packets_per_frame * seal_ns / 1e3,
            packets_per_frame * open_ns / 1e3,
        );
    }
    let (seal_ns, open_ns) = measure(1200, budget);
    let per_second = STREAM_BPS / 8.0 / 1200.0;
    println!(
        "1,200-byte datagrams at 50 Mbps: {per_second:.0}/s; one core: seal {:.2}%, open {:.2}%; per 120 fps frame: seal {:.1} µs + open {:.1} µs of {frame_budget_us:.0} µs",
        per_second * seal_ns / 1e7,
        per_second * open_ns / 1e7,
        per_second / FPS * seal_ns / 1e3,
        per_second / FPS * open_ns / 1e3,
    );
}
