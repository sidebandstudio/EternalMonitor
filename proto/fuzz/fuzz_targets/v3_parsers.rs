//! Every v3 parser on arbitrary bytes: no panics, and whatever parses
//! re-encodes to something that parses back to the same value.
#![no_main]

use eternal_wire::v3::record::RecordHeader;
use eternal_wire::v3::wire::{self, HelloPayload, Msg2Payload, SessionMessage, SlotStatus};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wire::classify(data);
    let _ = RecordHeader::parse(data);

    if let Ok(msg1) = wire::parse_msg1(data) {
        let port = u16::from_le_bytes([data[22], data[23]]);
        let again = wire::encode_msg1(msg1.client_nonce, port, &msg1.dev_id, msg1.noise);
        assert_eq!(again, data, "message 1 has exactly one encoding");
    }
    if let Ok(noise) = wire::parse_msg2(data) {
        assert_eq!(wire::encode_msg2(noise), data, "message 2 has exactly one encoding");
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
    }
});
