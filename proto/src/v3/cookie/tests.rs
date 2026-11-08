use super::*;
use crate::v3::host::PeerAddr;
use crate::v3::vectors::load;
use crate::v3::{wire, SeededRandom};
use std::net::SocketAddr;

fn key32(bytes: &[u8]) -> Key32 {
    bytes.try_into().expect("32 bytes")
}

fn arr<const N: usize>(bytes: &[u8]) -> [u8; N] {
    bytes.try_into().expect("length")
}

fn golden_msg1() -> Vec<u8> {
    load("session_golden.txt")["msg1"].clone()
}

#[test]
fn vectors() {
    let v = load("cookie_vectors.txt");
    let g = load("session_golden.txt");
    let secret = key32(&v["input.secret"]);
    let msg1 = &g["msg1"];

    // Addresses as the host's peers give them.
    let v4: SocketAddr = "192.0.2.10:50000".parse().unwrap();
    let v6: SocketAddr = "[2001:db8::1]:50000".parse().unwrap();
    let mapped: SocketAddr = "[::ffff:192.0.2.10]:50000".parse().unwrap();
    assert_eq!(v4.cookie_input(), v["input.address_v4"]);
    assert_eq!(v6.cookie_input(), v["input.address_v6"]);
    assert_eq!(mapped.cookie_input(), v["input.address_v4"]);

    let cookie: [u8; 16] = arr(&v["cookie.v4"]);
    assert_eq!(cookie_value(&secret, &v["input.address_v4"]), cookie);
    assert_eq!(
        cookie_value(&secret, &v["input.address_v6"]).to_vec(),
        v["cookie.v6"]
    );
    let k_dev = key32(&g["input.k_dev"]);
    let dev_id: Id16 = arr(&g["input.dev_id"]);
    let session = session_key(&k_dev, &dev_id);
    assert_eq!(session.to_vec(), v["key.session"]);
    let pairing = pairing_key(&key32(&v["input.pair_psk"]));
    assert_eq!(pairing.to_vec(), v["key.pairing"]);

    let digest = msg1_digest(msg1);
    assert_eq!(digest.to_vec(), v["msg1.digest"]);
    assert_eq!(mac2(&cookie, msg1).to_vec(), v["msg1.mac2"]);
    let with = attach(msg1, &cookie);
    assert_eq!(with, v["msg1.with_cookie"]);
    let (canon, mac) = split(&with);
    assert_eq!(&*canon, &msg1[..]);
    assert_eq!(mac.map(|m| m.to_vec()), Some(v["msg1.mac2"].clone()));
    // The message 1 with the cookie still reads as the same message 1.
    assert_eq!(wire::classify(&with), V3Kind::Handshake1);
    assert!(wire::parse_msg1(&canon).is_ok());

    for (name, key, tag, nonce) in [
        (
            "reply.session",
            &session,
            EXT_SESSION,
            "input.nonce_session",
        ),
        ("reply.pairing", &pairing, EXT_PAIR_QR, "input.nonce_pair"),
    ] {
        let sealed = seal(key, tag, arr(&v[nonce]), &cookie, &digest).encode();
        assert_eq!(sealed, v[name], "{name}");
        assert_eq!(sealed.len(), COOKIE_REPLY_SIZE);
        assert!(is_reply(&sealed));
        let parsed = CookieReply::parse(&v[name]).unwrap();
        assert_eq!(parsed.tag, tag);
        assert_eq!(open(key, &parsed, &digest), Some(cookie));
    }
}

#[test]
fn a_reply_opens_only_under_its_key_and_for_its_own_message_1() {
    let v = load("cookie_vectors.txt");
    let reply = CookieReply::parse(&v["reply.session"]).unwrap();
    let key = key32(&v["key.session"]);
    let digest: [u8; 32] = arr(&v["msg1.digest"]);
    let mut other = digest;
    other[0] ^= 1;
    assert!(open(&key, &reply, &other).is_none(), "another message 1");
    let mut wrong = key;
    wrong[31] ^= 1;
    assert!(open(&wrong, &reply, &digest).is_none(), "another key");
    let pairing = key32(&v["key.pairing"]);
    assert!(open(&pairing, &reply, &digest).is_none(), "the pairing key");
    for at in 0..32 {
        let mut flipped = reply.clone();
        flipped.sealed[at] ^= 0x80;
        assert!(open(&key, &flipped, &digest).is_none(), "sealed byte {at}");
    }
    for at in 0..12 {
        let mut flipped = reply.clone();
        flipped.nonce[at] ^= 1;
        assert!(open(&key, &flipped, &digest).is_none(), "nonce byte {at}");
    }
}

#[test]
fn the_reply_parser_takes_exactly_what_the_encoder_writes() {
    let v = load("cookie_vectors.txt");
    let good = v["reply.session"].clone();
    assert!(CookieReply::parse(&good).is_ok());
    assert!(CookieReply::parse(&good[..63]).is_err(), "short");
    let mut long = good.clone();
    long.push(0);
    assert!(CookieReply::parse(&long).is_err(), "long");
    // Every header byte is fixed: magic, version, type, flags, payload_len,
    // session_id 0 and msg_seq 0.
    for at in 0..16 {
        let mut bad = good.clone();
        bad[at] ^= 1;
        assert!(CookieReply::parse(&bad).is_err(), "header byte {at}");
    }
    for tag in [0u16, 3, EXT_COOKIE, 0x0100] {
        let mut bad = good.clone();
        bad[16..18].copy_from_slice(&tag.to_le_bytes());
        assert!(CookieReply::parse(&bad).is_err(), "tag {tag:#x}");
    }
    let mut bad = good.clone();
    bad[18] = 43;
    assert!(CookieReply::parse(&bad).is_err(), "extension length");
    // Not a message any v2 or v3 parser takes.
    assert_eq!(wire::classify(&good), V3Kind::Other);
    assert!(crate::v2::control::parse_control(&good).is_err());
}

#[test]
fn split_takes_one_exact_cookie_and_leaves_anything_else_for_the_parser() {
    let msg1 = golden_msg1();
    let cookie = [7u8; 16];
    // No cookie: the same bytes, borrowed.
    let (canon, mac) = split(&msg1);
    assert!(matches!(canon, std::borrow::Cow::Borrowed(_)));
    assert_eq!((&*canon, mac), (&msg1[..], None));
    let with = attach(&msg1, &cookie);
    // Two cookies, a cookie of another length, another tag, or a byte more:
    // left as they are, so message 1's parser refuses them.
    let twice = attach(&with, &cookie);
    let mut long = with.clone();
    long.push(0);
    let mut short = with[..with.len() - 1].to_vec();
    let len = u16::try_from(short.len() - 16).unwrap();
    short[6..8].copy_from_slice(&len.to_le_bytes());
    let mut tag = with.clone();
    let at = msg1.len();
    tag[at] = 0xFE;
    let mut length = with.clone();
    length[at + 2] = 15;
    for (name, datagram) in [
        ("twice", twice),
        ("long", long),
        ("short", short),
        ("tag", tag),
        ("length", length),
    ] {
        let (canon, mac) = split(&datagram);
        assert_eq!(mac, None, "{name}");
        assert_eq!(&*canon, &datagram[..], "{name}");
        assert!(wire::parse_msg1(&canon).is_err(), "{name}");
    }
    // Not a message 1 at all: untouched.
    let reply = load("cookie_vectors.txt")["reply.session"].clone();
    assert_eq!(
        split(&reply),
        (std::borrow::Cow::Borrowed(&reply[..]), None)
    );
    assert_eq!(split(&[]), (std::borrow::Cow::Borrowed(&[][..]), None));
}

/// The jar against the address and message 1 it was made for.
fn check(jar: &mut CookieJar, now: Duration, cookie: &[u8; 16], rng: &mut SeededRandom) -> bool {
    let msg1 = golden_msg1();
    jar.verify(now, b"addr", &msg1, &mac2(cookie, &msg1), rng)
}

/// What the jar's reply to `b"addr"` carries at `now`.
fn issue(jar: &mut CookieJar, now: Duration, rng: &mut SeededRandom) -> [u8; 16] {
    let msg1 = golden_msg1();
    let key = [3u8; 32];
    let reply = jar.reply(now, b"addr", &key, EXT_SESSION, &msg1, rng);
    let parsed = CookieReply::parse(&reply).unwrap();
    open(&key, &parsed, &msg1_digest(&msg1)).unwrap()
}

#[test]
fn a_cookie_binds_its_address_and_its_message_1() {
    let mut rng = SeededRandom::new(1);
    let mut jar = CookieJar::new(Duration::ZERO, &mut rng);
    let now = Duration::from_secs(1);
    let cookie = issue(&mut jar, now, &mut rng);
    let msg1 = golden_msg1();
    let mac = mac2(&cookie, &msg1);
    assert!(jar.verify(now, b"addr", &msg1, &mac, &mut rng));
    assert!(
        !jar.verify(now, b"adds", &msg1, &mac, &mut rng),
        "another address"
    );
    let mut other = msg1.clone();
    other[20] ^= 1;
    assert!(
        !jar.verify(now, b"addr", &other, &mac, &mut rng),
        "another message 1"
    );
    let mut bad = mac;
    bad[15] ^= 1;
    assert!(
        !jar.verify(now, b"addr", &msg1, &bad, &mut rng),
        "a flipped MAC"
    );
    // Another port is another address.
    let a: SocketAddr = "192.0.2.10:50000".parse().unwrap();
    let b: SocketAddr = "192.0.2.10:50001".parse().unwrap();
    assert_ne!(a.cookie_input(), b.cookie_input());
}

#[test]
fn rotation_keeps_the_previous_secret_for_one_lifetime() {
    let mut rng = SeededRandom::new(2);
    let t0 = Duration::from_secs(1000);
    let mut jar = CookieJar::new(t0, &mut rng);
    let first = issue(&mut jar, t0, &mut rng);
    // Just before the lifetime: the same secret, the same cookie.
    let before = t0 + SECRET_LIFETIME - Duration::from_millis(1);
    assert_eq!(issue(&mut jar, before, &mut rng), first);
    // At it: a new secret for new cookies; the old one is still accepted.
    let at = t0 + SECRET_LIFETIME;
    let second = issue(&mut jar, at, &mut rng);
    assert_ne!(second, first);
    assert!(check(&mut jar, at, &first, &mut rng));
    assert!(check(&mut jar, at, &second, &mut rng));
    // A second rotation forgets the first secret.
    let next = at + SECRET_LIFETIME;
    assert!(check(&mut jar, next, &second, &mut rng));
    assert!(!check(&mut jar, next, &first, &mut rng));
}

#[test]
fn a_jar_idle_for_two_lifetimes_forgets_both_secrets() {
    let mut rng = SeededRandom::new(3);
    let t0 = Duration::from_secs(1000);
    let mut jar = CookieJar::new(t0, &mut rng);
    let first = issue(&mut jar, t0, &mut rng);
    let later = t0 + 2 * SECRET_LIFETIME;
    assert!(!check(&mut jar, later, &first, &mut rng));
    // Just under two lifetimes, it's the previous secret and still good.
    let mut jar = CookieJar::new(t0, &mut rng);
    let first = issue(&mut jar, t0, &mut rng);
    let almost = t0 + 2 * SECRET_LIFETIME - Duration::from_millis(1);
    assert!(check(&mut jar, almost, &first, &mut rng));
}

/// Wherever rotations fall, a cookie is accepted for at least
/// [`SECRET_LIFETIME`] after it was issued, so the iPad's
/// [`CLIENT_COOKIE_LIFETIME`] is always inside it.
#[test]
fn every_cookie_is_accepted_for_as_long_as_the_ipad_keeps_it() {
    const { assert!(CLIENT_COOKIE_LIFETIME.as_millis() < SECRET_LIFETIME.as_millis()) };
    let mut rng = SeededRandom::new(4);
    let t0 = Duration::from_secs(1000);
    for seed in 0..200u64 {
        let mut jar = CookieJar::new(t0, &mut rng);
        // Calls at pseudo-random gaps of up to 130 s rotate it lazily.
        let mut now = t0;
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut gap = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            Duration::from_millis(state % 130_000)
        };
        for _ in 0..6 {
            now += gap();
            issue(&mut jar, now, &mut rng);
        }
        now += gap();
        let issued = now;
        let cookie = issue(&mut jar, issued, &mut rng);
        let mut at = issued;
        while at < issued + CLIENT_COOKIE_LIFETIME {
            assert!(check(&mut jar, at, &cookie, &mut rng), "seed {seed}");
            at += gap().min(Duration::from_secs(20)) + Duration::from_millis(1);
        }
        let last = issued + CLIENT_COOKIE_LIFETIME - Duration::from_millis(1);
        assert!(check(&mut jar, last, &cookie, &mut rng), "seed {seed}");
    }
}

#[test]
fn a_held_cookie_is_fresh_for_its_lifetime() {
    let held = HeldCookie {
        cookie: [9; 16],
        received: Duration::from_secs(5),
    };
    assert!(held.fresh(Duration::from_secs(5) + CLIENT_COOKIE_LIFETIME - Duration::from_millis(1)));
    assert!(!held.fresh(Duration::from_secs(5) + CLIENT_COOKIE_LIFETIME));
    assert!(
        !format!("{held:?}").contains("9, 9"),
        "Debug hides the cookie"
    );
}

/// A cookie reply is never larger than what it answers: no message 1 the
/// host would answer is shorter than its extension's header.
#[test]
fn a_reply_is_smaller_than_any_message_1() {
    const { assert!(COOKIE_REPLY_SIZE < wire::MSG1_EXTENSION_OFFSET + EXTENSION_HEADER_SIZE) };
    assert!(COOKIE_REPLY_SIZE < golden_msg1().len());
}
