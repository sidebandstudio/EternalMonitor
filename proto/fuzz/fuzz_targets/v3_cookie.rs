//! The return-routability cookie on both handshakes, with cookies required:
//! a genuine iPad at one address, and fuzzer-chosen replays, bit flips and
//! forgeries of what it and the host sent, from its address or others,
//! across the cookie secret's rotations.
//!
//! What must hold:
//! - no message 1 from an address other than the iPad's is read or
//!   answered with anything but a cookie reply, and a reply is smaller than
//!   what it answers;
//! - message 1 for an unknown `dev_id` gets nothing at all;
//! - the iPad takes a cookie (and resends) only from a reply the host
//!   produced, byte for byte, for the message 1 it's sending;
//! - nothing panics.
#![no_main]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use eternal_wire::v3::client::{Client, ClientConfig, ClientKey, ClientOutput};
use eternal_wire::v3::cookie::{self, CookieMode};
use eternal_wire::v3::handshake::Initiator;
use eternal_wire::v3::host::{HostConfig, HostCredentials, HostOutput, HostSessions};
use eternal_wire::v3::pairing::{
    PairCredentials, PairOutput, PairingClient, PairingConfig, PairingHost,
};
use eternal_wire::v3::wire::{self, Accept, HelloPayload, SlotStatus, V3Kind};
use eternal_wire::v3::{Id16, Key32, SeededRandom, Zeroizing};
use libfuzzer_sys::fuzz_target;

const HOST: Id16 = [0xB0; 16];
const DEV: Id16 = [0xD0; 16];
const K: Key32 = [0x42; 32];
const IPAD: u32 = 1;
const WALL: u64 = 1_762_000_000_000_000;

struct Creds;

impl HostCredentials for Creds {
    fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        (*dev_id == DEV).then(|| Zeroizing::new(K))
    }
    fn last_committed_timestamp(&self, _: &Id16) -> u64 {
        0
    }
    fn accept_template(&self, _: &Id16) -> Accept {
        Accept::default()
    }
    fn slot_status(&self, _: &Id16) -> SlotStatus {
        SlotStatus::default()
    }
}

impl PairCredentials for Creds {
    fn active_in_slot(&self, _: &Id16) -> Option<(Id16, Zeroizing<Key32>)> {
        None
    }
    fn slot_status(&self, _: &Id16) -> SlotStatus {
        SlotStatus::default()
    }
}

fn source(b: u8) -> u32 {
    if b & 0x80 == 0 {
        IPAD
    } else {
        2 + u32::from(b & 0x7F)
    }
}

fn flip(datagram: &mut [u8], b: u8) {
    if !datagram.is_empty() {
        let at = usize::from(b) * 7 % datagram.len();
        datagram[at] ^= 1 << (b % 8);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, data)) = data.split_first() else {
        return;
    };
    if mode & 1 == 0 {
        session(mode, data);
    } else {
        pairing(mode, data);
    }
});

fn session(mode: u8, data: &[u8]) {
    let config = HostConfig {
        cookies: CookieMode::Always,
        ..HostConfig::new(HOST)
    };
    let mut now = Duration::from_secs(100);
    let mut host: HostSessions<u32> = HostSessions::new(
        config,
        Box::new(SeededRandom::new(u64::from(mode))),
        now,
        WALL,
    );
    let creds = Creds;
    let mut ipad = Client::new(
        ClientConfig::new(HOST),
        HelloPayload::default(),
        Box::new(SeededRandom::new(7)),
    );
    let key = ClientKey {
        dev_id: DEV,
        k_dev: K,
    };
    let clock = |now: Duration| WALL + u64::try_from(now.as_micros()).unwrap();
    // What the iPad sent, and what the host sent it (with how often).
    let mut from_ipad: Vec<Vec<u8>> = Vec::new();
    let mut to_ipad: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut inbox: Vec<Vec<u8>> = Vec::new();

    let to_host = |host: &mut HostSessions<u32>,
                   now: Duration,
                   from: u32,
                   datagram: &[u8],
                   to_ipad: &mut HashMap<Vec<u8>, u32>,
                   inbox: &mut Vec<Vec<u8>>| {
        let (reads, dh) = (host.msg1_reads(), host.handshakes_performed());
        let out = host.handle_datagram(now, &from, datagram, &creds);
        let foreign_msg1 = from != IPAD && wire::classify(datagram) == V3Kind::Handshake1;
        if from != IPAD {
            assert_eq!(host.msg1_reads(), reads, "a read for another address");
            assert_eq!(host.handshakes_performed(), dh);
        }
        for o in out {
            if let HostOutput::Send { to, datagram: d } = o {
                if foreign_msg1 || cookie::is_reply(&d) {
                    assert!(cookie::is_reply(&d), "only a cookie reply");
                    assert!(d.len() < datagram.len(), "smaller than its message 1");
                }
                if to == IPAD {
                    *to_ipad.entry(d.clone()).or_default() += 1;
                    inbox.push(d);
                }
            }
        }
    };

    for chunk in data.chunks_exact(3).take(96) {
        let (op, a, b) = (chunk[0], chunk[1], chunk[2]);
        now += Duration::from_millis(u64::from(a) * 3);
        let mut client_out = Vec::new();
        match op % 8 {
            0 => client_out = ipad.connect(now, key.clone(), clock(now)),
            1 => client_out = ipad.poll(now, clock(now)),
            2 => {
                // The host's datagrams reach the iPad, maybe flipped.
                for mut d in std::mem::take(&mut inbox) {
                    if b & 1 == 1 {
                        flip(&mut d, a);
                    }
                    let genuine = to_ipad.contains_key(&d);
                    let out = ipad.handle_datagram(now, &d, clock(now));
                    if cookie::is_reply(&d) && !out.is_empty() {
                        assert!(genuine, "a cookie from a reply the host never sent");
                    }
                    client_out.extend(out);
                }
            }
            3 | 4 => {
                // A replay or flip of the iPad's datagrams from some address.
                if !from_ipad.is_empty() {
                    let mut d = from_ipad[usize::from(a) % from_ipad.len()].clone();
                    if op % 8 == 4 {
                        flip(&mut d, b);
                    }
                    to_host(&mut host, now, source(b), &d, &mut to_ipad, &mut inbox);
                }
            }
            5 => {
                // A forged message 1, for the iPad's dev_id or an unknown one.
                let dev_id = if b & 1 == 0 { DEV } else { [b; 16] };
                let hello = HelloPayload {
                    timestamp_us: u64::from(a) + 1,
                    ..Default::default()
                };
                let forged = Initiator::start(&[a; 32], &dev_id, &HOST, 1, &hello).unwrap();
                let before = host.cookie_replies();
                to_host(
                    &mut host,
                    now,
                    source(b),
                    forged.msg1(),
                    &mut to_ipad,
                    &mut inbox,
                );
                if dev_id != DEV {
                    assert_eq!(
                        host.cookie_replies(),
                        before,
                        "a reply for an unknown dev_id"
                    );
                }
            }
            6 => {
                // Long enough to rotate the secret once or twice.
                now += Duration::from_secs(u64::from(b));
                host.poll(now);
            }
            _ => {
                // A host reply, maybe flipped, back at the host.
                if let Some(d) = to_ipad.keys().nth(usize::from(a) % to_ipad.len().max(1)) {
                    let mut d = d.clone();
                    flip(&mut d, b);
                    to_host(&mut host, now, source(b), &d, &mut to_ipad, &mut inbox);
                }
            }
        }
        for output in client_out {
            if let ClientOutput::Send(d) = output {
                from_ipad.push(d.clone());
                to_host(&mut host, now, IPAD, &d, &mut to_ipad, &mut inbox);
            }
        }
    }
}

fn pairing(mode: u8, data: &[u8]) {
    let config = PairingConfig {
        cookies: CookieMode::Always,
        ..PairingConfig::default()
    };
    let mut host: PairingHost<u32> = PairingHost::new(
        config,
        HOST,
        "0.4.0",
        "PC",
        Box::new(SeededRandom::new(u64::from(mode))),
    );
    let creds = Creds;
    let mut now = Duration::from_secs(100);
    let qr = host.open(now, SocketAddr::from(([10, 0, 0, 1], 47_700)));
    let mut rng = SeededRandom::new(9);
    let mut ipad =
        PairingClient::start(&qr, &[0x5A; 16], "0.4.0", "iPad", 50_000, &mut rng).unwrap();
    let mut seen: Vec<Vec<u8>> = vec![ipad.msg1().to_vec()];
    let mut replies: Vec<Vec<u8>> = Vec::new();

    for chunk in data.chunks_exact(3).take(96) {
        let (op, a, b) = (chunk[0], chunk[1], chunk[2]);
        now += Duration::from_millis(u64::from(a) * 3);
        let (from, mut datagram) = match op % 5 {
            0 => (IPAD, ipad.msg1().to_vec()),
            1 | 2 => {
                let mut d = seen[usize::from(a) % seen.len()].clone();
                if op % 5 == 2 {
                    flip(&mut d, b);
                }
                (source(b), d)
            }
            3 => {
                // A cookie reply, maybe flipped, to the iPad.
                if let Some(reply) = replies.get(usize::from(a) % replies.len().max(1)) {
                    let mut d = reply.clone();
                    if b & 1 == 1 {
                        flip(&mut d, a);
                    }
                    let genuine = replies.contains(&d);
                    if let Some(msg1) = ipad.handle_cookie(&d) {
                        assert!(genuine, "a cookie from a reply the host never sent");
                        seen.push(msg1.clone());
                        (IPAD, msg1)
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            _ => {
                now += Duration::from_secs(u64::from(b));
                host.poll(now);
                continue;
            }
        };
        if op % 5 == 0 && b & 1 == 1 {
            flip(&mut datagram, a);
        }
        let in_flight = host.in_flight(now);
        for output in host.handle_datagram(now, &from, &datagram, &creds) {
            match output {
                PairOutput::Send { to, datagram: d } => {
                    if from != IPAD && wire::classify(&datagram) == V3Kind::Handshake1 {
                        assert!(cookie::is_reply(&d), "only a cookie reply");
                    }
                    if cookie::is_reply(&d) {
                        assert!(d.len() < datagram.len());
                        if to == IPAD {
                            replies.push(d);
                        }
                    }
                }
                PairOutput::Store(_) | PairOutput::Paired { .. } => {
                    panic!("no confirm is ever sent here")
                }
            }
        }
        if from != IPAD {
            assert_eq!(
                host.in_flight(now),
                in_flight,
                "another address started a pairing"
            );
        }
    }
}
