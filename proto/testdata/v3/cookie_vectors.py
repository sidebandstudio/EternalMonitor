"""Writes cookie_vectors.txt: the return-routability cookie's vectors.

Independent of the Rust code: HMAC and HKDF come from Python's hashlib and
hmac, and ChaCha20-Poly1305 is the RFC 8439 construction written out below,
checked against RFC 8439 section 2.8.2 before anything is written. Message 1
is the golden transcript's (session_golden.txt), under its k_dev and dev_id.

    python3 -I proto/testdata/v3/cookie_vectors.py
"""

import hashlib
import hmac
import ipaddress
import pathlib
import struct

HERE = pathlib.Path(__file__).resolve().parent

MAGIC = 0x4D45
VERSION = 2
COOKIE_TYPE = 0x42
EXT_SESSION = 0x0001
EXT_PAIR_QR = 0x0002
EXT_COOKIE = 0x00FF
SALT = b"EM v3 cookie"


def rotl(v, n):
    return ((v << n) & 0xFFFFFFFF) | (v >> (32 - n))


def quarter(s, a, b, c, d):
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = rotl(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = rotl(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = rotl(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = rotl(s[b] ^ s[c], 7)


def chacha_block(key, counter, nonce):
    state = list(struct.unpack("<4I", b"expand 32-byte k"))
    state += list(struct.unpack("<8I", key))
    state += [counter] + list(struct.unpack("<3I", nonce))
    s = state[:]
    for _ in range(10):
        quarter(s, 0, 4, 8, 12)
        quarter(s, 1, 5, 9, 13)
        quarter(s, 2, 6, 10, 14)
        quarter(s, 3, 7, 11, 15)
        quarter(s, 0, 5, 10, 15)
        quarter(s, 1, 6, 11, 12)
        quarter(s, 2, 7, 8, 13)
        quarter(s, 3, 4, 9, 14)
    return struct.pack("<16I", *((x + y) & 0xFFFFFFFF for x, y in zip(s, state)))


def chacha_xor(key, counter, nonce, data):
    out = bytearray()
    for i in range(0, len(data), 64):
        block = chacha_block(key, counter + i // 64, nonce)
        out += bytes(a ^ b for a, b in zip(data[i : i + 64], block))
    return bytes(out)


def poly1305(key, msg):
    r = int.from_bytes(key[:16], "little") & 0x0FFFFFFC0FFFFFFC0FFFFFFC0FFFFFFF
    s = int.from_bytes(key[16:], "little")
    p = (1 << 130) - 5
    acc = 0
    for i in range(0, len(msg), 16):
        n = int.from_bytes(msg[i : i + 16] + b"\x01", "little")
        acc = (acc + n) * r % p
    return ((acc + s) & ((1 << 128) - 1)).to_bytes(16, "little")


def pad16(b):
    return b"\x00" * (-len(b) % 16)


def seal(key, nonce, plaintext, aad):
    otk = chacha_block(key, 0, nonce)[:32]
    ct = chacha_xor(key, 1, nonce, plaintext)
    mac_data = aad + pad16(aad) + ct + pad16(ct)
    mac_data += struct.pack("<QQ", len(aad), len(ct))
    return ct + poly1305(otk, mac_data)


def check_rfc8439():
    key = bytes(range(0x80, 0xA0))
    nonce = bytes.fromhex("070000004041424344454647")
    aad = bytes.fromhex("50515253c0c1c2c3c4c5c6c7")
    plaintext = (
        b"Ladies and Gentlemen of the class of '99: If I could offer you "
        b"only one tip for the future, sunscreen would be it."
    )
    out = seal(key, nonce, plaintext, aad)
    assert out[-16:].hex() == "1ae10b594f09e26a7e902ecbd0600691", "RFC 8439 2.8.2 tag"
    assert out[:16].hex() == "d31a8d34648e60db7b86afbc53ef7ec2", "RFC 8439 2.8.2 ciphertext"


def hkdf32(salt, ikm, info):
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    return hmac.new(prk, info + b"\x01", hashlib.sha256).digest()


def hmac16(key, data):
    return hmac.new(key, data, hashlib.sha256).digest()[:16]


def address(ip, port):
    ip = ipaddress.ip_address(ip)
    if ip.version == 6 and ip.ipv4_mapped:
        ip = ip.ipv4_mapped
    return ip.packed + struct.pack("<H", port)


def with_payload_len(datagram):
    return datagram[:6] + struct.pack("<H", len(datagram) - 16) + datagram[8:]


def attach(msg1, cookie):
    tail = struct.pack("<HH", EXT_COOKIE, 16) + hmac16(cookie, msg1)
    return with_payload_len(msg1 + tail)


def reply(key, tag, nonce, cookie, msg1):
    header = struct.pack("<HBB", MAGIC, VERSION, COOKIE_TYPE) + b"\x00" * 12
    body = struct.pack("<HH", tag, 44) + nonce
    body += seal(key, nonce, cookie, hashlib.sha256(msg1).digest())
    return with_payload_len(header + body)


def golden():
    out = {}
    for line in (HERE / "session_golden.txt").read_text().splitlines():
        if line and not line.startswith("#"):
            name, value = line.split(" ", 1)
            out[name] = bytes.fromhex(value)
    return out


def main():
    check_rfc8439()
    g = golden()
    msg1, k_dev, dev_id = g["msg1"], g["input.k_dev"], g["input.dev_id"]
    secret = bytes(range(0xC0, 0xE0))
    psk = bytes(range(0x50, 0x70))
    nonce_session = bytes(range(0x01, 0x0D))
    nonce_pair = bytes(range(0x11, 0x1D))
    nonce_r = bytes(range(0x30, 0x40))
    v4 = address("192.0.2.10", 50000)
    v6 = address("2001:db8::1", 50000)
    mapped = address("::ffff:192.0.2.10", 50000)
    assert mapped == v4
    # A session cookie binds the dev_id and the address; a pairing cookie
    # binds the QR window's nonce_r and the address.
    cookie_v4 = hmac16(secret, dev_id + v4)
    session_key = hkdf32(SALT, k_dev, dev_id)
    pairing_key = hkdf32(SALT, psk, b"pair")
    rows = [
        ("input.secret", secret),
        ("input.address_v4", v4),
        ("input.address_v6", v6),
        ("input.pair_psk", psk),
        ("input.nonce_r", nonce_r),
        ("input.nonce_session", nonce_session),
        ("input.nonce_pair", nonce_pair),
        ("cookie.v4", cookie_v4),
        ("cookie.v6", hmac16(secret, dev_id + v6)),
        ("cookie.pair_v4", hmac16(secret, nonce_r + v4)),
        ("key.session", session_key),
        ("key.pairing", pairing_key),
        ("msg1.digest", hashlib.sha256(msg1).digest()),
        ("msg1.mac2", hmac16(cookie_v4, msg1)),
        ("msg1.with_cookie", attach(msg1, cookie_v4)),
        ("reply.session", reply(session_key, EXT_SESSION, nonce_session, cookie_v4, msg1)),
        ("reply.pairing", reply(pairing_key, EXT_PAIR_QR, nonce_pair, cookie_v4, msg1)),
    ]
    header = [
        "# Return-routability cookie vectors. One `name<space>lowercase-hex` per line.",
        "# Written by cookie_vectors.py (independent of the Rust code); message 1, k_dev",
        "# and dev_id are session_golden.txt's. Addresses are the IP (4 or 16 bytes,",
        "# v4-mapped IPv6 as IPv4) then the port, little-endian. cookie.v4/v6 are",
        "# HMAC-SHA-256(secret, dev_id || address)[..16] (a session cookie);",
        "# cookie.pair_v4 is HMAC-SHA-256(secret, nonce_r || address)[..16] (pairing).",
        "# The iPad treats a cookie as opaque. key.session is HKDF(salt \"EM v3 cookie\",",
        "# k_dev, info dev_id), key.pairing HKDF(salt, pair_psk, info \"pair\");",
        "# msg1.with_cookie is message 1 with EXT_COOKIE carrying msg1.mac2 under",
        "# cookie.v4; reply.* seal cookie.v4 with AAD msg1.digest.",
        "# Consumed by `cargo test -p eternal-wire --lib v3::cookie` and, from phase 3,",
        "# by the iOS tests. Regenerate only on a deliberate wire change.",
    ]
    text = "\n".join(header + [f"{name} {value.hex()}" for name, value in rows]) + "\n"
    (HERE / "cookie_vectors.txt").write_text(text)


if __name__ == "__main__":
    main()
