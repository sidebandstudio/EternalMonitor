//! Differential parser vectors: `testdata/v3/parser_differential.txt`.
//!
//! For each v3 parser the iPad runs, a seeded generator mutates valid
//! encodings (bit flips, truncations, overwritten bytes, random bytes,
//! appended bytes) into 4,000 inputs. The file pins, per parser, a digest of
//! the inputs (so the Swift generator is checked to be the same), every
//! accept/reject verdict as a bitmap, and a digest of the canonical output of
//! each accepted input. The Swift parsers (phase 3) must reach the same
//! verdict and output on every input: the Rust reference is the oracle for
//! "every malformed message is rejected".
//!
//! The generator, which Swift reimplements step for step: xorshift64
//! (`x ^= x << 13; x ^= x >> 7; x ^= x << 17`) from the parser's seed. Per
//! case: `base = bases[next % bases.len]`, then one mutation chosen by
//! `next % 5`, and a second one when `next % 3 == 0`:
//! - 0, flip a bit: `at = next % len`, `bit = next % 8` (nothing if empty);
//! - 1, truncate to `next % (len + 1)` bytes;
//! - 2, overwrite: `at = next % len`, `value = next as u8` (nothing if empty);
//! - 3, random: `n = next % (len + 9)` bytes, each `next as u8`;
//! - 4, append `1 + next % 4` bytes, each `next as u8`.
//!
//! Canonical outputs: `hello`, `status`, `msg2_payload` re-encode the
//! parsed value; `session` is `[1, inner_type] || body` for a session
//! message, and for any other inner type `[2]` if it is application data
//! ([`wire::is_application`]) or `[3]` if it is a protocol type the client
//! drops (an empty input is rejected);
//! `msg1` is `dev_id || client_nonce || noise`; `msg2` the Noise message;
//! `record_header` `session_id || counter`; `classify` one byte (0
//! handshake 1, 1 handshake 2, 2 sealed, 3 other) and never rejects.
//!
//! Regenerate only on a deliberate wire change:
//! `UPDATE_GOLDEN=1 cargo test -p eternal-wire --lib v3::golden_differential`.

use ring::digest::{Context, SHA256};

use super::golden::check;
use super::record::RecordHeader;
use super::vectors::load;
use super::wire::{self, HelloPayload, Msg2Payload, SessionMessage, SlotStatus, V3Kind};

const CASES: usize = 4000;

struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn mutate_once(&mut self, out: &mut Vec<u8>) {
        let len = out.len();
        match self.next() % 5 {
            0 => {
                if len > 0 {
                    let at = self.below(len);
                    let bit = self.next() % 8;
                    out[at] ^= 1 << bit;
                }
            }
            1 => {
                let n = self.below(len + 1);
                out.truncate(n);
            }
            2 => {
                if len > 0 {
                    let at = self.below(len);
                    out[at] = self.next() as u8;
                }
            }
            3 => {
                let n = self.below(len + 9);
                *out = (0..n).map(|_| self.next() as u8).collect();
            }
            _ => {
                let n = 1 + self.below(4);
                for _ in 0..n {
                    out.push(self.next() as u8);
                }
            }
        }
    }

    fn case(&mut self, bases: &[Vec<u8>]) -> Vec<u8> {
        let mut out = bases[self.below(bases.len())].clone();
        self.mutate_once(&mut out);
        if self.next().is_multiple_of(3) {
            self.mutate_once(&mut out);
        }
        out
    }
}

type Parser = fn(&[u8]) -> Option<Vec<u8>>;

fn hello(input: &[u8]) -> Option<Vec<u8>> {
    HelloPayload::parse(input).ok().map(|p| p.encode())
}

fn status(input: &[u8]) -> Option<Vec<u8>> {
    SlotStatus::parse(input).ok().map(|s| {
        let mut out = Vec::new();
        s.encode_into(&mut out);
        out
    })
}

fn msg2_payload(input: &[u8]) -> Option<Vec<u8>> {
    Msg2Payload::parse(input).ok().map(|p| p.encode())
}

fn session(input: &[u8]) -> Option<Vec<u8>> {
    let (&inner, body) = input.split_first()?;
    match SessionMessage::parse(inner, body).ok()? {
        Some(message) => {
            let mut out = vec![1, message.inner_type()];
            out.extend(message.encode_body());
            Some(out)
        }
        None => Some(vec![if wire::is_application(inner) { 2 } else { 3 }]),
    }
}

fn msg1(input: &[u8]) -> Option<Vec<u8>> {
    wire::parse_msg1(input).ok().map(|m| {
        let mut out = m.dev_id.to_vec();
        out.extend_from_slice(&m.client_nonce.to_le_bytes());
        out.extend_from_slice(m.noise);
        out
    })
}

fn msg2(input: &[u8]) -> Option<Vec<u8>> {
    wire::parse_msg2(input).ok().map(<[u8]>::to_vec)
}

fn record_header(input: &[u8]) -> Option<Vec<u8>> {
    RecordHeader::parse(input).ok().map(|h| {
        let mut out = h.session_id.to_le_bytes().to_vec();
        out.extend_from_slice(&h.counter.to_le_bytes());
        out
    })
}

fn classify(input: &[u8]) -> Option<Vec<u8>> {
    Some(vec![match wire::classify(input) {
        V3Kind::Handshake1 => 0,
        V3Kind::Handshake2 => 1,
        V3Kind::Sealed => 2,
        V3Kind::Other => 3,
    }])
}

fn run(
    name: &'static str,
    seed: u64,
    bases: Vec<Vec<u8>>,
    parse: Parser,
    out: &mut Vec<(String, Vec<u8>)>,
) {
    let mut gen = Gen(seed);
    let mut inputs = Context::new(&SHA256);
    let mut outputs = Context::new(&SHA256);
    let mut verdicts = vec![0u8; CASES.div_ceil(8)];
    let mut accepted = 0u32;
    for index in 0..CASES {
        let input = gen.case(&bases);
        inputs.update(&(input.len() as u32).to_le_bytes());
        inputs.update(&input);
        if let Some(output) = parse(&input) {
            verdicts[index / 8] |= 1 << (index % 8);
            accepted += 1;
            outputs.update(&(output.len() as u32).to_le_bytes());
            outputs.update(&output);
        }
    }
    // Every parser both accepts and rejects some inputs (classify always
    // accepts), so the bitmap is never trivially all-one or all-zero.
    assert!(accepted > 0, "{name}");
    out.push((format!("{name}.seed"), seed.to_le_bytes().to_vec()));
    out.push((
        format!("{name}.inputs_sha256"),
        inputs.finish().as_ref().to_vec(),
    ));
    out.push((format!("{name}.accepted"), verdicts));
    out.push((
        format!("{name}.outputs_sha256"),
        outputs.finish().as_ref().to_vec(),
    ));
}

fn differential() -> Vec<(String, Vec<u8>)> {
    let s = load("session_golden.txt");
    let c = load("client_golden.txt");
    let mut out = Vec::new();
    run(
        "hello",
        0x1001,
        vec![
            s["input.hello_payload"].clone(),
            c["hello_long_name.payload"].clone(),
            HelloPayload::default().encode(),
        ],
        hello,
        &mut out,
    );
    run(
        "status",
        0x1002,
        vec![
            c["status.full"].clone(),
            c["status.empty"].clone(),
            c["reject_parse.status.two_active"].clone(),
        ],
        status,
        &mut out,
    );
    run(
        "msg2_payload",
        0x1003,
        vec![
            s["input.accept_payload"].clone(),
            c["input.accept_max_payload"].clone(),
            s["input.stale_payload"].clone(),
            c["input.too_new_payload"].clone(),
            c["input.pair_again_payload"].clone(),
        ],
        msg2_payload,
        &mut out,
    );
    let with_inner = |inner: u8, body: &[u8]| {
        let mut v = vec![inner];
        v.extend_from_slice(body);
        v
    };
    run(
        "session",
        0x1004,
        vec![
            with_inner(wire::INNER_SESSION_CONFIRM_ACK, &c["status.full"]),
            with_inner(wire::INNER_SESSION_REFUSED, &c["status.empty"]),
            with_inner(wire::INNER_BYE, &[2]),
            with_inner(wire::INNER_SESSION_CONFIRM, &[]),
            with_inner(0x12, &[0; 8]),
        ],
        session,
        &mut out,
    );
    run("msg1", 0x1005, vec![s["msg1"].clone()], msg1, &mut out);
    run(
        "msg2",
        0x1006,
        vec![s["msg2"].clone(), c["msg2_accept_max"].clone()],
        msg2,
        &mut out,
    );
    run(
        "record_header",
        0x1007,
        vec![s["record.input"].clone(), s["record.confirm"].clone()],
        record_header,
        &mut out,
    );
    run(
        "classify",
        0x1008,
        vec![
            s["msg1"].clone(),
            s["msg2"].clone(),
            s["record.input"].clone(),
            s["reject_framing.msg1_protocol_2"].clone(),
        ],
        classify,
        &mut out,
    );
    out
}

#[test]
fn parser_differential_matches_golden_file() {
    check(
        "parser_differential.txt",
        "# v3 differential parser vectors. One `name<space>lowercase-hex` per line.\n\
         # Per parser: the generator seed (u64), a SHA-256 over every input\n\
         # (u32 length || bytes), the accept bitmap (bit i of byte i/8 = case i),\n\
         # and a SHA-256 over each accepted input's canonical output (u32 length ||\n\
         # bytes). The generator and outputs are described in\n\
         # proto/src/v3/golden_differential.rs.\n\
         # Regenerate only on a deliberate wire change:\n\
         # UPDATE_GOLDEN=1 cargo test -p eternal-wire --lib v3::golden_differential\n",
        differential(),
    );
}
