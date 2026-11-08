//! The reconnect handshake: `Noise_NNpsk0_25519_ChaChaPoly_SHA256` through
//! `snow`, wrapped in the v3 message 1 and message 2 datagrams.
//!
//! The iPad is the initiator and the host the responder. Each attempt uses a
//! fresh ephemeral key, so a recorded message 1 can never complete a session:
//! committing needs records sealed under keys only the ephemeral's owner can
//! derive.

use snow::{Builder, HandshakeState};

use super::record::{wipe, SessionKeys};
use super::wire::{self, HelloPayload, Msg1, Msg2Payload, MAX_NOISE_MESSAGE};
use super::{session_prologue, Id16, Key32, V3Error, NOISE_PARAMS};

pub(super) fn builder<'a>(k_dev: &'a Key32, prologue: &'a [u8]) -> Builder<'a> {
    Builder::new(NOISE_PARAMS.parse().expect("valid Noise params"))
        .psk(0, k_dev)
        .expect("psk slot 0 exists for NNpsk0")
        .prologue(prologue)
        .expect("prologue set once")
}

pub(super) fn map_snow(error: snow::Error) -> V3Error {
    match error {
        snow::Error::Decrypt => V3Error::Auth,
        _ => V3Error::Handshake,
    }
}

pub(super) fn split(state: &mut HandshakeState) -> SessionKeys {
    let (mut k1, mut k2) = state.dangerously_get_raw_split();
    let mut handshake_hash = [0u8; 32];
    handshake_hash.copy_from_slice(state.get_handshake_hash());
    let keys = SessionKeys {
        initiator_to_responder: k1,
        responder_to_initiator: k2,
        handshake_hash,
    };
    wipe(&mut k1);
    wipe(&mut k2);
    keys
}

/// The iPad's side of one attempt, between sending message 1 and reading
/// message 2.
pub struct Initiator {
    state: HandshakeState,
    msg1: Vec<u8>,
}

impl std::fmt::Debug for Initiator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Initiator").finish_non_exhaustive()
    }
}

impl Initiator {
    /// Starts an attempt and returns the message 1 datagram. Retries resend
    /// these same bytes.
    pub fn start(
        k_dev: &Key32,
        dev_id: &Id16,
        host_id: &Id16,
        client_nonce: u32,
        payload: &HelloPayload,
    ) -> Result<Self, V3Error> {
        Self::start_inner(k_dev, dev_id, host_id, client_nonce, payload, None)
    }

    /// [`start`](Self::start) with a fixed ephemeral private key, for golden
    /// transcripts and fuzzing only.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn start_with_ephemeral(
        k_dev: &Key32,
        dev_id: &Id16,
        host_id: &Id16,
        client_nonce: u32,
        payload: &HelloPayload,
        ephemeral: &Key32,
    ) -> Result<Self, V3Error> {
        Self::start_inner(
            k_dev,
            dev_id,
            host_id,
            client_nonce,
            payload,
            Some(ephemeral),
        )
    }

    fn start_inner(
        k_dev: &Key32,
        dev_id: &Id16,
        host_id: &Id16,
        client_nonce: u32,
        payload: &HelloPayload,
        ephemeral: Option<&Key32>,
    ) -> Result<Self, V3Error> {
        let prologue = session_prologue(dev_id, host_id);
        let mut b = builder(k_dev, &prologue);
        if let Some(e) = ephemeral {
            b = b.fixed_ephemeral_key_for_testing_only(e);
        }
        let mut state = b.build_initiator().map_err(map_snow)?;
        let mut noise = vec![0u8; MAX_NOISE_MESSAGE];
        let len = state
            .write_message(&payload.encode(), &mut noise)
            .map_err(map_snow)?;
        noise.truncate(len);
        let msg1 = wire::encode_msg1(client_nonce, payload.listen_port, dev_id, &noise);
        Ok(Self { state, msg1 })
    }

    /// The message 1 datagram.
    pub fn msg1(&self) -> &[u8] {
        &self.msg1
    }

    /// Reads a message 2 datagram. On failure nothing changes, so a forged or
    /// stale message 2 doesn't spoil the real one arriving later. On success
    /// the attempt is over: the keys are returned for an `Accept`, and
    /// dropped for any other answer.
    pub fn read_msg2(
        &mut self,
        datagram: &[u8],
    ) -> Result<(Msg2Payload, Option<SessionKeys>), V3Error> {
        let noise = wire::parse_msg2(datagram)?;
        let mut payload = vec![0u8; MAX_NOISE_MESSAGE];
        let len = self
            .state
            .read_message(noise, &mut payload)
            .map_err(map_snow)?;
        let reply = Msg2Payload::parse(&payload[..len])?;
        let keys = split(&mut self.state);
        Ok(match reply {
            Msg2Payload::Accept(_) => (reply, Some(keys)),
            _ => (reply, None),
        })
    }
}

/// The host's side after reading a valid message 1 and before answering.
pub struct Responder {
    state: HandshakeState,
    payload: HelloPayload,
}

impl std::fmt::Debug for Responder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Responder").finish_non_exhaustive()
    }
}

impl Responder {
    /// Authenticates message 1 against `K_dev`. A wrong key, a tampered byte
    /// or a different `host_id` fails here, as [`V3Error::Auth`].
    pub fn read_msg1(msg1: &Msg1<'_>, k_dev: &Key32, host_id: &Id16) -> Result<Self, V3Error> {
        Self::read_inner(msg1, k_dev, host_id, None)
    }

    /// [`read_msg1`](Self::read_msg1) with a fixed ephemeral private key for
    /// message 2, for golden transcripts and fuzzing only.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn read_msg1_with_ephemeral(
        msg1: &Msg1<'_>,
        k_dev: &Key32,
        host_id: &Id16,
        ephemeral: &Key32,
    ) -> Result<Self, V3Error> {
        Self::read_inner(msg1, k_dev, host_id, Some(ephemeral))
    }

    fn read_inner(
        msg1: &Msg1<'_>,
        k_dev: &Key32,
        host_id: &Id16,
        ephemeral: Option<&Key32>,
    ) -> Result<Self, V3Error> {
        let prologue = session_prologue(&msg1.dev_id, host_id);
        let mut b = builder(k_dev, &prologue);
        if let Some(e) = ephemeral {
            b = b.fixed_ephemeral_key_for_testing_only(e);
        }
        let mut state = b.build_responder().map_err(map_snow)?;
        let mut payload = vec![0u8; MAX_NOISE_MESSAGE];
        let len = state
            .read_message(msg1.noise, &mut payload)
            .map_err(map_snow)?;
        let payload = HelloPayload::parse(&payload[..len])?;
        Ok(Self { state, payload })
    }

    /// The authenticated session fields from message 1.
    pub fn payload(&self) -> &HelloPayload {
        &self.payload
    }

    /// Writes message 2 with `reply` and finishes the handshake. Returns the
    /// datagram, and the session keys for an `Accept`.
    pub fn write_msg2(
        mut self,
        reply: &Msg2Payload,
    ) -> Result<(Vec<u8>, Option<SessionKeys>), V3Error> {
        let mut noise = vec![0u8; MAX_NOISE_MESSAGE];
        let len = self
            .state
            .write_message(&reply.encode(), &mut noise)
            .map_err(map_snow)?;
        noise.truncate(len);
        let datagram = wire::encode_msg2(&noise);
        let keys = split(&mut self.state);
        Ok(match reply {
            Msg2Payload::Accept(_) => (datagram, Some(keys)),
            _ => (datagram, None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v3::wire::{parse_msg1, Accept};

    const K: Key32 = [0x11; 32];
    const DEV: Id16 = [0x22; 16];
    const HOST: Id16 = [0x33; 16];

    fn hello(timestamp_us: u64) -> HelloPayload {
        HelloPayload {
            timestamp_us,
            listen_port: 9876,
            device_name: "iPad".into(),
            ..Default::default()
        }
    }

    fn accept(session_id: u32) -> Msg2Payload {
        Msg2Payload::Accept(Accept {
            session_id,
            host_name: "PC".into(),
            ..Default::default()
        })
    }

    #[test]
    fn both_sides_derive_the_same_directional_keys() {
        let mut ipad = Initiator::start(&K, &DEV, &HOST, 5, &hello(100)).unwrap();
        let msg1 = parse_msg1(ipad.msg1()).unwrap();
        assert_eq!(msg1.dev_id, DEV);
        let host = Responder::read_msg1(&msg1, &K, &HOST).unwrap();
        assert_eq!(host.payload(), &hello(100));
        let (msg2, host_keys) = host.write_msg2(&accept(77)).unwrap();
        let host_keys = host_keys.unwrap();
        let (reply, ipad_keys) = ipad.read_msg2(&msg2).unwrap();
        let ipad_keys = ipad_keys.unwrap();
        assert_eq!(reply, accept(77));
        assert_eq!(
            ipad_keys.initiator_to_responder,
            host_keys.initiator_to_responder
        );
        assert_eq!(
            ipad_keys.responder_to_initiator,
            host_keys.responder_to_initiator
        );
        assert_ne!(
            ipad_keys.initiator_to_responder,
            ipad_keys.responder_to_initiator
        );
        assert_eq!(ipad_keys.handshake_hash, host_keys.handshake_hash);
    }

    #[test]
    fn wrong_key_host_or_dev_id_fails_at_message_1() {
        let ipad = Initiator::start(&K, &DEV, &HOST, 5, &hello(1)).unwrap();
        let msg1 = parse_msg1(ipad.msg1()).unwrap();
        assert_eq!(
            Responder::read_msg1(&msg1, &[0x12; 32], &HOST).unwrap_err(),
            V3Error::Auth
        );
        assert_eq!(
            Responder::read_msg1(&msg1, &K, &[0x34; 16]).unwrap_err(),
            V3Error::Auth
        );
        let mut moved = ipad.msg1().to_vec();
        moved[71] ^= 1; // dev_id is bound through the prologue
        let moved = parse_msg1(&moved).unwrap();
        assert_eq!(
            Responder::read_msg1(&moved, &K, &HOST).unwrap_err(),
            V3Error::Auth
        );
    }

    #[test]
    fn every_noise_byte_of_both_messages_is_authenticated() {
        let ipad = Initiator::start(&K, &DEV, &HOST, 5, &hello(1)).unwrap();
        let good = ipad.msg1().to_vec();
        for at in wire::MSG1_EXTENSION_OFFSET + 4..good.len() {
            let mut bad = good.clone();
            bad[at] ^= 0x80;
            let parsed = parse_msg1(&bad).unwrap();
            assert!(
                Responder::read_msg1(&parsed, &K, &HOST).is_err(),
                "msg1 byte {at}"
            );
        }

        let mut ipad = ipad;
        let host = Responder::read_msg1(&parse_msg1(&good).unwrap(), &K, &HOST).unwrap();
        let (msg2, _) = host.write_msg2(&accept(9)).unwrap();
        for at in 0..msg2.len() {
            let mut bad = msg2.clone();
            bad[at] ^= 0x80;
            assert!(ipad.read_msg2(&bad).is_err(), "msg2 byte {at}");
        }
        // Every failure above restored the state, so the real one still works.
        assert!(ipad.read_msg2(&msg2).is_ok());
    }

    #[test]
    fn a_message_2_for_another_attempt_is_rejected_without_harm() {
        let mut first = Initiator::start(&K, &DEV, &HOST, 1, &hello(1)).unwrap();
        let second = Initiator::start(&K, &DEV, &HOST, 2, &hello(2)).unwrap();
        let host = Responder::read_msg1(&parse_msg1(second.msg1()).unwrap(), &K, &HOST).unwrap();
        let (other_msg2, _) = host.write_msg2(&accept(3)).unwrap();
        assert_eq!(first.read_msg2(&other_msg2).unwrap_err(), V3Error::Auth);
        let host = Responder::read_msg1(&parse_msg1(first.msg1()).unwrap(), &K, &HOST).unwrap();
        let (msg2, _) = host.write_msg2(&accept(4)).unwrap();
        assert!(first.read_msg2(&msg2).unwrap().1.is_some());
    }

    #[test]
    fn stale_reply_completes_without_session_keys() {
        let mut ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(5)).unwrap();
        let host = Responder::read_msg1(&parse_msg1(ipad.msg1()).unwrap(), &K, &HOST).unwrap();
        let reply = Msg2Payload::Stale {
            stored_timestamp_us: 900,
        };
        let (msg2, keys) = host.write_msg2(&reply).unwrap();
        assert!(keys.is_none());
        let (got, keys) = ipad.read_msg2(&msg2).unwrap();
        assert_eq!(got, reply);
        assert!(keys.is_none());
    }

    #[test]
    fn message_sizes_fit_the_smallest_datagram() {
        let mut long = hello(1);
        long.device_name = "n".repeat(64);
        let mut ipad = Initiator::start(&K, &DEV, &HOST, 1, &long).unwrap();
        assert!(
            ipad.msg1().len() <= crate::v2::media::MIN_DGRAM_SIZE,
            "{}",
            ipad.msg1().len()
        );
        let host = Responder::read_msg1(&parse_msg1(ipad.msg1()).unwrap(), &K, &HOST).unwrap();
        let Msg2Payload::Accept(mut a) = accept(1) else {
            unreachable!()
        };
        a.host_name = "h".repeat(64);
        a.slot_status = wire::SlotStatus {
            held: (0..4)
                .map(|i| {
                    (
                        [i; 16],
                        if i == 0 {
                            wire::RecordState::Active
                        } else {
                            wire::RecordState::Pending
                        },
                    )
                })
                .collect(),
            tombstones: (10..18).map(|i| [i; 16]).collect(),
        };
        let (msg2, _) = host.write_msg2(&Msg2Payload::Accept(a)).unwrap();
        // Design bound: about 365 bytes; the smallest datagram is 576.
        assert!(msg2.len() <= 365, "{}", msg2.len());
        assert!(ipad.read_msg2(&msg2).is_ok());
    }
}
