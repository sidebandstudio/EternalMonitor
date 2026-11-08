//! Golden vectors for the iPad side: `testdata/v3/client_golden.txt`.
//!
//! `session_golden.txt` pins one accepted handshake and its records. This
//! file adds what the iPad client must also agree on byte for byte with the
//! Rust reference (`client.rs`), and which the Swift client (phase 3) checks:
//!
//! - the other message 2 answers (`TooNew`, `PairAgain`) to the same
//!   message 1, and an `Accept` at its largest (a 64-byte host name cut at a
//!   character boundary, a full slot status), with records sealed under it;
//! - message 2s whose Noise read authenticates but whose payload doesn't
//!   parse (design gap 7): the attempt is over, and the genuine message 2
//!   that arrives next is refused too;
//! - name truncation, slot status encodings, and payloads every parser must
//!   reject;
//! - a long replay window script.
//!
//! Regenerate only on a deliberate wire change:
//! `UPDATE_GOLDEN=1 cargo test -p eternal-wire --lib v3::golden_client`.

use super::golden::{check, pattern};
use super::handshake::{Initiator, Responder};
use super::replay::ReplayWindow;
use super::wire::{
    self, Accept, HelloPayload, Msg2Payload, RecordState, SessionMessage, SlotStatus,
};
use super::{session_prologue, Key32, V3Error, NOISE_PARAMS};
use crate::v2::control::StreamConfig;
use crate::v2::PacketType;

/// The same fixed inputs as `session_golden.txt`, so message 1 is the same.
struct Inputs {
    k_dev: Key32,
    dev_id: [u8; 16],
    host_id: [u8; 16],
    init_ephemeral: Key32,
    client_nonce: u32,
    hello: HelloPayload,
}

fn inputs() -> Inputs {
    Inputs {
        k_dev: pattern(0x10, 7),
        dev_id: pattern(0xD0, 3),
        host_id: pattern(0xB0, 5),
        init_ephemeral: pattern(0x21, 11),
        client_nonce: 0xA1B2_C3D4,
        hello: HelloPayload {
            timestamp_us: 1_762_500_000_123_456,
            listen_port: 50_123,
            decoder_caps: 0x0007,
            feature_caps: 0x0017,
            screen_px_w: 2732,
            screen_px_h: 2048,
            screen_pt_w: 1366,
            screen_pt_h: 1024,
            refresh_hz: 120,
            preferred_fps: 120,
            device_id: 0x0102_0304_0506_0708,
            device_name: "Test iPad".into(),
        },
    }
}

fn initiator(i: &Inputs) -> Initiator {
    Initiator::start_with_ephemeral(
        &i.k_dev,
        &i.dev_id,
        &i.host_id,
        i.client_nonce,
        &i.hello,
        &i.init_ephemeral,
    )
    .unwrap()
}

fn responder(i: &Inputs, msg1: &[u8], ephemeral: &Key32) -> Responder {
    let parsed = wire::parse_msg1(msg1).unwrap();
    Responder::read_msg1_with_ephemeral(&parsed, &i.k_dev, &i.host_id, ephemeral).unwrap()
}

/// Message 2 carrying arbitrary payload bytes, written with the same snow
/// configuration as the handshake. Only a holder of `K_dev` can make one.
fn raw_msg2(i: &Inputs, msg1: &[u8], ephemeral: &Key32, payload: &[u8]) -> Vec<u8> {
    let parsed = wire::parse_msg1(msg1).unwrap();
    let prologue = session_prologue(&i.dev_id, &i.host_id);
    let mut state = snow::Builder::new(NOISE_PARAMS.parse().unwrap())
        .psk(0, &i.k_dev)
        .unwrap()
        .prologue(&prologue)
        .unwrap()
        .fixed_ephemeral_key_for_testing_only(ephemeral)
        .build_responder()
        .unwrap();
    let mut buf = [0u8; 1024];
    state.read_message(parsed.noise, &mut buf).unwrap();
    let len = state.write_message(payload, &mut buf).unwrap();
    wire::encode_msg2(&buf[..len])
}

fn full_status() -> SlotStatus {
    SlotStatus {
        held: vec![
            (pattern(0x01, 1), RecordState::Pending),
            (pattern(0x11, 1), RecordState::Active),
            (pattern(0x21, 1), RecordState::Pending),
            (pattern(0x31, 1), RecordState::Pending),
        ],
        tombstones: (0..8u8).map(|t| pattern(0x80 + t * 0x10, 1)).collect(),
    }
}

/// 63 ASCII bytes, then a 2-byte character across the 64-byte limit.
fn long_host_name() -> String {
    format!("{}éx", "h".repeat(63))
}

/// 62 ASCII bytes, then a 3-byte character across the 64-byte limit.
fn long_device_name() -> String {
    format!("{}日本", "n".repeat(62))
}

fn status_bytes(status: &SlotStatus) -> Vec<u8> {
    let mut out = Vec::new();
    status.encode_into(&mut out);
    out
}

fn client_transcript() -> Vec<(&'static str, Vec<u8>)> {
    let i = inputs();
    let msg1 = initiator(&i).msg1().to_vec();
    let mut out: Vec<(&'static str, Vec<u8>)> = Vec::new();

    // The other answers to the same message 1.
    let too_new_ephemeral: Key32 = pattern(0x87, 19);
    let too_new = Msg2Payload::TooNew {
        limit_us: 1_762_500_000_000_000,
    };
    let (msg2_too_new, keys) = responder(&i, &msg1, &too_new_ephemeral)
        .write_msg2(&too_new)
        .unwrap();
    assert!(keys.is_none());
    let (reply, keys) = initiator(&i).read_msg2(&msg2_too_new).unwrap();
    assert_eq!((reply, keys.is_none()), (too_new.clone(), true));
    out.push(("input.too_new_ephemeral", too_new_ephemeral.to_vec()));
    out.push(("input.too_new_payload", too_new.encode()));
    out.push(("msg2_too_new", msg2_too_new));

    let pair_again_ephemeral: Key32 = pattern(0x99, 23);
    let (msg2_pair_again, keys) = responder(&i, &msg1, &pair_again_ephemeral)
        .write_msg2(&Msg2Payload::PairAgain)
        .unwrap();
    assert!(keys.is_none());
    out.push(("input.pair_again_ephemeral", pair_again_ephemeral.to_vec()));
    out.push(("input.pair_again_payload", Msg2Payload::PairAgain.encode()));
    out.push(("msg2_pair_again", msg2_pair_again));

    // The largest Accept, and records under its session.
    let max_ephemeral: Key32 = pattern(0x31, 37);
    let session_id = 0xFEDC_BA98u32;
    let accept = Msg2Payload::Accept(Accept {
        session_id,
        heartbeat_interval_ms: 1000,
        report_interval_ms: 500,
        liveness_timeout_ms: 3000,
        stream_config: StreamConfig {
            stream_epoch: 0xFFFF_FFFF,
            width: 2732,
            height: 2048,
            fps: 120,
            codec: 2,
            flags: 0x81,
            bitrate_bps: 80_000_000,
        },
        host_caps: 0xFFFF,
        host_name: long_host_name(),
        slot_status: full_status(),
    });
    let accept_bytes = accept.encode();
    let (msg2_max, host_keys) = responder(&i, &msg1, &max_ephemeral)
        .write_msg2(&accept)
        .unwrap();
    assert!(msg2_max.len() <= 365, "{}", msg2_max.len());
    let mut ipad = initiator(&i);
    let (reply, ipad_keys) = ipad.read_msg2(&msg2_max).unwrap();
    let Msg2Payload::Accept(parsed) = &reply else {
        panic!("not an accept")
    };
    assert_eq!(
        parsed.host_name,
        "h".repeat(63),
        "cut before the 2-byte character"
    );
    let ipad_keys = ipad_keys.unwrap();
    out.push(("input.accept_max_ephemeral", max_ephemeral.to_vec()));
    out.push(("input.accept_max_host_name", long_host_name().into_bytes()));
    out.push(("input.accept_max_payload", accept_bytes.clone()));
    out.push(("msg2_accept_max", msg2_max));
    out.push((
        "accept_max.handshake_hash",
        ipad_keys.handshake_hash.to_vec(),
    ));
    out.push((
        "accept_max.key.initiator_to_responder",
        ipad_keys.initiator_to_responder.to_vec(),
    ));
    out.push((
        "accept_max.key.responder_to_initiator",
        ipad_keys.responder_to_initiator.to_vec(),
    ));
    let mut trailing = accept_bytes.clone();
    trailing.extend_from_slice(&[0xEE, 0x01, 0x02]);
    assert_eq!(Msg2Payload::parse(&trailing).unwrap(), reply);
    out.push(("accept_max.payload_with_trailing_bytes", trailing));

    let (mut ipad_send, ipad_recv) = ipad_keys.initiator(session_id);
    let (mut host_send, _) = host_keys.unwrap().responder(session_id);
    let refused = SessionMessage::Refused(full_status());
    let bye = SessionMessage::Bye(2);
    let heartbeat_body = 0x1122_3344_5566_7788u64.to_le_bytes();
    let confirm = SessionMessage::Confirm;
    let sealed = [
        (
            "accept_max.record.refused",
            host_send
                .seal(refused.inner_type(), &refused.encode_body())
                .unwrap(),
        ),
        (
            "accept_max.record.bye",
            host_send
                .seal(bye.inner_type(), &bye.encode_body())
                .unwrap(),
        ),
        (
            "accept_max.record.heartbeat",
            host_send
                .seal(PacketType::Heartbeat as u8, &heartbeat_body)
                .unwrap(),
        ),
    ];
    for (name, record) in &sealed {
        assert!(ipad_recv.open(record).is_ok(), "{name}");
    }
    out.extend(sealed);
    out.push((
        "accept_max.record.confirm",
        ipad_send
            .seal(confirm.inner_type(), &confirm.encode_body())
            .unwrap(),
    ));
    out.push(("input.heartbeat_body", heartbeat_body.to_vec()));

    // Gap 7: authenticated, but the payload doesn't parse. The attempt is
    // over; the genuine message 2 that follows is refused as well.
    let genuine = responder(&i, &msg1, &pattern(0x43, 13))
        .write_msg2(&Msg2Payload::Stale {
            stored_timestamp_us: 1,
        })
        .unwrap()
        .0;
    let mut accept_session_zero = accept_bytes.clone();
    accept_session_zero[1..5].fill(0);
    for (name, ephemeral, payload) in [
        (
            "msg2_unparseable.unknown_kind",
            pattern::<32>(0xAB, 29),
            vec![0x09u8],
        ),
        (
            "msg2_unparseable.accept_session_zero",
            pattern(0xBC, 31),
            accept_session_zero,
        ),
        (
            "msg2_unparseable.stale_truncated",
            pattern(0xCD, 41),
            vec![0x01, 0, 0, 0],
        ),
    ] {
        let msg2 = raw_msg2(&i, &msg1, &ephemeral, &payload);
        let mut ipad = initiator(&i);
        assert!(
            matches!(ipad.read_msg2(&msg2), Err(V3Error::Wire(_))),
            "{name}"
        );
        assert_eq!(
            ipad.read_msg2(&genuine).unwrap_err(),
            V3Error::Handshake,
            "{name}: the attempt is over"
        );
        out.push((name, msg2));
    }
    out.push(("msg2_unparseable.genuine_after", genuine));

    // Encodings.
    let mut hello = i.hello.clone();
    hello.device_name = long_device_name();
    let encoded = hello.encode();
    assert_eq!(
        HelloPayload::parse(&encoded).unwrap().device_name,
        "n".repeat(62)
    );
    out.push(("input.hello_long_name", long_device_name().into_bytes()));
    out.push(("hello_long_name.payload", encoded));
    out.push(("status.full", status_bytes(&full_status())));
    out.push(("status.empty", status_bytes(&SlotStatus::default())));

    out.extend(rejects());
    out.extend(replay_long());
    out
}

/// Payloads every parser must reject. `reject_parse.session.*` start with the
/// inner type; the rest is the body.
fn rejects() -> Vec<(&'static str, Vec<u8>)> {
    let id = |b: u8| [b; 16];
    let cat = |parts: &[&[u8]]| parts.concat();
    let accept_head = |name: &[u8]| {
        let mut out = vec![0u8];
        out.extend_from_slice(&7u32.to_le_bytes());
        out.extend_from_slice(&[0; 6]); // intervals
        out.extend_from_slice(&[0; 16]); // stream config
        out.extend_from_slice(&[0; 2]); // host caps
        out.extend_from_slice(name);
        out
    };
    let cases: Vec<(&'static str, &'static str, Vec<u8>)> = vec![
        ("status", "reject_parse.status.empty", vec![]),
        ("status", "reject_parse.status.held_5", vec![5]),
        (
            "status",
            "reject_parse.status.state_2",
            cat(&[&[1], &id(1), &[2], &[0]]),
        ),
        (
            "status",
            "reject_parse.status.two_active",
            cat(&[&[2], &id(1), &[1], &id(2), &[1], &[0]]),
        ),
        (
            "status",
            "reject_parse.status.held_duplicate",
            cat(&[&[2], &id(1), &[0], &id(1), &[1], &[0]]),
        ),
        (
            "status",
            "reject_parse.status.held_and_tombstone",
            cat(&[&[1], &id(1), &[0], &[1], &id(1)]),
        ),
        (
            "status",
            "reject_parse.status.tombstone_duplicate",
            cat(&[&[0], &[2], &id(3), &id(3)]),
        ),
        ("status", "reject_parse.status.tombstones_9", vec![0, 9]),
        (
            "status",
            "reject_parse.status.truncated_tombstone",
            cat(&[&[0], &[1], &[3; 15]]),
        ),
        ("status", "reject_parse.status.no_tombstone_count", vec![0]),
        ("msg2", "reject_parse.msg2.empty", vec![]),
        ("msg2", "reject_parse.msg2.kind_4", vec![4]),
        (
            "msg2",
            "reject_parse.msg2.stale_short",
            vec![1, 0, 0, 0, 0, 0, 0, 0],
        ),
        ("msg2", "reject_parse.msg2.too_new_short", vec![3, 1]),
        ("msg2", "reject_parse.msg2.accept_session_zero", {
            let mut b = accept_head(&[0]);
            b[1..5].fill(0);
            b.extend_from_slice(&[0, 0]);
            b
        }),
        (
            "msg2",
            "reject_parse.msg2.accept_name_65",
            cat(&[&accept_head(&[65]), &[b'a'; 65], &[0, 0]]),
        ),
        (
            "msg2",
            "reject_parse.msg2.accept_name_bad_utf8",
            cat(&[&accept_head(&[2, 0xC3, 0x28]), &[0, 0]]),
        ),
        (
            "msg2",
            "reject_parse.msg2.accept_name_overlong",
            cat(&[&accept_head(&[2, 0xC0, 0xAF]), &[0, 0]]),
        ),
        (
            "msg2",
            "reject_parse.msg2.accept_name_surrogate",
            cat(&[&accept_head(&[3, 0xED, 0xA0, 0x80]), &[0, 0]]),
        ),
        (
            "msg2",
            "reject_parse.msg2.accept_name_above_unicode",
            cat(&[&accept_head(&[4, 0xF4, 0x90, 0x80, 0x80]), &[0, 0]]),
        ),
        (
            "msg2",
            "reject_parse.msg2.accept_no_status",
            accept_head(&[0]),
        ),
        (
            "msg2",
            "reject_parse.msg2.accept_bad_status",
            cat(&[&accept_head(&[0]), &[0, 9]]),
        ),
        ("hello", "reject_parse.hello.short", vec![0; 29]),
        (
            "hello",
            "reject_parse.hello.name_65",
            cat(&[&[0; 30], &[65], &[b'a'; 65]]),
        ),
        (
            "hello",
            "reject_parse.hello.name_truncated",
            cat(&[&[0; 30], &[4], b"abc"]),
        ),
        (
            "hello",
            "reject_parse.hello.name_bad_utf8",
            cat(&[&[0; 30], &[1, 0xFF]]),
        ),
        (
            "session",
            "reject_parse.session.confirm_ack_bad_status",
            vec![wire::INNER_SESSION_CONFIRM_ACK, 0, 9],
        ),
        (
            "session",
            "reject_parse.session.refused_empty",
            vec![wire::INNER_SESSION_REFUSED],
        ),
        (
            "session",
            "reject_parse.session.bye_empty",
            vec![wire::INNER_BYE],
        ),
    ];
    let mut out = Vec::new();
    for (kind, name, bytes) in cases {
        let rejected = match kind {
            "status" => SlotStatus::parse(&bytes).is_err(),
            "msg2" => Msg2Payload::parse(&bytes).is_err(),
            "hello" => HelloPayload::parse(&bytes).is_err(),
            "session" => SessionMessage::parse(bytes[0], &bytes[1..]).is_err(),
            _ => unreachable!(),
        };
        assert!(rejected, "{name}");
        out.push((name, bytes));
    }
    out
}

/// 3,000 counters with small reorders, old and very old copies, and jumps
/// across block boundaries, fed to a fresh window's check-and-mark.
fn replay_long() -> Vec<(&'static str, Vec<u8>)> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut window = ReplayWindow::new();
    let mut base = 0u64;
    let mut script = Vec::new();
    let mut accepted = Vec::new();
    for _ in 0..3000 {
        let r = next();
        let counter = match r % 10 {
            0 => base.saturating_sub(r % 5000),
            1 => base + (r % 70_000),
            2 => base.saturating_sub(4096) + (r % 3),
            _ => base + (r % 64),
        };
        base = base.max(counter);
        script.extend_from_slice(&counter.to_le_bytes());
        accepted.push(u8::from(window.check_and_mark(counter)));
    }
    vec![
        ("replay_long.counters_u64le", script),
        ("replay_long.accepted", accepted),
    ]
}

#[test]
fn client_vectors_match_golden_file() {
    check(
        "client_golden.txt",
        "# v3 iPad-side golden vectors. One `name<space>lowercase-hex` per line.\n\
         # The handshake inputs (k_dev, dev_id, host_id, init_ephemeral, client_nonce,\n\
         # hello) are session_golden.txt's, so message 1 is its msg1. input.* are\n\
         # fixed inputs; the rest are outputs both sides must reproduce byte for byte.\n\
         # msg2_unparseable.*: Noise authenticates, the payload doesn't parse; the\n\
         # attempt is then over and msg2_unparseable.genuine_after is refused too.\n\
         # reject_parse.<parser>.*: must not parse (session.* start with the inner\n\
         # type). replay_long.*: as replay_vectors.txt.\n\
         # Regenerate only on a deliberate wire change:\n\
         # UPDATE_GOLDEN=1 cargo test -p eternal-wire --lib v3::golden_client\n",
        client_transcript(),
    );
}
