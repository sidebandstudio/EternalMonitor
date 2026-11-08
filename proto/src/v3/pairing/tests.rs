use super::*;
use crate::v3::wire::RecordState;
use crate::v3::SeededRandom;
use std::collections::HashMap;

const HOST: Id16 = [0xB0; 16];
const SLOT: Id16 = [0x5A; 16];
const T0: Duration = Duration::from_secs(100);
const ADDR: u32 = 1;

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn unhex_vec(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// The slot's active record, if any, and statuses by `dev_id`.
#[derive(Default)]
struct Creds {
    active: Option<(Id16, Key32)>,
    statuses: HashMap<Id16, SlotStatus>,
}

impl PairCredentials for Creds {
    fn active_in_slot(&self, slot_id: &Id16) -> Option<(Id16, Zeroizing<Key32>)> {
        assert_eq!(*slot_id, SLOT);
        self.active.map(|(id, k)| (id, Zeroizing::new(k)))
    }
    fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
        self.statuses.get(dev_id).cloned().unwrap_or_default()
    }
}

fn host() -> PairingHost<u32> {
    PairingHost::new(
        PairingConfig::default(),
        HOST,
        "0.4.0",
        "Test PC",
        Box::new(SeededRandom::new(1)),
    )
}

fn addr() -> SocketAddr {
    SocketAddr::from(([192, 168, 1, 5], 47_700))
}

fn client(qr: &QrPayload, seed: u64) -> PairingClient {
    PairingClient::start(
        qr,
        &SLOT,
        "0.4.0",
        "Test iPad",
        50_000,
        &mut SeededRandom::new(seed),
    )
    .unwrap()
}

fn sends<A: Clone>(out: &[PairOutput<A>]) -> Vec<Vec<u8>> {
    out.iter()
        .filter_map(|o| match o {
            PairOutput::Send { datagram, .. } => Some(datagram.clone()),
            _ => None,
        })
        .collect()
}

fn store_of<A>(out: Vec<PairOutput<A>>) -> Option<PendingPairing> {
    out.into_iter().find_map(|o| match o {
        PairOutput::Store(p) => Some(p),
        _ => None,
    })
}

/// Pairing up to the host's message 2, read by the iPad.
fn to_msg2(host: &mut PairingHost<u32>, qr: &QrPayload, creds: &Creds) -> PairingClient {
    let mut ipad = client(qr, 7);
    let msg2 = sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), creds));
    assert_eq!(msg2.len(), 1);
    ipad.read_msg2(&msg2[0]).unwrap();
    ipad
}

#[test]
fn a_qr_round_trips_and_parses_only_its_own_form() {
    let qr = QrPayload {
        addr: addr(),
        host_id: HOST,
        nonce_r: [0x0F; 16],
        secret: Zeroizing::new([0xA1; 32]),
        host_version: "0.4.0".into(),
    };
    let text = qr.encode();
    assert_eq!(
        *text,
        format!(
            "eternaldisplay://192.168.1.5:47700?v=3&h={}&n={}&s={}&hv=0.4.0",
            "b0".repeat(16),
            "0f".repeat(16),
            "a1".repeat(32)
        )
    );
    assert_eq!(QrPayload::parse(&text).unwrap(), qr);
    let v6 = QrPayload {
        addr: "[fd7a:115c:a1e0::1]:47700".parse().unwrap(),
        ..qr.clone()
    };
    assert_eq!(QrPayload::parse(&v6.encode()).unwrap(), v6);

    for bad in [
        text.replace("eternaldisplay://", "eternaldisplay:/"),
        text.replace("v=3", "v=2"),
        text.replace("&hv=0.4.0", ""),
        text.replace("&hv=0.4.0", "&hv=0.4.0&x=1"),
        text.replace("&hv=0.4.0", "&hv=0.4 0"),
        text.replace(&"a1".repeat(32), &"A1".repeat(32)),
        text.replace(&"a1".repeat(32), &"a1".repeat(31)),
        text.replace(":47700", ":0"),
        text.replace(":47700", ":047700"),
        text.replace("v=3&h=", "h=").replace("&n=", "&v=3&n="),
        format!("{}&", *text),
    ] {
        assert!(QrPayload::parse(&bad).is_err(), "{bad}");
    }
    assert!(
        !format!("{qr:?}").contains("a1a1"),
        "Debug leaves the secret out"
    );
}

#[test]
fn hkdf_and_hmac_match_their_rfc_vectors() {
    // RFC 5869, test case 1 (the first 32 bytes of its 42).
    let okm = hkdf32(
        &unhex_vec("000102030405060708090a0b0c"),
        &[0x0b; 22],
        &unhex_vec("f0f1f2f3f4f5f6f7f8f9"),
    );
    assert_eq!(
        okm.to_vec(),
        unhex_vec("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf")
    );
    // RFC 4231, test case 2.
    assert_eq!(
        hmac(b"Jefe", &[b"what do ya want ", b"for nothing?"]).to_vec(),
        unhex_vec("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
    );
}

/// Our own vectors, for the Swift side to match: `ctx`, the PSK, the MACs,
/// the payloads and the confirm body, from fixed inputs. The PSK and MAC
/// values were computed independently with Python's `hashlib` and `hmac`.
#[test]
fn pairing_vectors() {
    let ctx = pairing_ctx("0.4.0", "0.4.1", &SLOT, &HOST, &[0x11; 16], &[0x22; 16]);
    let mut expected = vec![25u8];
    expected.extend_from_slice(b"EternalMonitor pairing v1");
    expected.extend_from_slice(&[1, 3, 5]);
    expected.extend_from_slice(b"0.4.0");
    expected.push(5);
    expected.extend_from_slice(b"0.4.1");
    expected.push(2);
    expected.extend_from_slice(b"qr");
    expected.push(21);
    expected.extend_from_slice(b"ipad:");
    expected.extend_from_slice(&SLOT);
    expected.push(21);
    expected.extend_from_slice(b"host:");
    expected.extend_from_slice(&HOST);
    expected.push(16);
    expected.extend_from_slice(&[0x11; 16]);
    expected.push(16);
    expected.extend_from_slice(&[0x22; 16]);
    assert_eq!(ctx, expected);

    let psk = qr_psk(&[0xA1; 32], &ctx);
    let info = ring::digest::digest(&ring::digest::SHA256, &ctx);
    assert_eq!(*psk, *hkdf32(b"EM v3 qr psk", &[0xA1; 32], info.as_ref()));
    assert_eq!(
        hex(&*psk),
        "86219fb26a592317e9c6f2a901316643c76fd1034b6b751fdb4f840549ef0b2c"
    );
    let h = [0x33; 32];
    assert_eq!(
        hex(&confirm_mac(&[0x44; 32], &h)),
        hex(&hmac(&[0x44; 32], &[b"confirm", &h]))
    );
    assert_eq!(
        hex(&confirm_mac(&[0x44; 32], &h)),
        "e00ee1ec1c790c614e5d143c1b8737f4b3a3937bb66957950b34b3b3bd404a33"
    );
    assert_eq!(
        hex(&replace_mac(&[0x55; 32], &h)),
        "ed8cb01227eea51df0dcb41d311a2622039823978c25af3c4f4f6a432b3b4fbc"
    );

    let accept = PairAccept {
        session_id: 0x0102_0304,
        dev_id: [0x66; 16],
        k_new: Zeroizing::new([0x77; 32]),
        host_name: "PC".into(),
    };
    let mut expected = vec![0x04, 0x03, 0x02, 0x01];
    expected.extend_from_slice(&[0x66; 16]);
    expected.extend_from_slice(&[0x77; 32]);
    expected.extend_from_slice(&[2, b'P', b'C']);
    assert_eq!(*accept.encode(), expected);
    assert_eq!(PairAccept::parse(&expected).unwrap(), accept);

    let confirm = PairMessage::Confirm {
        confirm_mac: [0x88; 32],
        replace: Some(([0x99; 16], [0xAA; 32])),
    };
    let body = confirm.encode_body();
    let mut expected = vec![0x88; 32];
    expected.push(1);
    expected.extend_from_slice(&[0x99; 16]);
    expected.extend_from_slice(&[0xAA; 32]);
    assert_eq!(body, expected);
    assert_eq!(
        PairMessage::parse(INNER_PAIR_CONFIRM_FOR_TESTS, &body).unwrap(),
        Some(confirm)
    );
}

const INNER_PAIR_CONFIRM_FOR_TESTS: u8 = wire::INNER_PAIR_CONFIRM;

#[test]
fn an_ipad_pairs_and_the_host_stores_second() {
    let mut host = host();
    let qr = host.open(T0, addr());
    let mut creds = Creds::default();
    let mut ipad = to_msg2(&mut host, &qr, &creds);
    let offered = ipad.read_msg2(&[]).err();
    assert!(offered.is_some(), "a second message 2 is refused");

    // The iPad stored K_new; it confirms.
    let confirm = ipad.confirm(None).unwrap();
    let pending = store_of(host.handle_datagram(T0 + ms(10), &ADDR, &confirm, &creds))
        .expect("the host writes the record now");
    assert_eq!(pending.slot_id, SLOT);
    assert_eq!(pending.replaces, None);
    assert_eq!(pending.device_name, "Test iPad");
    assert!(host.is_open(), "S isn't used up until the write");

    // Only after the write: PairDone with the slot status.
    let status = SlotStatus {
        held: vec![(pending.dev_id, RecordState::Pending)],
        tombstones: vec![],
    };
    creds.statuses.insert(pending.dev_id, status.clone());
    let out = host.stored(T0 + ms(20), &pending.dev_id, StoreResult::Stored, &creds);
    assert!(out
        .iter()
        .any(|o| matches!(o, PairOutput::Paired { dev_id, .. } if *dev_id == pending.dev_id)));
    let done = sends(&out);
    assert_eq!(done.len(), 1);
    assert_eq!(
        ipad.handle_record(&done[0]),
        Some(PairResult::Done(status.clone()))
    );
    assert!(!host.is_open(), "S is used up");

    // A retried confirm gets the same answer, sealed again.
    let retry = ipad.confirm(None).unwrap();
    let again = sends(&host.handle_datagram(T0 + ms(100), &ADDR, &retry, &creds));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0], done[0], "a fresh record");
    assert_eq!(
        ipad.handle_record(&again[0]),
        Some(PairResult::Done(status))
    );
    // ... at most every 50 ms.
    let retry = ipad.confirm(None).unwrap();
    assert!(sends(&host.handle_datagram(T0 + ms(120), &ADDR, &retry, &creds)).is_empty());

    // The key the iPad got is the one the host stores.
    let accept = ipad.accept.as_ref().unwrap();
    assert_eq!(accept.dev_id, pending.dev_id);
    assert_eq!(*accept.k_new, **pending.key);
}

#[test]
fn a_replace_proof_sets_replaces_only_for_the_active_record() {
    let old = ClientKey {
        dev_id: [0x11; 16],
        k_dev: [0x12; 32],
    };
    /// The slot's active record, the proof sent, and the `replaces` expected.
    type Case = (Option<(Id16, Key32)>, Option<ClientKey>, Option<Id16>);
    let cases: [Case; 4] = [
        // The slot's active key: replaces it.
        (
            Some((old.dev_id, old.k_dev)),
            Some(old.clone()),
            Some(old.dev_id),
        ),
        // A proof from another key: no replaces.
        (
            Some((old.dev_id, old.k_dev)),
            Some(ClientKey {
                dev_id: old.dev_id,
                k_dev: [0x13; 32],
            }),
            None,
        ),
        // A dev_id that isn't the active one: no replaces.
        (Some(([0x21; 16], [0x22; 32])), Some(old.clone()), None),
        // No active record: no replaces.
        (None, Some(old.clone()), None),
    ];
    for (active, proof, expected) in cases {
        let mut host = host();
        let qr = host.open(T0, addr());
        let creds = Creds {
            active,
            ..Creds::default()
        };
        let mut ipad = to_msg2(&mut host, &qr, &creds);
        let confirm = ipad.confirm(proof.as_ref()).unwrap();
        let pending = store_of(host.handle_datagram(T0, &ADDR, &confirm, &creds)).unwrap();
        assert_eq!(pending.replaces, expected);
    }
}

#[test]
fn only_the_qr_s_secret_pairs_and_only_while_it_lasts() {
    let creds = Creds::default();
    // Another QR's secret: no answer.
    let mut host = host();
    let qr = host.open(T0, addr());
    let mut other = qr.clone();
    other.secret = Zeroizing::new([0x01; 32]);
    assert!(host
        .handle_datagram(T0, &ADDR, client(&other, 1).msg1(), &creds)
        .is_empty());
    // A changed host version (part of ctx): no answer.
    let mut other = qr.clone();
    other.host_version = "0.4.9".into();
    assert!(host
        .handle_datagram(T0, &ADDR, client(&other, 2).msg1(), &creds)
        .is_empty());
    // A changed iPad version on the wire: no answer.
    let ipad = client(&qr, 3);
    let mut msg1 = ipad.msg1().to_vec();
    let at = wire::MSG1_EXTENSION_OFFSET + 4 + 32 + 1;
    msg1[at] = b'9';
    assert!(host.handle_datagram(T0, &ADDR, &msg1, &creds).is_empty());

    // A newer QR: the old one stops working.
    let old = qr.clone();
    let _new = host.open(T0 + ms(10), addr());
    assert!(host
        .handle_datagram(T0 + ms(20), &ADDR, client(&old, 4).msg1(), &creds)
        .is_empty());

    // Past 10 minutes: refused even before the window is polled shut.
    let mut host = self::host();
    let qr = host.open(T0, addr());
    assert!(host
        .handle_datagram(T0 + QR_LIFETIME, &ADDR, client(&qr, 12).msg1(), &creds)
        .is_empty());
    let mut host = self::host();
    let qr = host.open(T0, addr());
    host.poll(T0 + QR_LIFETIME);
    assert!(!host.is_open());
    assert!(host
        .handle_datagram(T0 + QR_LIFETIME, &ADDR, client(&qr, 5).msg1(), &creds)
        .is_empty());
    // Closed by the host.
    let qr = host.open(T0 + QR_LIFETIME, addr());
    host.close();
    assert!(host
        .handle_datagram(T0 + QR_LIFETIME, &ADDR, client(&qr, 6).msg1(), &creds)
        .is_empty());
}

#[test]
fn message_1_retries_get_the_same_message_2_and_reads_are_budgeted() {
    let creds = Creds::default();
    let mut host = host();
    let qr = host.open(T0, addr());
    let ipad = client(&qr, 7);
    let first = sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds));
    assert_eq!(first.len(), 1);
    assert!(sends(&host.handle_datagram(T0 + ms(50), &ADDR, ipad.msg1(), &creds)).is_empty());
    let again = sends(&host.handle_datagram(T0 + ms(150), &ADDR, ipad.msg1(), &creds));
    assert_eq!(again, first, "the same message 2");

    // Forged message 1s: 50 reads a second, then nothing is read.
    let mut host = self::host();
    let qr = host.open(T0, addr());
    let mut forged = qr.clone();
    forged.secret = Zeroizing::new([0x02; 32]);
    let forged = client(&forged, 8);
    for _ in 0..60 {
        host.handle_datagram(T0, &ADDR, forged.msg1(), &creds);
    }
    assert!(host
        .handle_datagram(T0, &ADDR, client(&qr, 9).msg1(), &creds)
        .is_empty());
    assert_eq!(
        sends(&host.handle_datagram(
            T0 + Duration::from_secs(1),
            &ADDR,
            client(&qr, 9).msg1(),
            &creds
        ))
        .len(),
        1,
        "the next second reads again"
    );
}

#[test]
fn a_store_that_fails_waits_for_the_next_confirm_and_too_many_refuses() {
    let creds = Creds::default();
    let mut host = host();
    let qr = host.open(T0, addr());
    let mut ipad = to_msg2(&mut host, &qr, &creds);
    let confirm = ipad.confirm(None).unwrap();
    let pending = store_of(host.handle_datagram(T0, &ADDR, &confirm, &creds)).unwrap();
    // Retries while the write runs write nothing more.
    let retry = ipad.confirm(None).unwrap();
    assert!(host
        .handle_datagram(T0 + ms(5), &ADDR, &retry, &creds)
        .is_empty());
    // It fails: nothing is sent, and the next confirm writes again.
    assert!(host
        .stored(T0 + ms(10), &pending.dev_id, StoreResult::Failed, &creds)
        .is_empty());
    let retry = ipad.confirm(None).unwrap();
    let pending = store_of(host.handle_datagram(T0 + ms(20), &ADDR, &retry, &creds)).unwrap();
    // Too many: a terminal refusal.
    let refused = sends(&host.stored(T0 + ms(30), &pending.dev_id, StoreResult::TooMany, &creds));
    assert_eq!(
        ipad.handle_record(&refused[0]),
        Some(PairResult::Refused(PAIR_REFUSED_TOO_MANY))
    );
    assert!(host.is_open(), "nothing stored: the QR still works");
}

#[test]
fn an_abort_ends_the_pairing_and_nothing_is_stored() {
    let creds = Creds::default();
    let mut host = host();
    let qr = host.open(T0, addr());
    let mut ipad = to_msg2(&mut host, &qr, &creds);
    let abort = ipad.abort().unwrap();
    assert!(host.handle_datagram(T0, &ADDR, &abort, &creds).is_empty());
    assert_eq!(host.session_id(), None);
    // A late confirm finds nothing.
    let confirm = ipad.confirm(None).unwrap();
    assert!(store_of(host.handle_datagram(T0, &ADDR, &confirm, &creds)).is_none());
}

#[test]
fn records_must_authenticate_and_never_replay() {
    let creds = Creds::default();
    let mut host = host();
    let qr = host.open(T0, addr());
    let mut ipad = to_msg2(&mut host, &qr, &creds);
    let confirm = ipad.confirm(None).unwrap();
    let mut flipped = confirm.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert!(host.handle_datagram(T0, &ADDR, &flipped, &creds).is_empty());
    assert!(store_of(host.handle_datagram(T0, &ADDR, &confirm, &creds)).is_some());
    assert!(
        host.handle_datagram(T0, &ADDR, &confirm, &creds).is_empty(),
        "a replay"
    );

    // A confirm whose MAC is for another key stores nothing.
    let mut host = self::host();
    let qr = host.open(T0, addr());
    let mut ipad = to_msg2(&mut host, &qr, &creds);
    if let Some(accept) = ipad.accept.as_mut() {
        accept.k_new = Zeroizing::new([0x01; 32]);
    }
    let confirm = ipad.confirm(None).unwrap();
    assert!(store_of(host.handle_datagram(T0, &ADDR, &confirm, &creds)).is_none());
}

#[test]
fn an_unconfirmed_pairing_ends_after_its_lifetime() {
    let creds = Creds::default();
    let mut host = host();
    let qr = host.open(T0, addr());
    let mut ipad = to_msg2(&mut host, &qr, &creds);
    host.poll(T0 + Duration::from_secs(5));
    assert_eq!(host.session_id(), None);
    let confirm = ipad.confirm(None).unwrap();
    assert!(host
        .handle_datagram(T0 + Duration::from_secs(5), &ADDR, &confirm, &creds)
        .is_empty());
}

#[test]
fn one_pairing_at_a_time_while_one_is_being_stored() {
    let creds = Creds::default();
    let mut host = host();
    let qr = host.open(T0, addr());
    let mut ipad = to_msg2(&mut host, &qr, &creds);
    let confirm = ipad.confirm(None).unwrap();
    assert!(store_of(host.handle_datagram(T0, &ADDR, &confirm, &creds)).is_some());
    let session = host.session_id();
    assert!(host
        .handle_datagram(T0, &ADDR, client(&qr, 11).msg1(), &creds)
        .is_empty());
    assert_eq!(host.session_id(), session);
    // A newer QR doesn't end the pairing being stored either.
    host.open(T0 + ms(1), addr());
    assert_eq!(host.session_id(), session);
}
