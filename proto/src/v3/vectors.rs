//! Spec test vectors: `testdata/v3/noise_vectors.txt`.
//!
//! - The Noise NNpsk0 vector runs through the same `snow` configuration the
//!   handshake uses, then its four transport messages run through the record
//!   layer's AEAD on the raw `Split()` keys. That pins the nonce format and
//!   which key belongs to which direction.
//! - RFC 8439 §2.8.2 checks the ChaCha20-Poly1305 the records use.
//! - RFC 7748 §5.2 checks the X25519 the handshake uses.

use std::collections::BTreeMap;
use std::path::PathBuf;

use snow::resolvers::{CryptoResolver, DefaultResolver};

use super::record::{open_raw, seal_raw};
use super::NOISE_PARAMS;

/// Reads a `name<space>hex` file, skipping blank lines and `#` comments.
pub(super) fn load(name: &str) -> BTreeMap<String, Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/v3")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(' ').expect("name<space>hex");
        assert!(
            out.insert(key.to_string(), unhex(value)).is_none(),
            "duplicate {key}"
        );
    }
    out
}

pub(super) fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex length");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn key32(bytes: &[u8]) -> [u8; 32] {
    bytes.try_into().expect("32 bytes")
}

#[test]
fn noise_nnpsk0_vector_through_snow() {
    let v = load("noise_vectors.txt");
    let psk = key32(&v["nnpsk0.psk"]);
    let build = |ephemeral: &str| {
        snow::Builder::new(NOISE_PARAMS.parse().unwrap())
            .psk(0, &psk)
            .unwrap()
            .prologue(&v["nnpsk0.prologue"])
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(&v[ephemeral])
    };
    let mut init = build("nnpsk0.init_ephemeral").build_initiator().unwrap();
    let mut resp = build("nnpsk0.resp_ephemeral").build_responder().unwrap();
    let mut buf = [0u8; 1024];
    let mut plain = [0u8; 1024];

    let n = init
        .write_message(&v["nnpsk0.msg0.payload"], &mut buf)
        .unwrap();
    assert_eq!(hex(&buf[..n]), hex(&v["nnpsk0.msg0.ciphertext"]));
    let m = resp.read_message(&buf[..n], &mut plain).unwrap();
    assert_eq!(plain[..m], v["nnpsk0.msg0.payload"][..]);

    let n = resp
        .write_message(&v["nnpsk0.msg1.payload"], &mut buf)
        .unwrap();
    assert_eq!(hex(&buf[..n]), hex(&v["nnpsk0.msg1.ciphertext"]));
    let m = init.read_message(&buf[..n], &mut plain).unwrap();
    assert_eq!(plain[..m], v["nnpsk0.msg1.payload"][..]);

    assert_eq!(init.get_handshake_hash(), &v["nnpsk0.handshake_hash"][..]);
    assert_eq!(resp.get_handshake_hash(), &v["nnpsk0.handshake_hash"][..]);

    let (i2r, r2i) = init.dangerously_get_raw_split();
    assert_eq!(
        (i2r, r2i),
        resp.dangerously_get_raw_split(),
        "same split order on both sides"
    );

    // msg2..msg5: i->r nonce 0, r->i nonce 0, i->r nonce 1, r->i nonce 1.
    for (index, (key, counter)) in [(i2r, 0u64), (r2i, 0), (i2r, 1), (r2i, 1)]
        .into_iter()
        .enumerate()
    {
        let msg = index + 2;
        let payload = &v[&format!("nnpsk0.msg{msg}.payload")];
        let ciphertext = &v[&format!("nnpsk0.msg{msg}.ciphertext")];
        let mut sealed = payload.clone();
        seal_raw(&key, counter, &[], &mut sealed);
        assert_eq!(hex(&sealed), hex(ciphertext), "transport msg{msg}");
        let mut opened = ciphertext.clone();
        let len = open_raw(&key, counter, &[], &mut opened).unwrap();
        assert_eq!(opened[..len], payload[..]);
    }
}

#[test]
fn rfc8439_aead_vector() {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
    let v = load("noise_vectors.txt");
    let key = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &v["rfc8439.key"]).unwrap());
    let nonce = || Nonce::try_assume_unique_for_key(&v["rfc8439.nonce"]).unwrap();
    let mut in_out = v["rfc8439.plaintext"].clone();
    key.seal_in_place_append_tag(nonce(), Aad::from(&v["rfc8439.aad"]), &mut in_out)
        .unwrap();
    let mut expected = v["rfc8439.ciphertext"].clone();
    expected.extend_from_slice(&v["rfc8439.tag"]);
    assert_eq!(hex(&in_out), hex(&expected));
    let plain = key
        .open_in_place(nonce(), Aad::from(&v["rfc8439.aad"]), &mut in_out)
        .unwrap();
    assert_eq!(plain, &v["rfc8439.plaintext"][..]);
}

#[test]
fn rfc7748_x25519_iterated_through_snow() {
    let v = load("noise_vectors.txt");
    let resolver = DefaultResolver;
    let mut k = v["rfc7748.start"].clone();
    let mut u = k.clone();
    for round in 1..=1000 {
        let mut dh = resolver
            .resolve_dh(&snow::params::DHChoice::Curve25519)
            .unwrap();
        dh.set(&k);
        let mut out = [0u8; 32];
        dh.dh(&u, &mut out).unwrap();
        u = std::mem::replace(&mut k, out.to_vec());
        if round == 1 {
            assert_eq!(hex(&k), hex(&v["rfc7748.after_1"]));
        }
    }
    assert_eq!(hex(&k), hex(&v["rfc7748.after_1000"]));
}
