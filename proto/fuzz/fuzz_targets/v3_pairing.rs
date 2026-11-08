//! The host's QR pairing state machine against a genuine iPad and
//! fuzzer-chosen events: retries, bit flips and replays of real datagrams,
//! forged message 1s, aborts, store results (written, too many, failed),
//! new QRs, closed windows and time.
//!
//! What must hold:
//! - a record is handed to the store only after a genuine confirm, with the
//!   `dev_id` and key the iPad was given, and at most one store is in flight;
//! - a confirm authorizes a write only before the QR the pairing came from
//!   expires (a write authorized earlier may be retried after it);
//! - a replayed message 1 never replaces a pairing in flight;
//! - `replaces` is set only to the slot's active record, and only when the
//!   iPad proved it holds that record's key;
//! - the iPad hears `PairDone` only after its own pairing's write
//!   succeeded, and `PairRefused` only after its own write was refused;
//! - each written record is reported paired once;
//! - nothing panics.
#![no_main]

use std::net::SocketAddr;
use std::time::Duration;

use eternal_wire::v3::client::ClientKey;
use eternal_wire::v3::pairing::{
    PairCredentials, PairOutput, PairResult, PairingClient, PairingConfig, PairingHost,
    PendingPairing, QrPayload, StoreResult, QR_LIFETIME,
};
use eternal_wire::v3::wire::SlotStatus;
use eternal_wire::v3::{Id16, Key32, SeededRandom, Zeroizing};
use libfuzzer_sys::fuzz_target;

const HOST: Id16 = [0xB0; 16];
const SLOT: Id16 = [0x5A; 16];
const OLD: Id16 = [0x11; 16];
const K_OLD: Key32 = [0x12; 32];

struct Creds {
    /// Whether the slot's active record is `OLD`.
    active: bool,
}

impl PairCredentials for Creds {
    fn active_in_slot(&self, _: &Id16) -> Option<(Id16, Zeroizing<Key32>)> {
        self.active.then(|| (OLD, Zeroizing::new(K_OLD)))
    }
    fn slot_status(&self, _: &Id16) -> SlotStatus {
        SlotStatus::default()
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, data)) = data.split_first() else {
        return;
    };
    let creds = Creds {
        active: mode & 1 == 1,
    };
    let mut host: PairingHost<u32> = PairingHost::new(
        PairingConfig::default(),
        HOST,
        "0.4.0",
        "PC",
        Box::new(SeededRandom::new(u64::from(mode))),
    );
    let addr = SocketAddr::from(([10, 0, 0, 1], 47_700));
    let mut now = Duration::from_secs(10);
    let mut qr: QrPayload = host.open(now, addr);
    let mut qr_expires = now + QR_LIFETIME;
    let mut rng = SeededRandom::new(99);
    let mut ipad: Option<PairingClient> = None;
    // What each pairing gave its iPad in message 2: the key, the expiry of
    // the QR it came from, and whether a confirm authorized its write.
    let mut given: Vec<(Id16, Key32, Duration, bool)> = Vec::new();
    // The record in the store's hands, and what the store said.
    let mut storing: Option<PendingPairing> = None;
    // The dev_ids whose write succeeded, or was refused, and the one the
    // current iPad was given.
    let mut written: Vec<Id16> = Vec::new();
    let mut refused: Vec<Id16> = Vec::new();
    let mut current: Option<Id16> = None;
    let mut paired: Vec<Id16> = Vec::new();
    // Real datagrams seen, for replays and flips.
    let mut seen: Vec<Vec<u8>> = Vec::new();

    let deliver = |host: &mut PairingHost<u32>,
                   now: Duration,
                   datagram: &[u8],
                   ipad: &mut Option<PairingClient>,
                   given: &mut Vec<(Id16, Key32, Duration, bool)>,
                   storing: &mut Option<PendingPairing>,
                   written: &[Id16],
                   refused: &[Id16],
                   current: &mut Option<Id16>,
                   paired: &mut Vec<Id16>,
                   seen: &mut Vec<Vec<u8>>,
                   proved: bool,
                   qr_expires: Duration| {
        for output in host.handle_datagram(now, &1, datagram, &creds) {
            match output {
                PairOutput::Send { datagram, .. } => {
                    seen.push(datagram.clone());
                    if let Some(client) = ipad.as_mut() {
                        if let Ok(accept) = client.read_msg2(&datagram) {
                            given.push((accept.dev_id, *accept.k_new, qr_expires, false));
                            *current = Some(accept.dev_id);
                        }
                        match client.handle_record(&datagram) {
                            Some(PairResult::Done(_)) => {
                                assert!(
                                    current.is_some_and(|d| written.contains(&d)),
                                    "PairDone before its own write"
                                )
                            }
                            Some(PairResult::Refused(_)) => assert!(
                                current.is_some_and(|d| refused.contains(&d)),
                                "a refusal its own write didn't get"
                            ),
                            None => {}
                        }
                    }
                }
                PairOutput::Store(pending) => {
                    assert!(storing.is_none(), "one store at a time");
                    let entry = given
                        .iter_mut()
                        .find(|(dev_id, ..)| *dev_id == pending.dev_id)
                        .expect("a store only for a dev_id given in message 2");
                    assert_eq!(**pending.key, entry.1, "the key the iPad was given");
                    assert!(
                        now < entry.2 || entry.3,
                        "a write authorized after its QR expired"
                    );
                    entry.3 = true;
                    assert_eq!(pending.slot_id, SLOT);
                    if let Some(old) = pending.replaces {
                        assert_eq!(old, OLD);
                        assert!(creds.active && proved, "replaces without a proof");
                    }
                    *storing = Some(pending);
                }
                PairOutput::Paired { dev_id, .. } => {
                    assert!(!paired.contains(&dev_id), "a pairing is reported once");
                    paired.push(dev_id);
                }
            }
        }
    };

    for chunk in data.chunks_exact(3).take(64) {
        let (op, a, b) = (chunk[0], chunk[1], chunk[2]);
        now += Duration::from_millis(u64::from(a) * 4);
        match op % 10 {
            0 => {
                // A fresh pairing from the QR (a new nonce_i).
                let client =
                    PairingClient::start(&qr, &SLOT, "0.4.0", "iPad", 50_000, &mut rng).unwrap();
                let msg1 = client.msg1().to_vec();
                seen.push(msg1.clone());
                ipad = Some(client);
                current = None;
                deliver(
                    &mut host,
                    now,
                    &msg1,
                    &mut ipad,
                    &mut given,
                    &mut storing,
                    &written,
                    &refused,
                    &mut current,
                    &mut paired,
                    &mut seen,
                    false,
                    qr_expires,
                );
            }
            1 => {
                // The same message 1 again.
                if let Some(msg1) = ipad.as_ref().map(|c| c.msg1().to_vec()) {
                    deliver(
                        &mut host,
                        now,
                        &msg1,
                        &mut ipad,
                        &mut given,
                        &mut storing,
                        &written,
                        &refused,
                        &mut current,
                        &mut paired,
                        &mut seen,
                        false,
                        qr_expires,
                    );
                }
            }
            2 | 3 => {
                // A confirm, with a proof (right key, or not) or without.
                let proof = match b % 3 {
                    0 => None,
                    1 => Some(ClientKey {
                        dev_id: OLD,
                        k_dev: K_OLD,
                    }),
                    _ => Some(ClientKey {
                        dev_id: OLD,
                        k_dev: [0x13; 32],
                    }),
                };
                let proved = b % 3 == 1;
                if let Some(record) = ipad.as_mut().and_then(|c| c.confirm(proof.as_ref()).ok()) {
                    seen.push(record.clone());
                    deliver(
                        &mut host,
                        now,
                        &record,
                        &mut ipad,
                        &mut given,
                        &mut storing,
                        &written,
                        &refused,
                        &mut current,
                        &mut paired,
                        &mut seen,
                        proved,
                        qr_expires,
                    );
                }
            }
            4 => {
                if let Some(record) = ipad.as_mut().and_then(|c| c.abort().ok()) {
                    deliver(
                        &mut host,
                        now,
                        &record,
                        &mut ipad,
                        &mut given,
                        &mut storing,
                        &written,
                        &refused,
                        &mut current,
                        &mut paired,
                        &mut seen,
                        false,
                        qr_expires,
                    );
                }
            }
            5 => {
                // The store answers.
                if let Some(pending) = storing.take() {
                    let result = match b % 3 {
                        0 => StoreResult::Stored,
                        1 => StoreResult::TooMany,
                        _ => StoreResult::Failed,
                    };
                    match result {
                        StoreResult::Stored => written.push(pending.dev_id),
                        StoreResult::TooMany => refused.push(pending.dev_id),
                        StoreResult::Failed => {}
                    }
                    for output in host.stored(now, &pending.dev_id, result, &creds) {
                        match output {
                            PairOutput::Send { datagram, .. } => {
                                if let Some(client) = ipad.as_mut() {
                                    match client.handle_record(&datagram) {
                                        Some(PairResult::Done(_)) => {
                                            assert!(current.is_some_and(|d| written.contains(&d)))
                                        }
                                        Some(PairResult::Refused(_)) => {
                                            assert!(current.is_some_and(|d| refused.contains(&d)))
                                        }
                                        None => {}
                                    }
                                }
                                seen.push(datagram);
                            }
                            PairOutput::Paired { dev_id, .. } => {
                                assert!(!paired.contains(&dev_id), "a pairing is reported once");
                                assert_eq!(dev_id, pending.dev_id);
                                paired.push(dev_id);
                            }
                            PairOutput::Store(_) => panic!("stored() never asks to store"),
                        }
                    }
                }
            }
            6 => {
                // A replay or a bit flip of something seen.
                if !seen.is_empty() {
                    let mut datagram = seen[usize::from(a) % seen.len()].clone();
                    if b & 1 == 1 {
                        let at = usize::from(b) % datagram.len();
                        datagram[at] ^= 1 << (b % 8);
                    }
                    let held = host.in_flight(now).then(|| host.session_id());
                    deliver(
                        &mut host,
                        now,
                        &datagram,
                        &mut ipad,
                        &mut given,
                        &mut storing,
                        &written,
                        &refused,
                        &mut current,
                        &mut paired,
                        &mut seen,
                        false,
                        qr_expires,
                    );
                    if let Some(session) = held {
                        assert_eq!(host.session_id(), session, "a replay replaced a pairing");
                    }
                }
            }
            7 => {
                // A forged message 1 under another secret.
                let mut forged = qr.clone();
                forged.secret = Zeroizing::new([b; 32]);
                let client =
                    PairingClient::start(&forged, &SLOT, "0.4.0", "x", 1, &mut rng).unwrap();
                let msg1 = client.msg1().to_vec();
                for output in host.handle_datagram(now, &2, &msg1, &creds) {
                    assert!(!matches!(output, PairOutput::Store(_)));
                    if let PairOutput::Send { .. } = output {
                        assert_eq!(forged.secret, qr.secret, "an answer to a forged message 1");
                    }
                }
            }
            8 => {
                if b & 1 == 0 {
                    qr = host.open(now, addr);
                    qr_expires = now + QR_LIFETIME;
                } else {
                    host.close();
                }
            }
            _ => {
                now += Duration::from_secs(u64::from(b));
                host.poll(now);
            }
        }
    }
});
