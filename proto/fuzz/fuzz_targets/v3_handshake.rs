//! The handshake on both sides: arbitrary message 1 bytes at the responder,
//! arbitrary message 2 bytes at the initiator, and real messages with
//! fuzzer-chosen changes. Changes to anything authenticated must fail; a
//! failed message 2 must leave the initiator able to read the real one.
#![no_main]

use eternal_wire::v3::handshake::{Initiator, Responder};
use eternal_wire::v3::wire::{self, Accept, HelloPayload, Msg2Payload};
use libfuzzer_sys::fuzz_target;

const K: [u8; 32] = [0x31; 32];
const DEV: [u8; 16] = [0xD3; 16];
const HOST: [u8; 16] = [0xB3; 16];

fuzz_target!(|data: &[u8]| {
    if let Ok(msg1) = wire::parse_msg1(data) {
        let _ = Responder::read_msg1(&msg1, &K, &HOST);
    }

    let mut ephemeral_i = [0x11u8; 32];
    let mut ephemeral_r = [0x22u8; 32];
    for (i, byte) in data.iter().take(32).enumerate() {
        ephemeral_i[i] ^= byte;
        ephemeral_r[31 - i] ^= byte.rotate_left(3);
    }
    let name_len = data.first().map_or(0, |b| usize::from(*b) % 65);
    let hello = HelloPayload {
        timestamp_us: data.len() as u64 + 1,
        device_name: "n".repeat(name_len),
        ..Default::default()
    };
    let mut ipad =
        Initiator::start_with_ephemeral(&K, &DEV, &HOST, 7, &hello, &ephemeral_i).unwrap();
    let msg1 = ipad.msg1().to_vec();

    // A changed message 1: either the framing rejects it or the AEAD does,
    // unless only the unauthenticated shell fields changed.
    let flips: Vec<(usize, u8)> = data
        .chunks(3)
        .skip(11)
        .take(4)
        .map(|c| {
            let at = usize::from(u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]));
            (at % msg1.len(), *c.get(2).unwrap_or(&1) | 1)
        })
        .collect();
    let mut bad = msg1.clone();
    for &(at, x) in &flips {
        bad[at] ^= x;
    }
    if bad != msg1 {
        let only_shell = (0..msg1.len())
            .filter(|&i| bad[i] != msg1[i])
            .all(|i| (18..24).contains(&i));
        if let Ok(parsed) = wire::parse_msg1(&bad) {
            let result = Responder::read_msg1(&parsed, &K, &HOST);
            assert_eq!(result.is_ok(), only_shell, "authenticated byte changed but accepted");
        } else {
            assert!(!only_shell, "a nonce or port change must still parse");
        }
    }

    let parsed = wire::parse_msg1(&msg1).unwrap();
    let host = Responder::read_msg1_with_ephemeral(&parsed, &K, &HOST, &ephemeral_r).unwrap();
    assert_eq!(host.payload(), &hello);
    let reply = Msg2Payload::Accept(Accept {
        session_id: 1 + u32::from(data.get(1).copied().unwrap_or(0)),
        ..Default::default()
    });
    let (msg2, host_keys) = host.write_msg2(&reply).unwrap();

    // Arbitrary and changed message 2s first, then the real one.
    assert!(ipad.read_msg2(data).is_err());
    let mut bad2 = msg2.clone();
    for &(at, x) in &flips {
        bad2[at % msg2.len()] ^= x;
    }
    if bad2 != msg2 {
        assert!(ipad.read_msg2(&bad2).is_err(), "changed message 2 accepted");
    }
    let (got, ipad_keys) = ipad.read_msg2(&msg2).expect("real message 2 still reads");
    assert_eq!(got, reply);
    let (host_keys, ipad_keys) = (host_keys.unwrap(), ipad_keys.unwrap());
    assert_eq!(host_keys.handshake_hash(), ipad_keys.handshake_hash());
    let (mut ipad_send, ipad_recv) = ipad_keys.initiator(9);
    let (mut host_send, host_recv) = host_keys.responder(9);
    let up = ipad_send.seal(0x20, data).unwrap();
    let down = host_send.seal(0x01, data).unwrap();
    assert_eq!(host_recv.open(&up).unwrap().body, data);
    assert_eq!(ipad_recv.open(&down).unwrap().body, data);
    assert!(ipad_recv.open(&up).is_err(), "directions use different keys");
});
