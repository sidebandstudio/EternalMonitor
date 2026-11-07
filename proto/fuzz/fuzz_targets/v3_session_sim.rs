//! Host and iPad state machines together on the simulated network, with
//! fuzzer-chosen loss, duplication, delay, latency, host answers, reconnects,
//! replays and forged datagrams. After every step the host table's
//! invariants hold, and the iPad only ever goes live on a session the host
//! committed.
#![no_main]

#[path = "../../tests/v3_sim/mod.rs"]
mod v3_sim;

use std::cell::RefCell;
use std::rc::Rc;

use libfuzzer_sys::fuzz_target;
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
    let mut sim = Sim::new(seed);
    sim.latency = ms(u64::from(cursor.borrow_mut().byte()) * 4);
    sim.policy.durable_delay = ms(u64::from(cursor.borrow_mut().byte()) * 4);
    sim.policy.refuse = cursor.borrow_mut().byte() & 1 == 1;
    let net = cursor.clone();
    sim.fault = Box::new(move |_| {
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
    let mut checked = 0;
    for _ in 0..300 {
        if cursor.borrow().done() {
            break;
        }
        let op = cursor.borrow_mut().byte();
        match op % 32 {
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
            5 => {
                let ticks = u64::from(cursor.borrow_mut().byte());
                sim.run_for(ms(ticks * 8));
            }
            _ => sim.step(),
        }
        let commits = sim.commits();
        for event in &sim.events[checked..] {
            if let Event::IpadLive { session_id, .. } = event {
                assert!(commits.contains(session_id), "live on an uncommitted session");
            }
        }
        checked = sim.events.len();
    }
    // Let everything in flight settle, then check once more.
    sim.run_for(ms(6000));
    let commits = sim.commits();
    for event in &sim.events[checked..] {
        if let Event::IpadLive { session_id, .. } = event {
            assert!(commits.contains(session_id), "live on an uncommitted session");
        }
    }
});
