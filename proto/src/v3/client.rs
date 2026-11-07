//! The iPad's side of v3 reconnects, as a sans-IO state machine. It is the
//! reference the Swift implementation follows, and the peer the host table
//! is tested against.
//!
//! One attempt runs at a time:
//!
//! - **Connecting.** Message 1 goes out, and the same bytes are resent every
//!   250 ms for at most 2 s. Then the attempt has failed and the caller picks
//!   the next key.
//! - **Confirming.** Message 2 arrived. That is the input cutover: the caller
//!   releases every held touch, key and button locally, and nothing is sealed
//!   until the new session is live. `SessionConfirm` goes out under the new
//!   keys, freshly sealed every 100 ms for at most 2 s.
//! - The new session goes live on `SessionConfirmAck`, or on the first other
//!   valid record under it (confirms then continue until the ack or the end
//!   of the window).
//! - If the confirm window runs out, the iPad starts a new attempt but keeps
//!   the old attempt's receive key for the host's pending lifetime plus a
//!   margin ([`ClientConfig::abandoned_keep`]). The host may still commit or
//!   refuse that attempt; its ack, media or refusal is then still handled,
//!   and nothing is ever sealed under it. The old session drains: its media is still delivered
//!   until the new session's first IDR decodes, for at most 1 s.
//!
//! Only input stops across the cutover. Reports, heartbeats and anything
//! else keep flowing on the old session, so it stays alive while a
//! confirmation is retried.
//!
//! 2 s + 2 s is under the host's 5 s pending lifetime, so every retry goes out
//! while the pending session it targets still exists.

use std::time::Duration;

use super::handshake::Initiator;
use super::record::{Opener, RecordHeader, Sealer};
use super::wire::{self, Accept, HelloPayload, Msg2Payload, SessionMessage, SlotStatus, V3Kind};
use super::{Id16, Key32, Random, MAX_RECORDS_PER_DIRECTION, MAX_TIMESTAMP_US, SESSION_MAX_AGE};
use crate::v2::PacketType;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub host_id: Id16,
    pub msg1_retry_interval: Duration,
    /// How long one attempt sends message 1.
    pub msg1_window: Duration,
    pub confirm_retry_interval: Duration,
    /// How long `SessionConfirm` is retried after message 2.
    pub confirm_window: Duration,
    /// Longest an old session's media is still shown after cutover.
    pub drain_limit: Duration,
    pub max_age: Duration,
    /// Reconnect this long before the session's maximum age.
    pub reconnect_margin: Duration,
    /// No authenticated record on the live session for this long ends it,
    /// unless the host's Accept gives its own `liveness_timeout_ms`.
    pub liveness_timeout: Duration,
    /// How long, from message 2, an attempt whose confirm window ran out
    /// keeps its receive key. A `SessionConfirmAck`, a refusal or media under
    /// it in that time is still handled. The host decides within its 5 s
    /// pending lifetime, may take up to 5 s more to seal a refusal after
    /// retiring the record, and the answer needs time in flight: 5 + 5 + 1.
    pub abandoned_keep: Duration,
}

impl ClientConfig {
    pub fn new(host_id: Id16) -> Self {
        Self {
            host_id,
            msg1_retry_interval: Duration::from_millis(250),
            msg1_window: Duration::from_secs(2),
            confirm_retry_interval: Duration::from_millis(100),
            confirm_window: Duration::from_secs(2),
            drain_limit: Duration::from_secs(1),
            max_age: SESSION_MAX_AGE,
            reconnect_margin: Duration::from_secs(60),
            liveness_timeout: Duration::from_secs(3),
            abandoned_keep: Duration::from_secs(11),
        }
    }
}

/// One of the iPad's keys for this host. Every copy, clones included,
/// wipes its key on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientKey {
    pub dev_id: Id16,
    pub k_dev: Key32,
}

impl Zeroize for ClientKey {
    fn zeroize(&mut self) {
        self.k_dev.zeroize();
    }
}

impl Drop for ClientKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for ClientKey {}

impl std::fmt::Debug for ClientKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientKey")
            .field("dev_id", &self.dev_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientClose {
    /// The old session's drain ended: the new session's IDR decoded, or 1 s
    /// passed.
    DrainEnded,
    /// The host sent a sealed BYE with this v2 `ByeReason`.
    HostBye(u8),
    /// The session reached its maximum age or counter limit.
    Expired,
    /// Nothing authenticated arrived on it within the host's liveness
    /// timeout: the host has ended it, or the path is gone.
    Liveness,
    /// The app ended it.
    Local,
}

/// `Debug` shows a delivered body's length, not its bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum ClientOutput {
    Send(Vec<u8>),
    /// The input cutover. End every held touch, key and button locally, and
    /// send no release for presses that began before it.
    ReleaseAllInput,
    /// Input and reports go to this session from now on.
    Live {
        session_id: u32,
        accept: Accept,
    },
    /// An authenticated slot status, for the key rule.
    Status {
        dev_id: Id16,
        status: SlotStatus,
    },
    /// The host refused this key's record. Its status tombstones the key;
    /// ask the user to pair again. Any live session is untouched.
    Refused {
        dev_id: Id16,
        status: SlotStatus,
    },
    /// This key can never connect again: pair again, which issues a new
    /// key. Nothing more is attempted under it.
    PairAgain {
        dev_id: Id16,
    },
    /// No usable answer within the attempt's window. Try the next key.
    AttemptFailed {
        dev_id: Id16,
    },
    Deliver {
        session_id: u32,
        counter: u64,
        inner_type: u8,
        body: Vec<u8>,
        /// From the old session, during its drain.
        draining: bool,
    },
    Closed {
        session_id: u32,
        reason: ClientClose,
    },
}

impl std::fmt::Debug for ClientOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send(datagram) => f.debug_tuple("Send").field(&datagram.len()).finish(),
            Self::ReleaseAllInput => f.write_str("ReleaseAllInput"),
            Self::Live { session_id, .. } => f
                .debug_struct("Live")
                .field("session_id", session_id)
                .finish_non_exhaustive(),
            Self::Status { dev_id, status } => f
                .debug_struct("Status")
                .field("dev_id", dev_id)
                .field("status", status)
                .finish(),
            Self::Refused { dev_id, status } => f
                .debug_struct("Refused")
                .field("dev_id", dev_id)
                .field("status", status)
                .finish(),
            Self::PairAgain { dev_id } => {
                f.debug_struct("PairAgain").field("dev_id", dev_id).finish()
            }
            Self::AttemptFailed { dev_id } => f
                .debug_struct("AttemptFailed")
                .field("dev_id", dev_id)
                .finish(),
            Self::Deliver {
                session_id,
                counter,
                inner_type,
                body,
                draining,
            } => f
                .debug_struct("Deliver")
                .field("session_id", session_id)
                .field("counter", counter)
                .field("inner_type", inner_type)
                .field("body_len", &body.len())
                .field("draining", draining)
                .finish(),
            Self::Closed { session_id, reason } => f
                .debug_struct("Closed")
                .field("session_id", session_id)
                .field("reason", reason)
                .finish(),
        }
    }
}

struct Session {
    session_id: u32,
    sealer: Sealer,
    opener: Opener,
    established: Duration,
    /// The last authenticated record from the host, once live.
    last_rx: Duration,
    accept: Accept,
}

enum Attempt {
    Idle,
    Connecting {
        initiator: Box<Initiator>,
        key: ClientKey,
        started: Duration,
        last_sent: Duration,
        stale_retried: bool,
    },
    Confirming {
        key: ClientKey,
        /// Boxed: keys and a replay window are large next to the others.
        session: Box<Session>,
        started: Duration,
        last_sent: Duration,
    },
    /// The session went live on media; confirms continue until the ack.
    Acking {
        session_id: u32,
        started: Duration,
        last_sent: Duration,
    },
}

/// Records left in either direction before the client reconnects on its own.
const RECONNECT_RECORD_MARGIN: u64 = 1 << 20;

pub struct Client {
    config: ClientConfig,
    rng: Box<dyn Random + Send>,
    hello: HelloPayload,
    last_timestamp: u64,
    /// The live session's key, for reconnecting before its limits.
    key: Option<ClientKey>,
    attempt: Attempt,
    live: Option<Session>,
    drain: Option<(Session, Duration)>,
    /// Attempts whose confirm window ran out, oldest first, kept
    /// receive-only: the host may still have committed or refused them.
    abandoned: Vec<Abandoned>,
    /// The `dev_id` the app wants a session under: set by `connect`,
    /// cleared by `disconnect` and by a refusal of that key. An abandoned
    /// attempt is only ever promoted for it.
    intent: Option<Id16>,
    /// From a `TooNew` answer: the clock is used only up to this value.
    clock_limit: Option<u64>,
}

/// A confirming attempt the iPad stopped retrying. Nothing is sealed under
/// it; it can only be promoted by the host's answer, or reported refused.
struct Abandoned {
    key: ClientKey,
    session: Box<Session>,
    until: Duration,
}

/// Abandoned attempts kept at once: a new attempt starts at most every 2 s,
/// so 11 s of retention holds at most 6.
const MAX_ABANDONED: usize = 6;

impl Client {
    /// `hello` holds the session fields; its timestamp is set per attempt.
    pub fn new(config: ClientConfig, hello: HelloPayload, rng: Box<dyn Random + Send>) -> Self {
        Self {
            config,
            rng,
            hello,
            last_timestamp: 0,
            key: None,
            attempt: Attempt::Idle,
            live: None,
            drain: None,
            abandoned: Vec::new(),
            intent: None,
            clock_limit: None,
        }
    }

    /// The timestamp sent last. Persist it per host and restore it with
    /// [`set_last_timestamp`](Self::set_last_timestamp).
    pub fn last_timestamp(&self) -> u64 {
        self.last_timestamp
    }

    /// Values at or above [`MAX_TIMESTAMP_US`] are ignored.
    pub fn set_last_timestamp(&mut self, timestamp_us: u64) {
        if timestamp_us < MAX_TIMESTAMP_US {
            self.last_timestamp = self.last_timestamp.max(timestamp_us);
        }
    }

    /// The next message 1 timestamp: `max(clock, last + 1)`, using only
    /// values below [`MAX_TIMESTAMP_US`]. An out-of-range clock, or one past
    /// a host's `TooNew` limit, is ignored. The sequence never restarts:
    /// once `last + 1` would reach the bound it stays at the last valid
    /// value, and the host answers `PairAgain`.
    fn next_timestamp(&self, clock_us: u64) -> u64 {
        let clock = Some(clock_us)
            .filter(|&c| c < MAX_TIMESTAMP_US)
            .filter(|&c| self.clock_limit.is_none_or(|limit| c <= limit));
        let after_last = self
            .last_timestamp
            .checked_add(1)
            .filter(|&t| t < MAX_TIMESTAMP_US)
            .unwrap_or(MAX_TIMESTAMP_US - 1);
        clock.map_or(after_last, |c| c.max(after_last))
    }

    /// Abandoned attempts currently kept.
    pub fn abandoned_count(&self) -> usize {
        self.abandoned.len()
    }

    pub fn live_session(&self) -> Option<u32> {
        self.live.as_ref().map(|s| s.session_id)
    }

    pub fn draining_session(&self) -> Option<u32> {
        self.drain.as_ref().map(|(s, _)| s.session_id)
    }

    pub fn is_connecting(&self) -> bool {
        matches!(self.attempt, Attempt::Connecting { .. })
    }

    pub fn is_confirming(&self) -> bool {
        matches!(self.attempt, Attempt::Confirming { .. })
    }

    /// Starts an attempt with `key`. An attempt in progress ends the way
    /// every attempt ends ([`abandon_current`](Self::abandon_current)), and
    /// a live session keeps working until the cutover. Abandoned attempts
    /// under keys other than `key` and the live session's key are dropped.
    pub fn connect(&mut self, now: Duration, key: ClientKey, clock_us: u64) -> Vec<ClientOutput> {
        let mut out = Vec::new();
        self.begin_attempt(now, key.clone(), clock_us, false, &mut out);
        let live_key = self.key.as_ref().map(|k| k.dev_id);
        self.abandoned
            .retain(|a| a.key.dev_id == key.dev_id || Some(a.key.dev_id) == live_key);
        out
    }

    /// Ends the current attempt. A confirming attempt becomes abandoned:
    /// receive-only, because the host may still commit or refuse it. Any
    /// other attempt simply ends; the host can't commit without a confirm.
    /// Every path that ends or replaces an attempt goes through here.
    fn abandon_current(&mut self) {
        if let Attempt::Confirming {
            key,
            session,
            started,
            ..
        } = std::mem::replace(&mut self.attempt, Attempt::Idle)
        {
            if self.abandoned.len() >= MAX_ABANDONED {
                self.abandoned.remove(0);
            }
            self.abandoned.push(Abandoned {
                key,
                session,
                until: started + self.config.abandoned_keep,
            });
        }
    }

    /// The one way an attempt starts: the current one ends through
    /// [`abandon_current`](Self::abandon_current), and `key` becomes the
    /// intent.
    fn begin_attempt(
        &mut self,
        now: Duration,
        key: ClientKey,
        clock_us: u64,
        stale_retried: bool,
        out: &mut Vec<ClientOutput>,
    ) {
        self.abandon_current();
        self.intent = Some(key.dev_id);
        self.start_attempt(now, key, clock_us, stale_retried, out);
    }

    /// An attempt got no usable answer. The intent falls back to the live
    /// session's key, so a late answer for it can still be used.
    fn fail_attempt(&mut self, dev_id: Id16, out: &mut Vec<ClientOutput>) {
        self.attempt = Attempt::Idle;
        self.intent = self.key.as_ref().map(|k| k.dev_id);
        out.push(ClientOutput::AttemptFailed { dev_id });
    }

    /// The host refused this key, or said it must be paired again: stop
    /// using it. Ends an attempt under it, drops every abandoned attempt
    /// under it, and forgets it as the reconnect key.
    fn retire_key(&mut self, dev_id: &Id16) {
        let attempt_key = match &self.attempt {
            Attempt::Connecting { key, .. } | Attempt::Confirming { key, .. } => Some(key.dev_id),
            _ => None,
        };
        if attempt_key == Some(*dev_id) {
            // A refused key's attempt isn't kept: nothing more can come of it.
            self.attempt = Attempt::Idle;
        }
        self.abandoned.retain(|a| a.key.dev_id != *dev_id);
        if self.key.as_ref().is_some_and(|k| k.dev_id == *dev_id) {
            self.key = None;
        }
        if self.intent == Some(*dev_id) {
            self.intent = self.key.as_ref().map(|k| k.dev_id);
        }
    }

    fn start_attempt(
        &mut self,
        now: Duration,
        key: ClientKey,
        clock_us: u64,
        stale_retried: bool,
        out: &mut Vec<ClientOutput>,
    ) {
        let timestamp = self.next_timestamp(clock_us);
        self.last_timestamp = timestamp;
        let mut nonce = [0u8; 4];
        self.rng.fill(&mut nonce);
        let mut hello = self.hello.clone();
        hello.timestamp_us = timestamp;
        let initiator = Initiator::start(
            &key.k_dev,
            &key.dev_id,
            &self.config.host_id,
            u32::from_le_bytes(nonce),
            &hello,
        )
        .expect("message 1 always fits");
        out.push(ClientOutput::Send(initiator.msg1().to_vec()));
        self.attempt = Attempt::Connecting {
            initiator: Box::new(initiator),
            key,
            started: now,
            last_sent: now,
            stale_retried,
        };
    }

    pub fn handle_datagram(
        &mut self,
        now: Duration,
        datagram: &[u8],
        clock_us: u64,
    ) -> Vec<ClientOutput> {
        let mut out = Vec::new();
        match wire::classify(datagram) {
            V3Kind::Handshake2 => self.handle_msg2(now, datagram, clock_us, &mut out),
            V3Kind::Sealed => self.handle_record(now, datagram, &mut out),
            V3Kind::Handshake1 | V3Kind::Other => {}
        }
        out
    }

    fn handle_msg2(
        &mut self,
        now: Duration,
        datagram: &[u8],
        clock_us: u64,
        out: &mut Vec<ClientOutput>,
    ) {
        let Attempt::Connecting {
            initiator,
            key,
            stale_retried,
            ..
        } = &mut self.attempt
        else {
            return;
        };
        let Ok((reply, keys)) = initiator.read_msg2(datagram) else {
            return;
        };
        let key = key.clone();
        let stale_retried = *stale_retried;
        match (reply, keys) {
            (
                Msg2Payload::Stale {
                    stored_timestamp_us,
                },
                _,
            ) => {
                self.set_last_timestamp(stored_timestamp_us);
                if stale_retried {
                    self.fail_attempt(key.dev_id, out);
                } else {
                    self.begin_attempt(now, key, clock_us, true, out);
                }
            }
            (Msg2Payload::TooNew { limit_us }, _) => {
                // Use timestamps at or below the host's limit from now on.
                self.clock_limit = Some(limit_us);
                self.last_timestamp = self.last_timestamp.min(limit_us.saturating_sub(1));
                if stale_retried {
                    self.fail_attempt(key.dev_id, out);
                } else {
                    self.begin_attempt(now, key, clock_us, true, out);
                }
            }
            (Msg2Payload::PairAgain, _) => {
                self.retire_key(&key.dev_id);
                out.push(ClientOutput::PairAgain { dev_id: key.dev_id });
            }
            (Msg2Payload::Accept(accept), Some(keys)) => {
                out.push(ClientOutput::ReleaseAllInput);
                let (mut sealer, opener) = keys.initiator(accept.session_id);
                if let Some(confirm) = seal_confirm(&mut sealer) {
                    out.push(ClientOutput::Send(confirm));
                }
                self.attempt = Attempt::Confirming {
                    key,
                    session: Box::new(Session {
                        session_id: accept.session_id,
                        sealer,
                        opener,
                        established: now,
                        last_rx: now,
                        accept,
                    }),
                    started: now,
                    last_sent: now,
                };
            }
            (Msg2Payload::Accept(_), None) => unreachable!("an accept always has keys"),
        }
    }

    fn handle_record(&mut self, now: Duration, datagram: &[u8], out: &mut Vec<ClientOutput>) {
        let Ok(header) = RecordHeader::parse(datagram) else {
            return;
        };
        let session_id = header.session_id;

        if let Attempt::Confirming { key, session, .. } = &self.attempt {
            if session.session_id == session_id {
                let Ok(opened) = session.opener.open(datagram) else {
                    return;
                };
                let dev_id = key.dev_id;
                match SessionMessage::parse(opened.inner_type, &opened.body) {
                    Ok(Some(SessionMessage::ConfirmAck(status))) => {
                        self.promote(now, out);
                        self.attempt = Attempt::Idle;
                        out.push(ClientOutput::Status { dev_id, status });
                    }
                    Ok(Some(SessionMessage::Refused(status))) => {
                        self.retire_key(&dev_id);
                        out.push(ClientOutput::Refused { dev_id, status });
                    }
                    Ok(None) => {
                        let Attempt::Confirming {
                            started, last_sent, ..
                        } = self.attempt
                        else {
                            unreachable!()
                        };
                        self.promote(now, out);
                        self.attempt = Attempt::Acking {
                            session_id,
                            started,
                            last_sent,
                        };
                        out.push(ClientOutput::Deliver {
                            session_id,
                            counter: opened.counter,
                            inner_type: opened.inner_type,
                            body: opened.body,
                            draining: false,
                        });
                    }
                    Ok(Some(_)) | Err(_) => {}
                }
                return;
            }
        }

        if let Some(index) = self
            .abandoned
            .iter()
            .position(|a| a.session.session_id == session_id)
        {
            let Ok(opened) = self.abandoned[index].session.opener.open(datagram) else {
                return;
            };
            let dev_id = self.abandoned[index].key.dev_id;
            match SessionMessage::parse(opened.inner_type, &opened.body) {
                Ok(Some(SessionMessage::ConfirmAck(status))) => {
                    if self.promote_abandoned(now, index, out) {
                        out.push(ClientOutput::Status { dev_id, status });
                    }
                }
                Ok(Some(SessionMessage::Refused(status))) => {
                    self.retire_key(&dev_id);
                    out.push(ClientOutput::Refused { dev_id, status });
                }
                Ok(None) => {
                    if !self.promote_abandoned(now, index, out) {
                        return;
                    }
                    out.push(ClientOutput::Deliver {
                        session_id,
                        counter: opened.counter,
                        inner_type: opened.inner_type,
                        body: opened.body,
                        draining: false,
                    });
                }
                Ok(Some(_)) | Err(_) => {}
            }
            return;
        }

        if let Some(live) = self.live.as_mut().filter(|s| s.session_id == session_id) {
            let Ok(opened) = live.opener.open(datagram) else {
                return;
            };
            live.last_rx = now;
            match SessionMessage::parse(opened.inner_type, &opened.body) {
                Ok(None) => out.push(ClientOutput::Deliver {
                    session_id,
                    counter: opened.counter,
                    inner_type: opened.inner_type,
                    body: opened.body,
                    draining: false,
                }),
                Ok(Some(SessionMessage::ConfirmAck(status))) => {
                    if matches!(self.attempt, Attempt::Acking { session_id: s, .. } if s == session_id)
                    {
                        self.attempt = Attempt::Idle;
                    }
                    if let Some(key) = &self.key {
                        out.push(ClientOutput::Status {
                            dev_id: key.dev_id,
                            status,
                        });
                    }
                }
                Ok(Some(SessionMessage::Bye(reason))) => {
                    self.live = None;
                    if matches!(self.attempt, Attempt::Acking { .. }) {
                        self.attempt = Attempt::Idle;
                    }
                    out.push(ClientOutput::Closed {
                        session_id,
                        reason: ClientClose::HostBye(reason),
                    });
                }
                Ok(Some(_)) | Err(_) => {}
            }
            return;
        }

        if let Some((drain, _)) = self
            .drain
            .as_ref()
            .filter(|(s, _)| s.session_id == session_id)
        {
            let Ok(opened) = drain.opener.open(datagram) else {
                return;
            };
            if let Ok(None) = SessionMessage::parse(opened.inner_type, &opened.body) {
                out.push(ClientOutput::Deliver {
                    session_id,
                    counter: opened.counter,
                    inner_type: opened.inner_type,
                    body: opened.body,
                    draining: true,
                });
            }
        }
        // Anything else is for keys this side no longer has: drop.
    }

    /// Makes the confirming session live.
    fn promote(&mut self, now: Duration, out: &mut Vec<ClientOutput>) {
        let Attempt::Confirming { .. } = self.attempt else {
            return;
        };
        let Attempt::Confirming { session, key, .. } =
            std::mem::replace(&mut self.attempt, Attempt::Idle)
        else {
            unreachable!()
        };
        // Every abandoned attempt is older than this one.
        self.abandoned.clear();
        self.go_live(now, *session, key, out);
    }

    /// The host answered an abandoned attempt: it committed it after all.
    /// Older abandoned attempts are dropped; a message 1 still unanswered is
    /// no longer needed.
    ///
    /// Only for the key the app currently wants a session under. Being a
    /// new cutover, it releases held input first, like message 2 does.
    /// Returns whether it promoted.
    fn promote_abandoned(
        &mut self,
        now: Duration,
        index: usize,
        out: &mut Vec<ClientOutput>,
    ) -> bool {
        if self.intent != Some(self.abandoned[index].key.dev_id) {
            return false;
        }
        let promoted = self.abandoned.remove(index);
        self.abandoned.drain(..index);
        if self.is_connecting() {
            self.attempt = Attempt::Idle;
        }
        out.push(ClientOutput::ReleaseAllInput);
        self.go_live(now, *promoted.session, promoted.key.clone(), out);
        true
    }

    /// Makes `session` live and moves the old one to the drain. A drain
    /// still running ends at once: there is at most one.
    fn go_live(
        &mut self,
        now: Duration,
        session: Session,
        key: ClientKey,
        out: &mut Vec<ClientOutput>,
    ) {
        if let Some((old, _)) = self.drain.take() {
            out.push(ClientOutput::Closed {
                session_id: old.session_id,
                reason: ClientClose::DrainEnded,
            });
        }
        if let Some(old) = self.live.take() {
            self.drain = Some((old, now));
        }
        out.push(ClientOutput::Live {
            session_id: session.session_id,
            accept: session.accept.clone(),
        });
        let mut session = session;
        session.last_rx = now;
        self.live = Some(session);
        self.key = Some(key);
    }

    /// The new session's first IDR frame decoded: drop the old session.
    pub fn first_idr_decoded(&mut self, session_id: u32) -> Vec<ClientOutput> {
        let mut out = Vec::new();
        if self.live_session() == Some(session_id) {
            if let Some((old, _)) = self.drain.take() {
                out.push(ClientOutput::Closed {
                    session_id: old.session_id,
                    reason: ClientClose::DrainEnded,
                });
            }
        }
        out
    }

    /// Seals an application message under the live session. `None` without
    /// one, and for input while confirming: no input crosses the cutover.
    pub fn seal(&mut self, now: Duration, inner_type: u8, body: &[u8]) -> Option<Vec<u8>> {
        if self.is_confirming() && inner_type == PacketType::InputEvent as u8 {
            return None;
        }
        let live = self.live.as_mut()?;
        if now.saturating_sub(live.established) >= self.config.max_age {
            return None;
        }
        live.sealer.seal(inner_type, body).ok()
    }

    /// Ends the live session with a sealed BYE.
    /// Abandoned attempts go too, and their keys are wiped, so a late commit
    /// of one can't bring the session back.
    pub fn disconnect(&mut self, reason: u8) -> Vec<ClientOutput> {
        let mut out = Vec::new();
        self.abandon_current();
        self.abandoned.clear();
        self.intent = None;
        if let Some(mut live) = self.live.take() {
            let bye = SessionMessage::Bye(reason);
            if let Ok(record) = live.sealer.seal(bye.inner_type(), &bye.encode_body()) {
                out.push(ClientOutput::Send(record));
            }
            out.push(ClientOutput::Closed {
                session_id: live.session_id,
                reason: ClientClose::Local,
            });
        }
        out
    }

    /// Runs the timers. Call it often; every 10-50 ms is plenty.
    pub fn poll(&mut self, now: Duration, clock_us: u64) -> Vec<ClientOutput> {
        let mut out = Vec::new();
        match &mut self.attempt {
            Attempt::Idle => {}
            Attempt::Connecting {
                initiator,
                key,
                started,
                last_sent,
                ..
            } => {
                if now.saturating_sub(*started) >= self.config.msg1_window {
                    let dev_id = key.dev_id;
                    self.fail_attempt(dev_id, &mut out);
                } else if now.saturating_sub(*last_sent) >= self.config.msg1_retry_interval {
                    *last_sent = now;
                    out.push(ClientOutput::Send(initiator.msg1().to_vec()));
                }
            }
            Attempt::Confirming {
                key,
                session,
                started,
                last_sent,
            } => {
                if now.saturating_sub(*started) >= self.config.confirm_window {
                    // Stop confirming and start over; the attempt is kept
                    // receive-only by `abandon_current`.
                    let key = key.clone();
                    self.begin_attempt(now, key, clock_us, false, &mut out);
                } else if now.saturating_sub(*last_sent) >= self.config.confirm_retry_interval {
                    *last_sent = now;
                    if let Some(confirm) = seal_confirm(&mut session.sealer) {
                        out.push(ClientOutput::Send(confirm));
                    }
                }
            }
            Attempt::Acking {
                session_id,
                started,
                last_sent,
            } => {
                if now.saturating_sub(*started) >= self.config.confirm_window {
                    // The next HelloAck brings the status.
                    self.attempt = Attempt::Idle;
                } else if now.saturating_sub(*last_sent) >= self.config.confirm_retry_interval {
                    *last_sent = now;
                    let id = *session_id;
                    if let Some(live) = self.live.as_mut().filter(|s| s.session_id == id) {
                        if let Some(confirm) = seal_confirm(&mut live.sealer) {
                            out.push(ClientOutput::Send(confirm));
                        }
                    }
                }
            }
        }

        self.abandoned.retain(|a| now < a.until);

        if let Some((old, since)) = &self.drain {
            if now.saturating_sub(*since) >= self.config.drain_limit {
                out.push(ClientOutput::Closed {
                    session_id: old.session_id,
                    reason: ClientClose::DrainEnded,
                });
                self.drain = None;
            }
        }

        if let Some(live) = &self.live {
            let timeout = match live.accept.liveness_timeout_ms {
                0 => self.config.liveness_timeout,
                ms => Duration::from_millis(u64::from(ms)),
            };
            if now.saturating_sub(live.last_rx) >= timeout {
                // The host has ended the session, or the path is gone: stop
                // treating it as live, and reconnect if it's still wanted.
                let session_id = live.session_id;
                self.live = None;
                out.push(ClientOutput::Closed {
                    session_id,
                    reason: ClientClose::Liveness,
                });
                let wanted = self.key.clone().filter(|k| self.intent == Some(k.dev_id));
                if let (Some(key), Attempt::Idle) = (wanted, &self.attempt) {
                    self.begin_attempt(now, key, clock_us, false, &mut out);
                }
                return out;
            }
        }

        if let Some(live) = &self.live {
            let age = now.saturating_sub(live.established);
            // Authenticated progress in each direction: sealed here, opened
            // from the host.
            let sent = live.sealer.sealed();
            let received = live.opener.highest().map_or(0, |top| top + 1);
            let used = sent.max(received);
            if age >= self.config.max_age || used >= MAX_RECORDS_PER_DIRECTION {
                out.push(ClientOutput::Closed {
                    session_id: live.session_id,
                    reason: ClientClose::Expired,
                });
                self.live = None;
            } else if matches!(self.attempt, Attempt::Idle)
                && (age + self.config.reconnect_margin >= self.config.max_age
                    || used + RECONNECT_RECORD_MARGIN >= MAX_RECORDS_PER_DIRECTION)
            {
                if let Some(key) = self.key.clone() {
                    self.begin_attempt(now, key, clock_us, false, &mut out);
                }
            }
        }
        out
    }
}

fn seal_confirm(sealer: &mut Sealer) -> Option<Vec<u8>> {
    let confirm = SessionMessage::Confirm;
    sealer
        .seal(confirm.inner_type(), &confirm.encode_body())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v3::handshake::Responder;
    use crate::v3::SeededRandom;

    const HOST: Id16 = [0xB2; 16];
    const T0: Duration = Duration::from_secs(50);

    fn key() -> ClientKey {
        ClientKey {
            dev_id: [0xD2; 16],
            k_dev: [0x24; 32],
        }
    }

    fn client() -> Client {
        Client::new(
            ClientConfig::new(HOST),
            HelloPayload::default(),
            Box::new(SeededRandom::new(3)),
        )
    }

    fn sent(out: &[ClientOutput]) -> Vec<Vec<u8>> {
        out.iter()
            .filter_map(|o| match o {
                ClientOutput::Send(d) => Some(d.clone()),
                _ => None,
            })
            .collect()
    }

    /// Connects through a hand-driven host side and returns the host's
    /// sealer for the live session.
    fn live_client() -> (Client, Sealer) {
        let mut ipad = client();
        let msg1 = sent(&ipad.connect(T0, key(), 1_000)).remove(0);
        let parsed = wire::parse_msg1(&msg1).unwrap();
        let host = Responder::read_msg1(&parsed, &key().k_dev, &HOST).unwrap();
        let accept = Msg2Payload::Accept(Accept {
            session_id: 77,
            ..Default::default()
        });
        let (msg2, keys) = host.write_msg2(&accept).unwrap();
        let (mut host_sealer, _) = keys.unwrap().responder(77);
        ipad.handle_datagram(T0, &msg2, 1_000);
        let ack = SessionMessage::ConfirmAck(SlotStatus::default());
        let record = host_sealer
            .seal(ack.inner_type(), &ack.encode_body())
            .unwrap();
        ipad.handle_datagram(T0, &record, 1_000);
        assert_eq!(ipad.live_session(), Some(77));
        (ipad, host_sealer)
    }

    #[test]
    fn out_of_range_timestamps_are_ignored_never_clamped() {
        let mut ipad = client();
        ipad.set_last_timestamp(u64::MAX);
        ipad.set_last_timestamp(MAX_TIMESTAMP_US);
        assert_eq!(ipad.last_timestamp(), 0, "out-of-range values are ignored");
        // An out-of-range clock: last + 1 instead.
        ipad.set_last_timestamp(500);
        assert_eq!(ipad.next_timestamp(u64::MAX), 501);
        // At the ceiling the sequence stays at the last valid value; it
        // never restarts.
        ipad.set_last_timestamp(MAX_TIMESTAMP_US - 1);
        assert_eq!(ipad.next_timestamp(1_000), MAX_TIMESTAMP_US - 1);
        assert_eq!(ipad.next_timestamp(u64::MAX), MAX_TIMESTAMP_US - 1);
        let msg1 = sent(&ipad.connect(T0, key(), u64::MAX)).remove(0);
        let parsed = wire::parse_msg1(&msg1).unwrap();
        let host = Responder::read_msg1(&parsed, &key().k_dev, &HOST).unwrap();
        assert!(host.payload().timestamp_us < MAX_TIMESTAMP_US);
    }

    #[test]
    fn too_new_caps_the_clock_and_retries_once() {
        let mut ipad = client();
        let msg1 = sent(&ipad.connect(T0, key(), 9_000_000)).remove(0);
        let parsed = wire::parse_msg1(&msg1).unwrap();
        let host = Responder::read_msg1(&parsed, &key().k_dev, &HOST).unwrap();
        let (msg2, _) = host
            .write_msg2(&Msg2Payload::TooNew {
                limit_us: 5_000_000,
            })
            .unwrap();
        let out = ipad.handle_datagram(T0, &msg2, 9_000_000);
        let retry = sent(&out).remove(0);
        let parsed = wire::parse_msg1(&retry).unwrap();
        let host = Responder::read_msg1(&parsed, &key().k_dev, &HOST).unwrap();
        assert!(host.payload().timestamp_us <= 5_000_000);
        assert!(ipad.next_timestamp(9_000_000) <= 5_000_000 + 1);
    }

    #[test]
    fn pair_again_retires_the_key() {
        let mut ipad = client();
        let msg1 = sent(&ipad.connect(T0, key(), 1_000)).remove(0);
        let parsed = wire::parse_msg1(&msg1).unwrap();
        let host = Responder::read_msg1(&parsed, &key().k_dev, &HOST).unwrap();
        let (msg2, _) = host.write_msg2(&Msg2Payload::PairAgain).unwrap();
        let out = ipad.handle_datagram(T0, &msg2, 1_000);
        assert!(out.contains(&ClientOutput::PairAgain {
            dev_id: key().dev_id
        }));
        assert!(!ipad.is_connecting());
        assert!(
            ipad.poll(T0 + Duration::from_secs(3), 1_000).is_empty(),
            "no AttemptFailed, no retry"
        );
    }

    #[test]
    fn the_receive_direction_near_its_limit_starts_a_reconnect_and_ends_at_it() {
        let (mut ipad, mut host_sealer) = live_client();
        // An unauthenticated record with a high counter changes nothing.
        host_sealer.skip_to(MAX_RECORDS_PER_DIRECTION - 10);
        let mut forged = host_sealer.seal(0x01, &[0; 8]).unwrap();
        *forged.last_mut().unwrap() ^= 1;
        ipad.handle_datagram(T0, &forged, 1_000);
        assert!(sent(&ipad.poll(T0 + Duration::from_millis(10), 1_000)).is_empty());

        // Authenticated records inside the margin start a reconnect.
        host_sealer.skip_to(MAX_RECORDS_PER_DIRECTION - RECONNECT_RECORD_MARGIN);
        let near = host_sealer.seal(0x01, &[0; 8]).unwrap();
        ipad.handle_datagram(T0, &near, 1_000);
        let out = ipad.poll(T0 + Duration::from_millis(20), 1_000);
        assert!(ipad.is_connecting());
        assert_eq!(sent(&out).len(), 1, "message 1 of the new attempt");
        assert_eq!(ipad.live_session(), Some(77));

        // The very last counter ends the session.
        host_sealer.skip_to(MAX_RECORDS_PER_DIRECTION - 1);
        let last = host_sealer.seal(0x01, &[0; 8]).unwrap();
        ipad.handle_datagram(T0, &last, 1_000);
        let out = ipad.poll(T0 + Duration::from_millis(30), 1_000);
        assert!(out.contains(&ClientOutput::Closed {
            session_id: 77,
            reason: ClientClose::Expired
        }));
        assert_eq!(ipad.live_session(), None);
    }

    #[test]
    fn while_confirming_only_input_is_held_back() {
        let (mut ipad, _) = live_client();
        // A second attempt reaches confirming.
        let msg1 = sent(&ipad.connect(T0, key(), 2_000)).remove(0);
        let parsed = wire::parse_msg1(&msg1).unwrap();
        let host = Responder::read_msg1(&parsed, &key().k_dev, &HOST).unwrap();
        let (msg2, _) = host
            .write_msg2(&Msg2Payload::Accept(Accept {
                session_id: 78,
                ..Default::default()
            }))
            .unwrap();
        ipad.handle_datagram(T0, &msg2, 2_000);
        assert!(ipad.is_confirming());
        assert!(ipad
            .seal(T0, PacketType::InputEvent as u8, &[0; 30])
            .is_none());
        assert!(ipad
            .seal(T0, PacketType::ReceiverReport as u8, &[0; 36])
            .is_some());
        assert!(ipad.seal(T0, PacketType::Ping as u8, &[0; 8]).is_some());
    }

    #[test]
    fn keys_wipe_themselves() {
        fn wipes_on_drop<T: ZeroizeOnDrop>() {}
        wipes_on_drop::<ClientKey>();
        let mut copy = key().clone();
        copy.zeroize();
        assert_eq!(copy.k_dev, [0; 32]);
        assert_eq!(copy.dev_id, key().dev_id, "the id isn't secret");
        assert!(
            !format!("{:?}", key()).contains("36"),
            "Debug hides the key"
        );
    }

    #[test]
    fn debug_never_prints_a_delivered_body() {
        let output = ClientOutput::Deliver {
            session_id: 1,
            counter: 0,
            inner_type: 0x01,
            body: b"frame bytes".to_vec(),
            draining: false,
        };
        let text = format!("{output:?}");
        assert!(text.contains("body_len: 11"), "{text}");
        assert!(!text.contains("102, 114"), "{text}");
    }
}
