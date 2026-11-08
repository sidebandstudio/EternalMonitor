//! Every v3 parser on arbitrary bytes: no panics, and whatever parses
//! re-encodes to something that parses back to the same value.
#![no_main]

use eternal_wire::v3::cookie::{self, CookieReply};
use eternal_wire::v3::pairing::QrPayload;
use eternal_wire::v3::record::RecordHeader;
use eternal_wire::v3::wire::{
    self, HelloPayload, Msg2Payload, PairAccept, PairHello, PairMessage, SessionMessage, SlotStatus,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wire::classify(data);
    let _ = RecordHeader::parse(data);

    if let Ok(reply) = CookieReply::parse(data) {
        assert_eq!(
            reply.encode(),
            data,
            "a cookie reply has exactly one encoding"
        );
        assert!(cookie::is_reply(data));
        assert_eq!(wire::classify(data), wire::V3Kind::Other);
    }
    match cookie::split(data) {
        (canon, Some(mac)) => {
            // Exactly one cookie after the handshake extension, stripped,
            // with only payload_len rewritten.
            let end = canon.len();
            assert_eq!(data.len(), end + 20);
            assert_eq!(canon[..6], data[..6]);
            assert_eq!(canon[8..], data[8..end]);
            assert_eq!(data[end..end + 4], [0xFF, 0x00, 16, 0]);
            assert_eq!(data[end + 4..], mac);
            assert_eq!(cookie::split(&canon).1, None);
        }
        (canon, None) => assert_eq!(&*canon, data),
    }

    if let Ok(msg1) = wire::parse_msg1(data) {
        let port = u16::from_le_bytes([data[22], data[23]]);
        let again = wire::encode_msg1(msg1.client_nonce, port, &msg1.dev_id, msg1.noise);
        assert_eq!(again, data, "message 1 has exactly one encoding");
    }
    if let Ok(pair1) = wire::parse_pair1(data) {
        let port = u16::from_le_bytes([data[22], data[23]]);
        let again = wire::encode_pair1(
            pair1.client_nonce,
            port,
            &pair1.slot_id,
            &pair1.nonce_i,
            pair1.ipad_version,
            pair1.noise,
        );
        assert_eq!(again, data, "pairing message 1 has exactly one encoding");
        assert!(wire::parse_msg1(data).is_err());
    }
    if let Ok(noise) = wire::parse_pair2(data) {
        assert_eq!(
            wire::encode_pair2(noise),
            data,
            "pairing message 2 has exactly one encoding"
        );
    }
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(qr) = QrPayload::parse(text) {
            assert_eq!(*qr.encode(), text, "a QR has exactly one form");
        }
    }
    if let Ok(hello) = PairHello::parse(data) {
        assert_eq!(PairHello::parse(&hello.encode()).unwrap(), hello);
    }
    if let Ok(accept) = PairAccept::parse(data) {
        assert_eq!(PairAccept::parse(&accept.encode()).unwrap(), accept);
    }
    if let Ok(noise) = wire::parse_msg2(data) {
        assert_eq!(
            wire::encode_msg2(noise),
            data,
            "message 2 has exactly one encoding"
        );
    }
    if let Ok(hello) = HelloPayload::parse(data) {
        assert_eq!(HelloPayload::parse(&hello.encode()).unwrap(), hello);
    }
    if let Ok(reply) = Msg2Payload::parse(data) {
        assert_eq!(Msg2Payload::parse(&reply.encode()).unwrap(), reply);
    }
    if let Ok(status) = SlotStatus::parse(data) {
        let mut bytes = Vec::new();
        status.encode_into(&mut bytes);
        assert!(bytes.len() <= wire::MAX_SLOT_STATUS_SIZE);
        assert_eq!(SlotStatus::parse(&bytes).unwrap(), status);
    }
    if let Some((&inner, body)) = data.split_first() {
        if let Ok(Some(message)) = SessionMessage::parse(inner, body) {
            assert_eq!(message.inner_type(), inner);
            let again = SessionMessage::parse(inner, &message.encode_body()).unwrap();
            assert_eq!(again, Some(message));
        }
        if let Ok(Some(message)) = PairMessage::parse(inner, body) {
            assert_eq!(message.inner_type(), inner);
            let again = PairMessage::parse(inner, &message.encode_body()).unwrap();
            assert_eq!(again, Some(message));
        }
    }
});
