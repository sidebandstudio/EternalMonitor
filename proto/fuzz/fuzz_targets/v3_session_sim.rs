//! Host and iPad state machines together on the simulated network, with
//! fuzzer-chosen loss, duplication, delay, latency, host answers, reconnects
//! (also mid-confirm and automatic), disconnects, lost acks and refusals,
//! host restarts, wall clock steps, persisted boundary timestamps, replays
//! and forged datagrams. After every step the host table's invariants hold,
//! and every iPad only ever goes live on a session the host committed for it,
//! also after the network settles.
//!
//! In lossless mode (no drops, no disconnect, revoke, restart, roaming or
//! spoofed sources, which all legitimately leave a commit unseen), every
//! commit the host makes must also be one the iPad uses.
#![no_main]

#[path = "../../tests/v3_sim/mod.rs"]
mod v3_sim;

use std::cell::RefCell;
use std::rc::Rc;

use libfuzzer_sys::fuzz_target;
use eternal_wire::v3::client::ClientConfig;
use eternal_wire::v3::host::HostConfig;
use v3_sim::*;

struct Cursor {
    data: Vec<u8>,
    at: usize,
}

impl Cursor {
    /// The next input byte; 0xFF (deliver, step) once the input runs out.
    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.at).copied().unwrap_or(0xFF);
        self.at += 1;
        b
    }

    fn done(&self) -> bool {
        self.at >= self.data.len()
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let cursor = Rc::new(RefCell::new(Cursor {
        data: data.to_vec(),
        at: 0,
    }));
    let seed = u64::from(cursor.borrow_mut().byte());
    // Short session ages, so the automatic reconnect runs.
    let mut host_config = HostConfig::new(HOST_ID);
    host_config.max_age = std::time::Duration::from_secs(12);
    let mut client_config = ClientConfig::new(HOST_ID);
    client_config.max_age = std::time::Duration::from_secs(12);
    client_config.reconnect_margin = std::time::Duration::from_secs(4);
    let mut sim = Sim::with_configs(seed, host_config, client_config);
    sim.latency = ms(u64::from(cursor.borrow_mut().byte()) * 4);
    sim.policy.durable_delay = ms(u64::from(cursor.borrow_mut().byte()) * 4);
    let flags = cursor.borrow_mut().byte();
    let lossless = flags & 4 == 4;
    sim.policy.refuse = flags & 1 == 1;
    sim.policy.invalidate_before_refuse = flags & 2 == 2;
    // Decisions from 0 to about 3 s late.
    sim.policy.decision_delay = ms(u64::from(cursor.borrow_mut().byte()) * 12);
    let second = sim.add_ipad(SimAddr { ip: 20, port: 1 });
    let net = cursor.clone();
    // Acks and refusals to lose next, set by an op.
    let lose_answers = Rc::new(std::cell::Cell::new(0u8));
    let lose = lose_answers.clone();
    sim.fault = Box::new(move |sent| {
        if lossless {
            // Reordering and duplication only.
            let b = net.borrow_mut().byte();
            return match b % 16 {
                2 => Fate::Duplicate { extra: ms(u64::from(b / 16) * 10) },
                3 => Fate::Delay { extra: ms(u64::from(b / 16) * 50) },
                _ => Fate::Deliver,
            };
        }
        if sent.kind == Kind::AckOrRefused && lose.get() > 0 {
            lose.set(lose.get() - 1);
            return Fate::Drop;
        }
        let b = net.borrow_mut().byte();
        match b % 16 {
            0 | 1 => Fate::Drop,
            2 => Fate::Duplicate { extra: ms(u64::from(b / 16) * 10) },
            3 => Fate::Delay { extra: ms(u64::from(b / 16) * 50) },
            _ => Fate::Deliver,
        }
    });
    sim.policy.media_every = Some(ms(100));
    sim.connect();
    for _ in 0..300 {
        if cursor.borrow().done() {
            break;
        }
        let op = cursor.borrow_mut().byte();
        let op = match op % 32 {
            // Ops that legitimately leave a commit unseen.
            3 | 7 | 8 | 9 | 10 | 14 if lossless => 31,
            other => other,
        };
        match op {
            0 => sim.connect(),
            1 => {
                // Replay something already sent, in either direction.
                let pick = usize::from(cursor.borrow_mut().byte());
                if let Some(sent) = sim.sent.get(pick % sim.sent.len().max(1)) {
                    let (dir, datagram) = (sent.dir, sent.datagram.clone());
                    sim.inject(dir, &datagram);
                }
            }
            2 => {
                // A forged copy with some bytes changed.
                let pick = usize::from(cursor.borrow_mut().byte());
                let at = usize::from(cursor.borrow_mut().byte());
                let x = cursor.borrow_mut().byte() | 1;
                if let Some(sent) = sim.sent.get(pick % sim.sent.len().max(1)) {
                    let (dir, mut datagram) = (sent.dir, sent.datagram.clone());
                    let len = datagram.len();
                    datagram[at % len] ^= x;
                    sim.inject(dir, &datagram);
                }
            }
            3 => sim.revoke(),
            4 => sim.policy.refuse = !sim.policy.refuse,
            6 => sim.connect_other(second),
            9 => sim.disconnect(),
            10 => {
                let count = cursor.borrow_mut().byte() % 4 + 1;
                lose_answers.set(count);
            }
            7 => {
                // Device 0 roams to a fuzzer-chosen address.
                let ip = u16::from(cursor.borrow_mut().byte() % 4);
                sim.roam(SimAddr { ip: 7 + ip, port: 50_000 });
            }
            8 => {
                // Anything sent so far, replayed from a spoofed source.
                let pick = usize::from(cursor.borrow_mut().byte());
                let ip = u16::from(cursor.borrow_mut().byte());
                if let Some(sent) = sim.sent.get(pick % sim.sent.len().max(1)) {
                    let datagram = sent.datagram.clone();
                    sim.inject_from(SimAddr { ip, port: ip }, &datagram);
                }
            }
            5 => {
                let ticks = u64::from(cursor.borrow_mut().byte());
                sim.run_for(ms(ticks * 8));
            }
            11 => {
                // The app asks again while an attempt is confirming.
                if sim.client.is_confirming() {
                    sim.connect();
                }
            }
            12 => {
                // The host's wall clock steps by up to ±5 days.
                let hours = i64::from(cursor.borrow_mut().byte()) - 128;
                let wall = sim.clock_us() as i64 + hours * 3_600_000_000;
                let now = sim.now;
                sim.host.set_wall_clock(now, wall.max(0) as u64);
            }
            13 => {
                // Persisted values near a boundary, on either side.
                use eternal_wire::v3::MAX_TIMESTAMP_US;
                let day = 86_400_000_000u64;
                let pick = cursor.borrow_mut().byte();
                let value = match pick % 5 {
                    0 => MAX_TIMESTAMP_US - 3,
                    1 => sim.clock_us() + 3 * day,
                    2 => sim.clock_us() + day - 1,
                    3 => sim.clock_us().saturating_sub(2 * day),
                    _ => 0,
                };
                if pick & 0x80 == 0 {
                    sim.client.set_last_timestamp(value);
                } else {
                    sim.store.timestamps.borrow_mut().insert(sim.key.dev_id, value);
                }
            }
            14 => {
                // The host restarts: sessions gone, store kept.
                let wall = sim.clock_us();
                let mut config = HostConfig::new(HOST_ID);
                config.max_age = std::time::Duration::from_secs(12);
                sim.restart_host(config, wall);
            }
            _ => sim.step(),
        }
        sim.check_live_only_on_commits();
    }
    // Let everything in flight settle, then check once more. Settling runs
    // past the iPad's 11 s retention of abandoned attempts.
    sim.run_for(ms(12_000));
    sim.check_live_only_on_commits();
    if lossless {
        // Every commit old enough to have reached the iPad (its 11 s
        // retention, plus slack for latency and the durable write).
        let settled = sim.now - std::time::Duration::from_secs(12);
        assert_eq!(sim.unused_commits_before(settled), 0, "a commit the iPad never used");
    }
});
