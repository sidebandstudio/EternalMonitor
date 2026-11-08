//! A v3 host for live interop tests: the reference session table
//! (`HostSessions`) on a real UDP socket on loopback, so another
//! implementation of the iPad side (the Swift client, phase 3) can be run
//! against it end to end.
//!
//! `cargo run -p eternal-wire --features test-hooks --example v3_interop_host`
//!
//! It makes a fresh `host_id` and two credentials, prints one JSON line with
//! the port and the test keys, and serves until it's told to quit, nothing
//! arrives for 30 s, or 5 minutes pass:
//!
//! - key `a` commits like any valid record;
//! - key `b`'s Finalize always conflicts: its confirm gets `SessionRefused`
//!   with `b` tombstoned, through `prepare_refusal` and `refuse`.
//!
//! Every committed session gets a sealed heartbeat every 250 ms. An
//! application record from the iPad is sealed back to it unchanged, except
//! inner type [`QUERY`]: body `summary` is answered with a JSON summary of
//! what the host saw, and `quit` with the summary before it exits.
//!
//! Test keys only, printed in the clear. Never point a real iPad at it.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eternal_wire::v2::PacketType;
use eternal_wire::v3::host::{CloseReason, HostConfig, HostCredentials, HostOutput, HostSessions};
use eternal_wire::v3::wire::{Accept, RecordState, SlotStatus};
use eternal_wire::v3::{Id16, Key32, Random, SystemRandom, Zeroizing};

/// Inner type of the harness's own query records. Below 0x40, so the
/// session tables treat it as an application message.
const QUERY: u8 = 0x3F;

struct Store {
    keys: HashMap<Id16, Key32>,
    a: Id16,
    b: Id16,
    timestamps: RefCell<HashMap<Id16, u64>>,
    retired: RefCell<Vec<Id16>>,
}

impl HostCredentials for Store {
    fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        if self.retired.borrow().contains(dev_id) {
            return None;
        }
        self.keys.get(dev_id).copied().map(Zeroizing::new)
    }

    fn last_committed_timestamp(&self, dev_id: &Id16) -> u64 {
        self.timestamps.borrow().get(dev_id).copied().unwrap_or(0)
    }

    fn accept_template(&self, dev_id: &Id16) -> Accept {
        Accept {
            heartbeat_interval_ms: 250,
            report_interval_ms: 500,
            liveness_timeout_ms: 3000,
            host_caps: 0x003F,
            host_name: "INTEROP-HOST".into(),
            slot_status: self.slot_status(dev_id),
            ..Default::default()
        }
    }

    fn slot_status(&self, _dev_id: &Id16) -> SlotStatus {
        let retired = self.retired.borrow();
        let mut held = Vec::new();
        if !retired.contains(&self.a) {
            held.push((self.a, RecordState::Active));
        }
        if !retired.contains(&self.b) {
            held.push((self.b, RecordState::Pending));
        }
        SlotStatus {
            held,
            tombstones: retired.clone(),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn wall_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

#[derive(Default)]
struct Counts {
    commits: u64,
    acks: u64,
    refused: u64,
    delivered: u64,
    closed: BTreeMap<&'static str, u64>,
}

fn close_name(reason: CloseReason) -> &'static str {
    match reason {
        CloseReason::Superseded => "superseded",
        CloseReason::Revoked => "revoked",
        CloseReason::Liveness => "liveness",
        CloseReason::Expired => "expired",
        CloseReason::Exhausted => "exhausted",
        CloseReason::PeerBye => "peer_bye",
        CloseReason::Shutdown => "shutdown",
    }
}

fn summary(counts: &Counts, host: &HostSessions<SocketAddr>) -> String {
    let closed: Vec<String> = counts
        .closed
        .iter()
        .map(|(name, n)| format!("\"{name}\":{n}"))
        .collect();
    format!(
        "{{\"commits\":{},\"acks\":{},\"refused\":{},\"delivered\":{},\"handshakes\":{},\"closed\":{{{}}}}}",
        counts.commits,
        counts.acks,
        counts.refused,
        counts.delivered,
        host.handshakes_performed(),
        closed.join(",")
    )
}

fn main() -> std::io::Result<()> {
    let mut rng = SystemRandom::default();
    let mut id = || {
        let mut out = [0u8; 16];
        rng.fill(&mut out);
        out
    };
    let (host_id, a, b) = (id(), id(), id());
    let mut rng = SystemRandom::default();
    let mut key = || {
        let mut out = [0u8; 32];
        rng.fill(&mut out);
        out
    };
    let store = Store {
        keys: HashMap::from([(a, key()), (b, key())]),
        a,
        b,
        timestamps: RefCell::new(HashMap::new()),
        retired: RefCell::new(Vec::new()),
    };

    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(Duration::from_millis(10)))?;
    println!(
        "{{\"port\":{},\"host_id\":\"{}\",\"a_dev_id\":\"{}\",\"a_k_dev\":\"{}\",\"b_dev_id\":\"{}\",\"b_k_dev\":\"{}\"}}",
        socket.local_addr()?.port(),
        hex(&host_id),
        hex(&a),
        hex(&store.keys[&a]),
        hex(&b),
        hex(&store.keys[&b]),
    );
    std::io::stdout().flush()?;

    let start = Instant::now();
    let mut host = HostSessions::<SocketAddr>::new(
        HostConfig::new(host_id),
        Box::new(SystemRandom::default()),
        start.elapsed(),
        wall_us(),
    );
    let mut counts = Counts::default();
    let mut last_rx = start.elapsed();
    let mut last_heartbeat = start.elapsed();
    let mut quit = false;
    let mut buf = vec![0u8; 2048];

    while !quit {
        let now = start.elapsed();
        if now > Duration::from_secs(300) || now - last_rx > Duration::from_secs(30) {
            eprintln!("v3_interop_host: idle or overtime, exiting");
            break;
        }
        let mut queue = Vec::new();
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                last_rx = now;
                queue.extend(host.handle_datagram(now, &from, &buf[..len], &store));
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e),
        }
        queue.extend(host.poll(now));
        if now - last_heartbeat >= Duration::from_millis(250) {
            last_heartbeat = now;
            for dev_id in [a, b] {
                if let Some((to, record)) =
                    host.seal(now, &dev_id, PacketType::Heartbeat as u8, &[0; 8])
                {
                    socket.send_to(&record, to)?;
                }
            }
        }

        while !queue.is_empty() {
            let mut next = Vec::new();
            for output in queue.drain(..) {
                match output {
                    HostOutput::Send { to, datagram } => {
                        socket.send_to(&datagram, to)?;
                    }
                    HostOutput::ConfirmRequested { dev_id, session_id } => {
                        if dev_id == b {
                            if host.prepare_refusal(now, &dev_id, session_id) {
                                store.retired.borrow_mut().push(dev_id);
                                counts.refused += 1;
                                next.extend(host.refuse(now, &dev_id, session_id, &store));
                            }
                        } else {
                            next.extend(host.commit(now, &dev_id, session_id));
                        }
                    }
                    HostOutput::Committed {
                        dev_id,
                        session_id,
                        timestamp_us,
                        ..
                    } => {
                        counts.commits += 1;
                        store.timestamps.borrow_mut().insert(dev_id, timestamp_us);
                        let acked = host.ack(now, &dev_id, session_id, &store);
                        counts.acks += u64::from(!acked.is_empty());
                        next.extend(acked);
                    }
                    HostOutput::Deliver {
                        dev_id,
                        inner_type,
                        body,
                        ..
                    } => {
                        let reply = if inner_type == QUERY {
                            quit |= body == b"quit";
                            summary(&counts, &host).into_bytes()
                        } else {
                            counts.delivered += 1;
                            body
                        };
                        if let Some((to, record)) = host.seal(now, &dev_id, inner_type, &reply) {
                            socket.send_to(&record, to)?;
                        }
                    }
                    HostOutput::Closed { reason, .. } => {
                        *counts.closed.entry(close_name(reason)).or_default() += 1;
                    }
                    HostOutput::ConfirmCancelled { .. } => {}
                }
            }
            queue = next;
        }
    }
    eprintln!("v3_interop_host: {}", summary(&counts, &host));
    Ok(())
}
