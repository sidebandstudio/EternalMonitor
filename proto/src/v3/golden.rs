//! Golden transcripts: `testdata/v3/session_golden.txt` and
//! `testdata/v3/replay_vectors.txt`.
//!
//! One reconnect handshake with fixed keys and ephemerals, the records that
//! follow it, a `Stale` answer, inputs that must fail, and a scripted replay
//! window run. The Swift implementation (phase 3) must reproduce every byte.
//! This test asserts the Rust side produces exactly the checked-in files.
//!
//! To regenerate after an intentional wire change:
//! `UPDATE_GOLDEN=1 cargo test -p eternal-wire --lib v3::golden`.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::handshake::{Initiator, Responder};
use super::record::{Opener, RecordHeader, Sealer};
use super::replay::ReplayWindow;
use super::vectors::{hex, load};
use super::wire::{
    self, Accept, HelloPayload, Msg2Payload, RecordState, SessionMessage, SlotStatus,
};
use super::{session_prologue, V3Error, MAX_RECORDS_PER_DIRECTION};
use crate::v2::control::StreamConfig;
use crate::v2::{PacketType, WireError};

pub(super) fn pattern<const N: usize>(start: u8, step: u8) -> [u8; N] {
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = start.wrapping_add(step.wrapping_mul(i as u8));
    }
    out
}

fn session_transcript() -> Vec<(&'static str, Vec<u8>)> {
    let k_dev: [u8; 32] = pattern(0x10, 7);
    let dev_id: [u8; 16] = pattern(0xD0, 3);
    let host_id: [u8; 16] = pattern(0xB0, 5);
    let init_ephemeral: [u8; 32] = pattern(0x21, 11);
    let resp_ephemeral: [u8; 32] = pattern(0x43, 13);
    let stale_ephemeral: [u8; 32] = pattern(0x65, 17);
    let client_nonce = 0xA1B2_C3D4u32;
    let session_id = 0x1357_9BDFu32;

    let hello = HelloPayload {
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
    };
    let status = SlotStatus {
        held: vec![
            (dev_id, RecordState::Active),
            (pattern(0xE0, 1), RecordState::Pending),
        ],
        tombstones: vec![pattern(0xF0, 1)],
    };
    let accept = Msg2Payload::Accept(Accept {
        session_id,
        heartbeat_interval_ms: 1000,
        report_interval_ms: 500,
        liveness_timeout_ms: 3000,
        stream_config: StreamConfig {
            stream_epoch: 3,
            width: 2560,
            height: 1440,
            fps: 120,
            codec: 1,
            flags: 0,
            bitrate_bps: 50_000_000,
        },
        host_caps: 0x003F,
        host_name: "TEST-PC".into(),
        slot_status: status.clone(),
    });

    let mut out: Vec<(&'static str, Vec<u8>)> = vec![
        ("input.k_dev", k_dev.to_vec()),
        ("input.dev_id", dev_id.to_vec()),
        ("input.host_id", host_id.to_vec()),
        ("input.init_ephemeral", init_ephemeral.to_vec()),
        ("input.resp_ephemeral", resp_ephemeral.to_vec()),
        ("input.client_nonce", client_nonce.to_le_bytes().to_vec()),
        ("input.hello_payload", hello.encode()),
        ("input.accept_payload", accept.encode()),
        ("prologue", session_prologue(&dev_id, &host_id)),
    ];

    let mut ipad = Initiator::start_with_ephemeral(
        &k_dev,
        &dev_id,
        &host_id,
        client_nonce,
        &hello,
        &init_ephemeral,
    )
    .unwrap();
    let msg1 = ipad.msg1().to_vec();
    out.push(("msg1", msg1.clone()));
    let parsed = wire::parse_msg1(&msg1).unwrap();
    let host =
        Responder::read_msg1_with_ephemeral(&parsed, &k_dev, &host_id, &resp_ephemeral).unwrap();
    assert_eq!(host.payload(), &hello);
    let (msg2, host_keys) = host.write_msg2(&accept).unwrap();
    out.push(("msg2", msg2.clone()));
    let (reply, ipad_keys) = ipad.read_msg2(&msg2).unwrap();
    assert_eq!(reply, accept);
    let (host_keys, ipad_keys) = (host_keys.unwrap(), ipad_keys.unwrap());
    assert_eq!(host_keys.handshake_hash, ipad_keys.handshake_hash);
    out.push(("handshake_hash", ipad_keys.handshake_hash.to_vec()));
    out.push((
        "key.initiator_to_responder",
        ipad_keys.initiator_to_responder.to_vec(),
    ));
    out.push((
        "key.responder_to_initiator",
        ipad_keys.responder_to_initiator.to_vec(),
    ));

    let i2r = ipad_keys.initiator_to_responder;
    let r2i = ipad_keys.responder_to_initiator;
    let (mut ipad_send, ipad_recv) = ipad_keys.initiator(session_id);
    let (mut host_send, host_recv) = host_keys.responder(session_id);
    let mut record = |name: &'static str, sealer: &mut Sealer, inner: u8, body: &[u8]| {
        let sealed = sealer.seal(inner, body).unwrap();
        out.push((name, sealed.clone()));
        sealed
    };
    let confirm = SessionMessage::Confirm;
    let confirm_ack = SessionMessage::ConfirmAck(status.clone());
    let r_confirm = record(
        "record.confirm",
        &mut ipad_send,
        confirm.inner_type(),
        &confirm.encode_body(),
    );
    let r_ack = record(
        "record.confirm_ack",
        &mut host_send,
        confirm_ack.inner_type(),
        &confirm_ack.encode_body(),
    );
    let input_body: Vec<u8> = (0..30u8).collect();
    let r_input = record(
        "record.input",
        &mut ipad_send,
        PacketType::InputEvent as u8,
        &input_body,
    );
    let media_body: Vec<u8> = (0..1200u32).map(|i| (i * 31 % 251) as u8).collect();
    let r_media = record(
        "record.media_1200",
        &mut host_send,
        PacketType::Media as u8,
        &media_body,
    );
    let bye = SessionMessage::Bye(0);
    let r_bye = record(
        "record.bye",
        &mut ipad_send,
        bye.inner_type(),
        &bye.encode_body(),
    );

    for (sealed, opener, inner, body) in [
        (
            &r_confirm,
            &host_recv,
            confirm.inner_type(),
            confirm.encode_body(),
        ),
        (
            &r_ack,
            &ipad_recv,
            confirm_ack.inner_type(),
            confirm_ack.encode_body(),
        ),
        (
            &r_input,
            &host_recv,
            PacketType::InputEvent as u8,
            input_body.clone(),
        ),
        (
            &r_media,
            &ipad_recv,
            PacketType::Media as u8,
            media_body.clone(),
        ),
        (&r_bye, &host_recv, bye.inner_type(), bye.encode_body()),
    ] {
        let opened = opener.open(sealed).unwrap();
        assert_eq!((opened.inner_type, opened.body), (inner, body));
        assert_eq!(RecordHeader::parse(sealed).unwrap().session_id, session_id);
    }

    // Records at counters whose encoding crosses a byte boundary, sealed
    // host → iPad. Each opens, with its counter, under a fresh opener.
    let fresh_host = || Opener::new(&i2r, session_id);
    let fresh_ipad = || Opener::new(&r2i, session_id);
    let counter_body = [0xC3u8; 32];
    out.push(("input.counter_inner_type", vec![PacketType::Media as u8]));
    out.push(("input.counter_body", counter_body.to_vec()));
    let mut boundary = Sealer::new(&r2i, session_id);
    for (name, counter) in [
        ("record.counter_255", 255u64),
        ("record.counter_256", 256),
        ("record.counter_65535", 65_535),
        ("record.counter_65536", 65_536),
        ("record.counter_last", MAX_RECORDS_PER_DIRECTION - 1),
    ] {
        boundary.skip_to(counter);
        let sealed = boundary
            .seal(PacketType::Media as u8, &counter_body)
            .unwrap();
        assert_eq!(
            fresh_ipad().open(&sealed).unwrap().counter,
            counter,
            "{name}"
        );
        out.push((name, sealed));
    }

    // Must fail authentication, each under a fresh opener or handshake
    // state, while the original opens.
    let mut bad_msg1 = msg1.clone();
    *bad_msg1.last_mut().unwrap() ^= 0x01;
    let parsed_bad = wire::parse_msg1(&bad_msg1).unwrap();
    assert_eq!(
        Responder::read_msg1(&parsed_bad, &k_dev, &host_id).unwrap_err(),
        V3Error::Auth
    );
    assert!(Responder::read_msg1(&parsed, &k_dev, &host_id).is_ok());
    out.push(("reject_auth.msg1_tag_flipped", bad_msg1));

    let mut bad_msg2 = msg2.clone();
    *bad_msg2.last_mut().unwrap() ^= 0x01;
    let mut again = Initiator::start_with_ephemeral(
        &k_dev,
        &dev_id,
        &host_id,
        client_nonce,
        &hello,
        &init_ephemeral,
    )
    .unwrap();
    assert_eq!(again.read_msg2(&bad_msg2).unwrap_err(), V3Error::Auth);
    assert!(again.read_msg2(&msg2).is_ok());
    out.push(("reject_auth.msg2_tag_flipped", bad_msg2));

    let flipped = |record: &[u8], at: usize| {
        let mut bad = record.to_vec();
        bad[at] ^= 0x01;
        bad
    };
    let body_flipped = flipped(&r_input, 25);
    assert_eq!(fresh_host().open(&body_flipped), Err(V3Error::Auth));
    let counter_flipped = flipped(&r_input, 12);
    assert_eq!(fresh_host().open(&counter_flipped), Err(V3Error::Auth));
    assert!(fresh_host().open(&r_input).is_ok());
    out.push(("reject_auth.record_input_body_flipped", body_flipped));
    out.push(("reject_auth.record_input_counter_flipped", counter_flipped));

    // session_id is in the AAD: even the opener for the flipped id rejects.
    let session_flipped = flipped(&r_media, 8);
    let flipped_id = RecordHeader::parse(&session_flipped).unwrap().session_id;
    assert_eq!(
        Opener::new(&r2i, flipped_id).open(&session_flipped),
        Err(V3Error::Auth)
    );
    assert!(fresh_ipad().open(&r_media).is_ok());
    out.push(("reject_auth.record_media_session_flipped", session_flipped));

    // An iPad record sent back to the iPad: the other direction's key.
    assert_eq!(fresh_ipad().open(&r_input), Err(V3Error::Auth));
    out.push(("reject_auth.record_input_reflected", r_input.clone()));

    // Must not parse.
    let length_flipped = flipped(&r_input, 6);
    assert_eq!(
        RecordHeader::parse(&length_flipped),
        Err(WireError::LengthMismatch)
    );
    let mut flags_set = r_input.clone();
    flags_set[4] = 0x01;
    assert_eq!(
        RecordHeader::parse(&flags_set),
        Err(WireError::InvalidField("flags"))
    );
    out.push(("reject_framing.record_length_flipped", length_flipped));
    out.push(("reject_framing.record_flags_set", flags_set));
    let mut proto_2 = msg1.clone();
    proto_2[16] = 2;
    proto_2[17] = 2;
    let mut trailing = msg1.clone();
    trailing.push(0);
    let mut msg2_session = msg2.clone();
    msg2_session[8] = 0x01;
    let msg2_truncated = msg2[..msg2.len() - 1].to_vec();
    assert!(wire::parse_msg1(&proto_2).is_err());
    assert!(wire::parse_msg1(&trailing).is_err());
    assert!(wire::parse_msg2(&msg2_session).is_err());
    assert!(wire::parse_msg2(&msg2_truncated).is_err());
    out.push(("reject_framing.msg1_protocol_2", proto_2));
    out.push(("reject_framing.msg1_trailing_byte", trailing));
    out.push(("reject_framing.msg2_session_id_set", msg2_session));
    out.push(("reject_framing.msg2_truncated", msg2_truncated));

    // Refused before the AEAD: the counter limit.
    let mut over_limit = r_input.clone();
    over_limit[12..20].copy_from_slice(&MAX_RECORDS_PER_DIRECTION.to_le_bytes());
    assert_eq!(fresh_host().open(&over_limit), Err(V3Error::Replay));
    out.push(("reject_limit.record_counter_2_32", over_limit));

    // The originals were opened once above; a second open is a replay.
    assert_eq!(host_recv.open(&r_input), Err(V3Error::Replay));
    assert_eq!(ipad_recv.open(&r_media), Err(V3Error::Replay));

    // A Stale answer to the same message 1.
    let host =
        Responder::read_msg1_with_ephemeral(&parsed, &k_dev, &host_id, &stale_ephemeral).unwrap();
    let stale = Msg2Payload::Stale {
        stored_timestamp_us: hello.timestamp_us + 1,
    };
    let (msg2_stale, keys) = host.write_msg2(&stale).unwrap();
    assert!(keys.is_none());
    out.push(("input.stale_ephemeral", stale_ephemeral.to_vec()));
    out.push(("input.stale_payload", stale.encode()));
    out.push(("msg2_stale", msg2_stale));
    out
}

/// Counters fed to a fresh window in order, and whether each is accepted.
fn replay_script() -> Vec<(&'static str, Vec<u8>)> {
    let counters: [u64; 20] = [
        0,
        0,
        5,
        3,
        4096,
        1,
        0,
        4097,
        1,
        2,
        10_000,
        5905,
        5904,
        10_000,
        9999,
        74_000,
        69_905,
        69_904,
        (1 << 32) - 1,
        5,
    ];
    let mut window = ReplayWindow::new();
    let mut script = Vec::new();
    let mut expected = Vec::new();
    for counter in counters {
        script.extend_from_slice(&counter.to_le_bytes());
        expected.push(u8::from(window.check_and_mark(counter)));
    }
    vec![
        ("replay.counters_u64le", script),
        ("replay.accepted", expected),
    ]
}

fn render<N: AsRef<str>>(header: &str, entries: &[(N, Vec<u8>)]) -> String {
    let mut text = String::from(header);
    for (name, bytes) in entries {
        writeln!(text, "{} {}", name.as_ref(), hex(bytes)).unwrap();
    }
    text
}

pub(super) fn check<N: AsRef<str>>(file: &str, header: &str, entries: Vec<(N, Vec<u8>)>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/v3")
        .join(file);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, render(header, &entries)).unwrap();
    }
    let on_disk = load(file);
    assert_eq!(on_disk.len(), entries.len(), "{file}: entry count");
    for (name, bytes) in &entries {
        let name = name.as_ref();
        let stored = on_disk
            .get(name)
            .unwrap_or_else(|| panic!("{file}: missing {name}"));
        assert_eq!(hex(stored), hex(bytes), "{file}: {name}");
    }
}

#[test]
fn session_transcript_matches_golden_file() {
    check(
        "session_golden.txt",
        "# v3 reconnect golden transcript. One `name<space>lowercase-hex` per line.\n\
         # input.* are fixed inputs; the rest are outputs both sides must reproduce\n\
         # byte for byte. Integers are little-endian. record.counter_* are host -> iPad\n\
         # records that must open with that counter under the responder->initiator key;\n\
         # each seals input.counter_inner_type || input.counter_body.\n\
         # reject_auth.* must fail authentication under a fresh opener or handshake\n\
         # state for this session (record_media_session_flipped: the opener for the\n\
         # flipped id; record_input_reflected: the responder->initiator key).\n\
         # reject_framing.* must not parse. reject_limit.* must be refused before\n\
         # the AEAD.\n\
         # Regenerate only on a deliberate wire change:\n\
         # UPDATE_GOLDEN=1 cargo test -p eternal-wire --lib v3::golden\n",
        session_transcript(),
    );
}

#[test]
fn replay_script_matches_golden_file() {
    check(
        "replay_vectors.txt",
        "# v3 replay window script: feed each u64 counter, in order, to a fresh\n\
         # 4096-wide window's check-and-mark; `replay.accepted` holds 01 or 00 per\n\
         # counter.\n",
        replay_script(),
    );
}
