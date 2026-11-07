//! The host's session table for v3 reconnects, as a sans-IO state machine.
//!
//! Callers feed it datagrams and the current time and act on the
//! [`HostOutput`]s it returns. It never reads a clock, a socket or a disk:
//! `now` is any monotonic [`Duration`], so tests drive it on a simulated
//! clock. Durable work (the timestamp, Finalize, Remove) stays with the
//! caller, which answers in steps:
//!
//! 1. [`HostOutput::ConfirmRequested`]: a valid `SessionConfirm` arrived for a
//!    pending session. The caller checks whether its record's Finalize would
//!    conflict, then either
//!    - calls [`HostSessions::commit`], or
//!    - calls [`HostSessions::prepare_refusal`], which keeps only the send key
//!      and destroys the pending session, retires the record durably (the
//!      invalidation procedure, whose step 1 may call
//!      [`HostSessions::revoke`]), and then calls [`HostSessions::refuse`].
//!
//!    The decision must come within the pending session's lifetime. If the
//!    pending session is replaced or times out first, a
//!    [`HostOutput::ConfirmCancelled`] says so, and a late answer does
//!    nothing.
//! 2. [`HostOutput::Committed`]: the old session's keys are gone and the new
//!    one is live. The caller persists the timestamp, runs the input
//!    cutover, sends an IDR, and finalizes a pending record durably.
//! 3. [`HostSessions::ack`]: only then, the `SessionConfirmAck` goes out.
//!
//! Per `dev_id` there is at most one live and one pending session. Every
//! packet for a `session_id` whose keys are gone is dropped without a reply.
//! Application records only flow under a committed session.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use super::handshake::Responder;
use super::record::{Opener, RecordHeader, Sealer};
use super::wire::{
    self, Accept, HelloPayload, Msg1, Msg2Payload, SessionMessage, SlotStatus, V3Kind,
};
use super::{
    Id16, Key32, Random, Zeroizing, MAX_RECORDS_PER_DIRECTION, MAX_TIMESTAMP_US, SESSION_MAX_AGE,
};
use crate::v2::control::ByeReason;

/// Timing and limits. The defaults are the spec's numbers.
#[derive(Debug, Clone)]
pub struct HostConfig {
    pub host_id: Id16,
    /// How long a pending session waits for its `SessionConfirm` and the
    /// caller's decision on it.
    pub pending_lifetime: Duration,
    /// How long a timed-out message 1 is remembered and dropped.
    pub expired_msg1_lifetime: Duration,
    /// Shortest gap between two resends of a cached message 2.
    pub msg2_resend_interval: Duration,
    /// `Stale` answers per `dev_id` per second.
    pub stale_per_second: u32,
    /// Shortest gap between two re-sealed `SessionConfirmAck`s.
    pub confirm_ack_interval: Duration,
    /// How long a sealed `SessionRefused` is resent to its address.
    pub refusal_lifetime: Duration,
    /// Shortest gap between two resends of it.
    pub refusal_resend_interval: Duration,
    /// Message 1s per source per second, before any Diffie-Hellman. The
    /// source is [`PeerAddr::rate_bucket`].
    pub msg1_per_second: u32,
    /// Pending sessions host-wide.
    pub max_pending: usize,
    /// No authenticated packet for this long ends a live session.
    pub liveness_timeout: Duration,
    /// A session's maximum age.
    pub max_age: Duration,
}

impl HostConfig {
    pub fn new(host_id: Id16) -> Self {
        Self {
            host_id,
            pending_lifetime: Duration::from_secs(5),
            expired_msg1_lifetime: Duration::from_secs(30),
            msg2_resend_interval: Duration::from_millis(100),
            stale_per_second: 5,
            confirm_ack_interval: Duration::from_millis(50),
            refusal_lifetime: Duration::from_secs(5),
            refusal_resend_interval: Duration::from_millis(100),
            msg1_per_second: 20,
            max_pending: 16,
            liveness_timeout: Duration::from_secs(3),
            max_age: SESSION_MAX_AGE,
        }
    }
}

/// A peer address. The message 1 rate limit counts per [`rate_bucket`]: the
/// source IP, never the port, so one machine can't spread over many buckets.
///
/// [`rate_bucket`]: PeerAddr::rate_bucket
pub trait PeerAddr: Clone + Eq + Hash {
    type Bucket: Clone + Eq + Hash;
    fn rate_bucket(&self) -> Self::Bucket;
}

impl PeerAddr for SocketAddr {
    type Bucket = IpAddr;

    /// The IPv4 address, or the IPv6 /64 (one host's usual allocation).
    fn rate_bucket(&self) -> IpAddr {
        match self.ip() {
            IpAddr::V4(v4) => IpAddr::V4(v4),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => {
                    let mut segments = v6.segments();
                    segments[4..].fill(0);
                    IpAddr::V6(Ipv6Addr::from(segments))
                }
            },
        }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl PeerAddr for u32 {
    type Bucket = u32;
    fn rate_bucket(&self) -> u32 {
        *self
    }
}

/// What the host's credential store answers. Phase 2 backs this with the
/// record database; tests back it with a map.
pub trait HostCredentials {
    /// `K_dev` for a `dev_id` the host accepts for reconnects (an active or
    /// pending record). `None` drops message 1 before any Diffie-Hellman.
    fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>>;
    /// The last committed timestamp for this `dev_id`, or 0.
    fn last_committed_timestamp(&self, dev_id: &Id16) -> u64;
    /// The HelloAck fields for message 2. `session_id` is filled in here.
    fn accept_template(&self, dev_id: &Id16) -> Accept;
    /// The slot status for a `SessionConfirmAck` or `SessionRefused`.
    fn slot_status(&self, dev_id: &Id16) -> SlotStatus;
}

/// Why a session's keys were destroyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// A newer session for the same `dev_id` committed.
    Superseded,
    /// The host user removed the record.
    Revoked,
    /// No authenticated packet within the liveness timeout.
    Liveness,
    /// The session reached its maximum age.
    Expired,
    /// A direction reached its record counter limit.
    Exhausted,
    /// The iPad sent a sealed BYE.
    PeerBye,
    /// The host is shutting down.
    Shutdown,
}

/// `Debug` shows a delivered body's length, not its bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum HostOutput<A> {
    Send {
        to: A,
        datagram: Vec<u8>,
    },
    /// Answer with [`HostSessions::commit`], or with
    /// [`HostSessions::prepare_refusal`] and [`HostSessions::refuse`].
    ConfirmRequested {
        dev_id: Id16,
        session_id: u32,
    },
    /// A requested decision no longer applies: the pending session was
    /// replaced by a newer attempt, timed out, or was revoked. A later
    /// `commit` or `refuse` for it does nothing.
    ConfirmCancelled {
        dev_id: Id16,
        session_id: u32,
    },
    /// Persist the timestamp, run the input cutover, send an IDR, finalize a
    /// pending record, then call [`HostSessions::ack`].
    Committed {
        dev_id: Id16,
        session_id: u32,
        /// The live session this one replaced; its keys are already gone.
        replaced: Option<u32>,
        timestamp_us: u64,
        hello: HelloPayload,
        peer: A,
    },
    /// An authenticated application message from a live session.
    Deliver {
        dev_id: Id16,
        session_id: u32,
        counter: u64,
        inner_type: u8,
        body: Vec<u8>,
    },
    Closed {
        dev_id: Id16,
        session_id: u32,
        reason: CloseReason,
    },
}

impl<A: std::fmt::Debug> std::fmt::Debug for HostOutput<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send { to, datagram } => f
                .debug_struct("Send")
                .field("to", to)
                .field("len", &datagram.len())
                .finish(),
            Self::ConfirmRequested { dev_id, session_id } => f
                .debug_struct("ConfirmRequested")
                .field("dev_id", dev_id)
                .field("session_id", session_id)
                .finish(),
            Self::ConfirmCancelled { dev_id, session_id } => f
                .debug_struct("ConfirmCancelled")
                .field("dev_id", dev_id)
                .field("session_id", session_id)
                .finish(),
            Self::Committed {
                dev_id,
                session_id,
                replaced,
                timestamp_us,
                peer,
                ..
            } => f
                .debug_struct("Committed")
                .field("dev_id", dev_id)
                .field("session_id", session_id)
                .field("replaced", replaced)
                .field("timestamp_us", timestamp_us)
                .field("peer", peer)
                .finish_non_exhaustive(),
            Self::Deliver {
                dev_id,
                session_id,
                counter,
                inner_type,
                body,
            } => f
                .debug_struct("Deliver")
                .field("dev_id", dev_id)
                .field("session_id", session_id)
                .field("counter", counter)
                .field("inner_type", inner_type)
                .field("body_len", &body.len())
                .finish(),
            Self::Closed {
                dev_id,
                session_id,
                reason,
            } => f
                .debug_struct("Closed")
                .field("dev_id", dev_id)
                .field("session_id", session_id)
                .field("reason", reason)
                .finish(),
        }
    }
}

type Digest = [u8; 32];

/// Identifies a message 1 by its authenticated content only: `dev_id` (bound
/// through the prologue) and the Noise message. The HELLO2 shell around it
/// doesn't count, so changing shell bytes never makes a new message 1.
fn msg1_key(msg1: &Msg1<'_>) -> Digest {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(&msg1.dev_id);
    ctx.update(msg1.noise);
    let mut out = [0u8; 32];
    out.copy_from_slice(ctx.finish().as_ref());
    out
}

struct PendingSession<A> {
    session_id: u32,
    sealer: Sealer,
    opener: Opener,
    msg1_key: Digest,
    msg2: Vec<u8>,
    last_msg2_sent: Duration,
    deadline: Duration,
    timestamp_us: u64,
    hello: HelloPayload,
    peer: A,
    /// The counter of the authenticated record that last set `peer`.
    peer_counter: Option<u64>,
    /// A `SessionConfirm` arrived and the caller hasn't answered yet.
    confirming: bool,
}

struct LiveSession<A> {
    session_id: u32,
    sealer: Sealer,
    opener: Opener,
    msg1_key: Digest,
    established: Duration,
    last_rx: Duration,
    peer: A,
    peer_counter: Option<u64>,
    /// The caller called `ack`: everything the commit needed is durable.
    acked: bool,
    /// A `SessionConfirm` retry arrived before that.
    ack_owed: bool,
    last_ack: Option<Duration>,
}

impl<A: Clone> LiveSession<A> {
    /// Follows the peer to a new address, but only for a record newer than
    /// the one that last moved it, so a delayed copy can't move it back.
    fn follow(&mut self, from: &A, counter: u64) {
        if self.peer_counter.is_none_or(|c| counter > c) {
            self.peer = from.clone();
            self.peer_counter = Some(counter);
        }
    }
}

struct Device<A> {
    live: Option<LiveSession<A>>,
    pending: Option<PendingSession<A>>,
    /// The newest timestamp committed in memory, in case the caller hasn't
    /// persisted it yet.
    committed_timestamp: u64,
}

impl<A> Default for Device<A> {
    fn default() -> Self {
        Self {
            live: None,
            pending: None,
            committed_timestamp: 0,
        }
    }
}

/// A refused pending session between [`HostSessions::prepare_refusal`] and
/// [`HostSessions::refuse`]: its send key and where to send, nothing else.
struct RefusalContext<A> {
    dev_id: Id16,
    session_id: u32,
    peer: A,
    sealer: Sealer,
    deadline: Duration,
}

/// A sealed `SessionRefused` kept for resending after its keys are gone.
struct Refusal<A> {
    session_id: u32,
    peer: A,
    sealed: Vec<u8>,
    until: Duration,
    last_sent: Duration,
}

#[derive(Clone, Copy)]
struct RateWindow {
    start: Duration,
    count: u32,
}

impl RateWindow {
    fn allow(&mut self, now: Duration, limit: u32) -> bool {
        if now.saturating_sub(self.start) >= Duration::from_secs(1) {
            *self = RateWindow {
                start: now,
                count: 0,
            };
        }
        if self.count >= limit {
            return false;
        }
        self.count += 1;
        true
    }
}

/// Per-`dev_id` budget for `Stale` answers, with the last one cached so a
/// replayed message 1 is answered without another Diffie-Hellman.
struct StaleBudget {
    window: RateWindow,
    last: Option<(Digest, Vec<u8>)>,
}

/// Sources tracked by the message 1 rate limit. Beyond this, new sources
/// share one bucket.
const MAX_RATE_ENTRIES: usize = 1024;
/// Remembered timed-out message 1s.
const MAX_EXPIRED_MSG1: usize = 4096;

pub struct HostSessions<A: PeerAddr> {
    config: HostConfig,
    rng: Box<dyn Random + Send>,
    devices: HashMap<Id16, Device<A>>,
    sessions: HashMap<u32, Id16>,
    expired_msg1: HashMap<Digest, Duration>,
    refusal_contexts: Vec<RefusalContext<A>>,
    refusals: Vec<Refusal<A>>,
    rate: HashMap<A::Bucket, RateWindow>,
    overflow_rate: RateWindow,
    stale: HashMap<Id16, StaleBudget>,
}

impl<A: PeerAddr> HostSessions<A> {
    pub fn new(config: HostConfig, rng: Box<dyn Random + Send>) -> Self {
        Self {
            config,
            rng,
            devices: HashMap::new(),
            sessions: HashMap::new(),
            expired_msg1: HashMap::new(),
            refusal_contexts: Vec::new(),
            refusals: Vec::new(),
            rate: HashMap::new(),
            overflow_rate: RateWindow {
                start: Duration::ZERO,
                count: 0,
            },
            stale: HashMap::new(),
        }
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// The live session for `dev_id`, if any.
    pub fn live_session(&self, dev_id: &Id16) -> Option<u32> {
        self.devices
            .get(dev_id)?
            .live
            .as_ref()
            .map(|l| l.session_id)
    }

    /// The pending session for `dev_id`, if any.
    pub fn pending_session(&self, dev_id: &Id16) -> Option<u32> {
        self.devices
            .get(dev_id)?
            .pending
            .as_ref()
            .map(|p| p.session_id)
    }

    pub fn pending_count(&self) -> usize {
        self.devices
            .values()
            .filter(|d| d.pending.is_some())
            .count()
    }

    /// Handles one inbound datagram. Anything that isn't v3 is ignored, so
    /// the caller can offer every datagram here first.
    pub fn handle_datagram(
        &mut self,
        now: Duration,
        from: &A,
        datagram: &[u8],
        creds: &dyn HostCredentials,
    ) -> Vec<HostOutput<A>> {
        let mut out = Vec::new();
        match wire::classify(datagram) {
            V3Kind::Handshake1 => self.handle_msg1(now, from, datagram, creds, &mut out),
            V3Kind::Sealed => self.handle_record(now, from, datagram, creds, &mut out),
            V3Kind::Handshake2 | V3Kind::Other => {}
        }
        out
    }

    fn allow_msg1(&mut self, now: Duration, from: &A) -> bool {
        let limit = self.config.msg1_per_second;
        let bucket = from.rate_bucket();
        let window = if let Some(w) = self.rate.get_mut(&bucket) {
            w
        } else if self.rate.len() < MAX_RATE_ENTRIES {
            self.rate.entry(bucket).or_insert(RateWindow {
                start: now,
                count: 0,
            })
        } else {
            &mut self.overflow_rate
        };
        window.allow(now, limit)
    }

    fn stale_budget(&mut self, dev_id: &Id16, now: Duration) -> &mut StaleBudget {
        self.stale.entry(*dev_id).or_insert(StaleBudget {
            window: RateWindow {
                start: now,
                count: 0,
            },
            last: None,
        })
    }

    fn handle_msg1(
        &mut self,
        now: Duration,
        from: &A,
        datagram: &[u8],
        creds: &dyn HostCredentials,
        out: &mut Vec<HostOutput<A>>,
    ) {
        let Ok(msg1) = wire::parse_msg1(datagram) else {
            return;
        };
        if !self.allow_msg1(now, from) {
            return;
        }
        let key = msg1_key(&msg1);
        if self
            .expired_msg1
            .get(&key)
            .is_some_and(|&until| now < until)
        {
            return;
        }
        let dev_id = msg1.dev_id;
        let resend_interval = self.config.msg2_resend_interval;
        if let Some(device) = self.devices.get_mut(&dev_id) {
            if let Some(p) = device.pending.as_mut().filter(|p| p.msg1_key == key) {
                if now.saturating_sub(p.last_msg2_sent) >= resend_interval {
                    p.last_msg2_sent = now;
                    out.push(HostOutput::Send {
                        to: from.clone(),
                        datagram: p.msg2.clone(),
                    });
                }
                return;
            }
            if device.live.as_ref().is_some_and(|l| l.msg1_key == key) {
                return;
            }
        }
        let stale_limit = self.config.stale_per_second;
        if let Some(budget) = self.stale.get_mut(&dev_id) {
            if let Some((_, msg2)) = budget.last.as_ref().filter(|(k, _)| *k == key) {
                if budget.window.allow(now, stale_limit) {
                    out.push(HostOutput::Send {
                        to: from.clone(),
                        datagram: msg2.clone(),
                    });
                }
                return;
            }
        }
        let Some(k_dev) = creds.key(&dev_id) else {
            return;
        };
        let has_pending = self
            .devices
            .get(&dev_id)
            .is_some_and(|d| d.pending.is_some());
        if !has_pending && self.pending_count() >= self.config.max_pending {
            return;
        }
        let Ok(responder) = Responder::read_msg1(&msg1, &k_dev, &self.config.host_id) else {
            return;
        };
        drop(k_dev);
        let timestamp_us = responder.payload().timestamp_us;
        if timestamp_us > MAX_TIMESTAMP_US {
            return;
        }
        let committed = creds.last_committed_timestamp(&dev_id).max(
            self.devices
                .get(&dev_id)
                .map_or(0, |d| d.committed_timestamp),
        );
        if timestamp_us <= committed {
            let budget = self.stale_budget(&dev_id, now);
            if !budget.window.allow(now, stale_limit) {
                return;
            }
            let reply = Msg2Payload::Stale {
                stored_timestamp_us: committed,
            };
            if let Ok((msg2, _)) = responder.write_msg2(&reply) {
                self.stale_budget(&dev_id, now).last = Some((key, msg2.clone()));
                out.push(HostOutput::Send {
                    to: from.clone(),
                    datagram: msg2,
                });
            }
            return;
        }
        if let Some(p) = self.devices.get(&dev_id).and_then(|d| d.pending.as_ref()) {
            if timestamp_us <= p.timestamp_us {
                // A late copy from an older attempt.
                return;
            }
        }
        let hello = responder.payload().clone();
        let session_id = self.allocate_session_id();
        let mut accept = creds.accept_template(&dev_id);
        accept.session_id = session_id;
        let Ok((msg2, Some(keys))) = responder.write_msg2(&Msg2Payload::Accept(accept)) else {
            return;
        };
        let (sealer, opener) = keys.responder(session_id);
        // A newer attempt replaces the pending one, even while its
        // confirmation is being decided.
        self.destroy_pending(&dev_id, now, out);
        self.sessions.insert(session_id, dev_id);
        self.devices.entry(dev_id).or_default().pending = Some(PendingSession {
            session_id,
            sealer,
            opener,
            msg1_key: key,
            msg2: msg2.clone(),
            last_msg2_sent: now,
            deadline: now + self.config.pending_lifetime,
            timestamp_us,
            hello,
            peer: from.clone(),
            peer_counter: None,
            confirming: false,
        });
        out.push(HostOutput::Send {
            to: from.clone(),
            datagram: msg2,
        });
    }

    fn handle_record(
        &mut self,
        now: Duration,
        from: &A,
        datagram: &[u8],
        creds: &dyn HostCredentials,
        out: &mut Vec<HostOutput<A>>,
    ) {
        let Ok(header) = RecordHeader::parse(datagram) else {
            return;
        };
        if let Some(refusal) = self
            .refusals
            .iter_mut()
            .find(|r| r.session_id == header.session_id)
        {
            if refusal.peer == *from
                && now < refusal.until
                && now.saturating_sub(refusal.last_sent) >= self.config.refusal_resend_interval
            {
                refusal.last_sent = now;
                out.push(HostOutput::Send {
                    to: from.clone(),
                    datagram: refusal.sealed.clone(),
                });
            }
            return;
        }
        let Some(&dev_id) = self.sessions.get(&header.session_id) else {
            return;
        };
        let device = self
            .devices
            .get_mut(&dev_id)
            .expect("session maps to a device");

        if let Some(p) = device
            .pending
            .as_mut()
            .filter(|p| p.session_id == header.session_id)
        {
            let Ok(opened) = p.opener.open(datagram) else {
                return;
            };
            if p.peer_counter.is_none_or(|c| opened.counter > c) {
                p.peer = from.clone();
                p.peer_counter = Some(opened.counter);
            }
            // Only SessionConfirm commits. Anything else under a pending
            // session is dropped.
            if let Ok(Some(SessionMessage::Confirm)) =
                SessionMessage::parse(opened.inner_type, &opened.body)
            {
                if !p.confirming {
                    p.confirming = true;
                    out.push(HostOutput::ConfirmRequested {
                        dev_id,
                        session_id: p.session_id,
                    });
                }
            }
            return;
        }

        let Some(live) = device
            .live
            .as_mut()
            .filter(|l| l.session_id == header.session_id)
        else {
            return;
        };
        if now.saturating_sub(live.established) >= self.config.max_age {
            self.close(now, &dev_id, CloseReason::Expired, out);
            return;
        }
        let Ok(opened) = live.opener.open(datagram) else {
            return;
        };
        live.last_rx = now;
        live.follow(from, opened.counter);
        match SessionMessage::parse(opened.inner_type, &opened.body) {
            Ok(None) => out.push(HostOutput::Deliver {
                dev_id,
                session_id: live.session_id,
                counter: opened.counter,
                inner_type: opened.inner_type,
                body: opened.body,
            }),
            Ok(Some(SessionMessage::Confirm)) => {
                if !live.acked {
                    live.ack_owed = true;
                } else if live
                    .last_ack
                    .is_none_or(|t| now.saturating_sub(t) >= self.config.confirm_ack_interval)
                {
                    Self::send_ack(now, live, &dev_id, creds, out);
                }
            }
            Ok(Some(SessionMessage::Bye(_))) => {
                let session_id = live.session_id;
                self.sessions.remove(&session_id);
                device.live = None;
                out.push(HostOutput::Closed {
                    dev_id,
                    session_id,
                    reason: CloseReason::PeerBye,
                });
            }
            // Host-to-iPad messages, or a malformed body: drop.
            Ok(Some(_)) | Err(_) => {}
        }
    }

    fn send_ack(
        now: Duration,
        live: &mut LiveSession<A>,
        dev_id: &Id16,
        creds: &dyn HostCredentials,
        out: &mut Vec<HostOutput<A>>,
    ) {
        let message = SessionMessage::ConfirmAck(creds.slot_status(dev_id));
        if let Ok(record) = live
            .sealer
            .seal(message.inner_type(), &message.encode_body())
        {
            live.last_ack = Some(now);
            live.ack_owed = false;
            out.push(HostOutput::Send {
                to: live.peer.clone(),
                datagram: record,
            });
        }
    }

    /// Commits the pending session named by a [`HostOutput::ConfirmRequested`].
    /// Zeroes the live session it replaces. Does nothing if that pending
    /// session is gone or past its lifetime.
    pub fn commit(&mut self, now: Duration, dev_id: &Id16, session_id: u32) -> Vec<HostOutput<A>> {
        let mut out = Vec::new();
        let Some(device) = self.devices.get_mut(dev_id) else {
            return out;
        };
        let Some(p) = device
            .pending
            .as_ref()
            .filter(|p| p.session_id == session_id && p.confirming)
        else {
            return out;
        };
        if now >= p.deadline {
            self.destroy_pending(dev_id, now, &mut out);
            return out;
        }
        let p = device.pending.take().expect("checked above");
        let replaced = device.live.take().map(|old| {
            self.sessions.remove(&old.session_id);
            out.push(HostOutput::Closed {
                dev_id: *dev_id,
                session_id: old.session_id,
                reason: CloseReason::Superseded,
            });
            old.session_id
        });
        device.committed_timestamp = device.committed_timestamp.max(p.timestamp_us);
        device.live = Some(LiveSession {
            session_id,
            sealer: p.sealer,
            opener: p.opener,
            msg1_key: p.msg1_key,
            established: now,
            last_rx: now,
            peer: p.peer.clone(),
            peer_counter: p.peer_counter,
            acked: false,
            ack_owed: false,
            last_ack: None,
        });
        out.push(HostOutput::Committed {
            dev_id: *dev_id,
            session_id,
            replaced,
            timestamp_us: p.timestamp_us,
            hello: p.hello,
            peer: p.peer,
        });
        out
    }

    /// Sends the `SessionConfirmAck` once everything the commit needed is
    /// durable. Later `SessionConfirm` retries get a re-sealed ack.
    pub fn ack(
        &mut self,
        now: Duration,
        dev_id: &Id16,
        session_id: u32,
        creds: &dyn HostCredentials,
    ) -> Vec<HostOutput<A>> {
        let mut out = Vec::new();
        if let Some(live) = self
            .devices
            .get_mut(dev_id)
            .and_then(|d| d.live.as_mut())
            .filter(|l| l.session_id == session_id && !l.acked)
        {
            live.acked = true;
            Self::send_ack(now, live, dev_id, creds, &mut out);
        }
        out
    }

    /// Starts refusing a pending session whose record conflicts at Finalize.
    /// Keeps a send-only context (its send key, its `session_id`, the
    /// confirm's address) and destroys everything else, including the
    /// receive key. Nothing that later ends the `dev_id`'s sessions, such as
    /// [`revoke`](Self::revoke) at step 1 of the invalidation procedure,
    /// touches the context. Returns false if the pending session is gone.
    pub fn prepare_refusal(&mut self, now: Duration, dev_id: &Id16, session_id: u32) -> bool {
        let Some(device) = self.devices.get_mut(dev_id) else {
            return false;
        };
        if !device
            .pending
            .as_ref()
            .is_some_and(|p| p.session_id == session_id && p.confirming && now < p.deadline)
        {
            return false;
        }
        let p = device.pending.take().expect("checked above");
        self.sessions.remove(&session_id);
        self.remember_expired(p.msg1_key, now);
        self.refusal_contexts.push(RefusalContext {
            dev_id: *dev_id,
            session_id,
            peer: p.peer,
            sealer: p.sealer,
            deadline: now + self.config.pending_lifetime,
        });
        true
    }

    /// Seals `SessionRefused` for a prepared refusal. Call it once the record
    /// is retired durably, so `creds.slot_status` carries its tombstone. The
    /// send key is destroyed right after this one seal; only the sealed bytes
    /// are kept, for resending to the same address. A confirming pending
    /// session that wasn't prepared is prepared here first. The live session
    /// is untouched.
    pub fn refuse(
        &mut self,
        now: Duration,
        dev_id: &Id16,
        session_id: u32,
        creds: &dyn HostCredentials,
    ) -> Vec<HostOutput<A>> {
        let mut out = Vec::new();
        let find = |contexts: &[RefusalContext<A>]| {
            contexts
                .iter()
                .position(|c| c.session_id == session_id && c.dev_id == *dev_id)
        };
        let index = match find(&self.refusal_contexts) {
            Some(index) => index,
            None => {
                if !self.prepare_refusal(now, dev_id, session_id) {
                    return out;
                }
                find(&self.refusal_contexts).expect("just prepared")
            }
        };
        let mut context = self.refusal_contexts.swap_remove(index);
        if now >= context.deadline {
            out.push(HostOutput::ConfirmCancelled {
                dev_id: *dev_id,
                session_id,
            });
            return out;
        }
        let message = SessionMessage::Refused(creds.slot_status(dev_id));
        if let Ok(sealed) = context
            .sealer
            .seal(message.inner_type(), &message.encode_body())
        {
            out.push(HostOutput::Send {
                to: context.peer.clone(),
                datagram: sealed.clone(),
            });
            self.refusals.push(Refusal {
                session_id,
                peer: context.peer,
                sealed,
                until: now + self.config.refusal_lifetime,
                last_sent: now,
            });
        }
        out
    }

    /// Ends every session for `dev_id` at once, with a sealed BYE to a live
    /// one. For Remove, call this at step 1 of the invalidation procedure.
    pub fn revoke(&mut self, now: Duration, dev_id: &Id16) -> Vec<HostOutput<A>> {
        let mut out = Vec::new();
        self.close(now, dev_id, CloseReason::Revoked, &mut out);
        out
    }

    /// Ends every session, for shutdown.
    pub fn shutdown(&mut self, now: Duration) -> Vec<HostOutput<A>> {
        let mut out = Vec::new();
        let ids: Vec<Id16> = self.devices.keys().copied().collect();
        for dev_id in ids {
            self.close(now, &dev_id, CloseReason::Shutdown, &mut out);
        }
        out
    }

    fn close(
        &mut self,
        now: Duration,
        dev_id: &Id16,
        reason: CloseReason,
        out: &mut Vec<HostOutput<A>>,
    ) {
        self.destroy_pending(dev_id, now, out);
        let Some(device) = self.devices.get_mut(dev_id) else {
            return;
        };
        if let Some(mut live) = device.live.take() {
            self.sessions.remove(&live.session_id);
            let bye = match reason {
                CloseReason::Shutdown => Some(ByeReason::HostShuttingDown),
                CloseReason::Revoked | CloseReason::Liveness | CloseReason::Expired => {
                    Some(ByeReason::Error)
                }
                CloseReason::Superseded | CloseReason::Exhausted | CloseReason::PeerBye => None,
            };
            if let Some(bye) = bye {
                let message = SessionMessage::Bye(bye as u8);
                if let Ok(record) = live
                    .sealer
                    .seal(message.inner_type(), &message.encode_body())
                {
                    out.push(HostOutput::Send {
                        to: live.peer.clone(),
                        datagram: record,
                    });
                }
            }
            out.push(HostOutput::Closed {
                dev_id: *dev_id,
                session_id: live.session_id,
                reason,
            });
        }
    }

    /// Destroys the pending session for `dev_id`, if any, and remembers its
    /// message 1 so late copies are dropped. A decision still owed on it is
    /// cancelled.
    fn destroy_pending(&mut self, dev_id: &Id16, now: Duration, out: &mut Vec<HostOutput<A>>) {
        if let Some(p) = self.devices.get_mut(dev_id).and_then(|d| d.pending.take()) {
            self.sessions.remove(&p.session_id);
            self.remember_expired(p.msg1_key, now);
            if p.confirming {
                out.push(HostOutput::ConfirmCancelled {
                    dev_id: *dev_id,
                    session_id: p.session_id,
                });
            }
        }
    }

    fn remember_expired(&mut self, key: Digest, now: Duration) {
        if self.expired_msg1.len() >= MAX_EXPIRED_MSG1 {
            self.expired_msg1.retain(|_, until| *until > now);
            if self.expired_msg1.len() >= MAX_EXPIRED_MSG1 {
                return;
            }
        }
        self.expired_msg1
            .insert(key, now + self.config.expired_msg1_lifetime);
    }

    /// Seals an application message for the live session of `dev_id`.
    /// Returns the destination and the record, or `None` without a live
    /// session or once it hit a limit ([`poll`](Self::poll) then closes it).
    pub fn seal(
        &mut self,
        now: Duration,
        dev_id: &Id16,
        inner_type: u8,
        body: &[u8],
    ) -> Option<(A, Vec<u8>)> {
        let live = self.devices.get_mut(dev_id)?.live.as_mut()?;
        if now.saturating_sub(live.established) >= self.config.max_age {
            return None;
        }
        let record = live.sealer.seal(inner_type, body).ok()?;
        Some((live.peer.clone(), record))
    }

    /// Runs the timers: pending and decision deadlines, liveness, maximum
    /// age, counter limits in both directions, and the expiry of remembered
    /// message 1s, refusals and rate windows. Call it often; every 10-50 ms is
    /// plenty.
    pub fn poll(&mut self, now: Duration) -> Vec<HostOutput<A>> {
        let mut out = Vec::new();
        let ids: Vec<Id16> = self.devices.keys().copied().collect();
        for dev_id in ids {
            let device = &self.devices[&dev_id];
            if device.pending.as_ref().is_some_and(|p| now >= p.deadline) {
                self.destroy_pending(&dev_id, now, &mut out);
            }
            let device = &self.devices[&dev_id];
            if let Some(live) = &device.live {
                let reason = if now.saturating_sub(live.established) >= self.config.max_age {
                    Some(CloseReason::Expired)
                } else if live.sealer.sealed() >= MAX_RECORDS_PER_DIRECTION
                    || live.opener.highest() == Some(MAX_RECORDS_PER_DIRECTION - 1)
                {
                    Some(CloseReason::Exhausted)
                } else if now.saturating_sub(live.last_rx) >= self.config.liveness_timeout {
                    Some(CloseReason::Liveness)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    self.close(now, &dev_id, reason, &mut out);
                }
            }
            let device = &self.devices[&dev_id];
            if device.live.is_none() && device.pending.is_none() {
                self.devices.remove(&dev_id);
            }
        }
        let mut cancelled = Vec::new();
        self.refusal_contexts.retain(|c| {
            let keep = now < c.deadline;
            if !keep {
                cancelled.push(HostOutput::ConfirmCancelled {
                    dev_id: c.dev_id,
                    session_id: c.session_id,
                });
            }
            keep
        });
        out.extend(cancelled);
        self.expired_msg1.retain(|_, until| *until > now);
        self.refusals.retain(|r| r.until > now);
        self.rate
            .retain(|_, w| now.saturating_sub(w.start) < Duration::from_secs(1));
        self.stale
            .retain(|_, b| now.saturating_sub(b.window.start) < Duration::from_secs(1));
        out
    }

    fn allocate_session_id(&mut self) -> u32 {
        loop {
            let mut bytes = [0u8; 4];
            self.rng.fill(&mut bytes);
            let id = u32::from_le_bytes(bytes);
            if id != 0
                && !self.sessions.contains_key(&id)
                && !self.refusals.iter().any(|r| r.session_id == id)
                && !self.refusal_contexts.iter().any(|c| c.session_id == id)
            {
                return id;
            }
        }
    }

    /// Checks the table's invariants. For tests and fuzzing.
    pub fn check_invariants(&self) {
        let mut mapped = 0;
        for (dev_id, device) in &self.devices {
            for id in device
                .live
                .as_ref()
                .map(|l| l.session_id)
                .into_iter()
                .chain(device.pending.as_ref().map(|p| p.session_id))
            {
                assert_eq!(self.sessions.get(&id), Some(dev_id), "session map");
                mapped += 1;
            }
            if let (Some(l), Some(p)) = (&device.live, &device.pending) {
                assert_ne!(l.session_id, p.session_id);
            }
        }
        assert_eq!(mapped, self.sessions.len(), "no stray session ids");
        assert!(self.pending_count() <= self.config.max_pending);
        for r in &self.refusals {
            assert!(!self.sessions.contains_key(&r.session_id));
        }
        for c in &self.refusal_contexts {
            assert!(!self.sessions.contains_key(&c.session_id));
            assert!(!self.refusals.iter().any(|r| r.session_id == c.session_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v3::handshake::Initiator;
    use crate::v3::record::SessionKeys;
    use crate::v3::SeededRandom;
    use std::cell::RefCell;

    const HOST: Id16 = [0xB0; 16];
    const DEV: Id16 = [0xD0; 16];
    const K: Key32 = [0x42; 32];
    const ADDR: u32 = 1;

    #[derive(Default)]
    struct Creds {
        keys: HashMap<Id16, Key32>,
        timestamp: RefCell<u64>,
    }

    impl HostCredentials for Creds {
        fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
            self.keys.get(dev_id).copied().map(Zeroizing::new)
        }
        fn last_committed_timestamp(&self, _: &Id16) -> u64 {
            *self.timestamp.borrow()
        }
        fn accept_template(&self, _: &Id16) -> Accept {
            Accept {
                host_name: "PC".into(),
                ..Default::default()
            }
        }
        fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
            SlotStatus {
                held: vec![(*dev_id, wire::RecordState::Active)],
                tombstones: vec![],
            }
        }
    }

    fn setup() -> (HostSessions<u32>, Creds) {
        let mut creds = Creds::default();
        creds.keys.insert(DEV, K);
        (
            HostSessions::new(HostConfig::new(HOST), Box::new(SeededRandom::new(1))),
            creds,
        )
    }

    fn hello(ts: u64) -> HelloPayload {
        HelloPayload {
            timestamp_us: ts,
            ..Default::default()
        }
    }

    fn sends<A: Clone>(out: &[HostOutput<A>]) -> Vec<Vec<u8>> {
        out.iter()
            .filter_map(|o| match o {
                HostOutput::Send { datagram, .. } => Some(datagram.clone()),
                _ => None,
            })
            .collect()
    }

    const T0: Duration = Duration::from_secs(100);

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    /// Runs a handshake up to a confirm the host has seen. Returns the iPad's
    /// halves and the session id.
    fn confirming(
        host: &mut HostSessions<u32>,
        creds: &Creds,
        ts: u64,
        at: Duration,
    ) -> (Sealer, Opener, u32) {
        let mut ipad = Initiator::start(&K, &DEV, &HOST, ts as u32, &hello(ts)).unwrap();
        let msg2 = sends(&host.handle_datagram(at, &ADDR, ipad.msg1(), creds)).remove(0);
        let (reply, keys) = ipad.read_msg2(&msg2).unwrap();
        let Msg2Payload::Accept(accept) = reply else {
            panic!()
        };
        let (mut sealer, opener) = keys.unwrap().initiator(accept.session_id);
        let confirm = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        let out = host.handle_datagram(at, &ADDR, &confirm, creds);
        assert!(matches!(out[..], [HostOutput::ConfirmRequested { .. }]));
        (sealer, opener, accept.session_id)
    }

    fn live(
        host: &mut HostSessions<u32>,
        creds: &Creds,
        ts: u64,
        at: Duration,
    ) -> (Sealer, Opener, u32) {
        let (sealer, opener, sid) = confirming(host, creds, ts, at);
        host.commit(at, &DEV, sid);
        host.ack(at, &DEV, sid, creds);
        (sealer, opener, sid)
    }

    #[test]
    fn identical_message_1_gets_the_same_message_2_and_no_second_pending() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        let first = sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds));
        let pending = host.pending_session(&DEV).unwrap();
        let again = sends(&host.handle_datagram(T0 + ms(250), &ADDR, ipad.msg1(), &creds));
        assert_eq!(first, again);
        assert_eq!(host.pending_session(&DEV), Some(pending));
        host.check_invariants();
    }

    #[test]
    fn cached_message_2_is_resent_at_most_every_100_ms() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
        let replies: usize = (1..=10)
            .map(|i| {
                sends(&host.handle_datagram(T0 + ms(10 * i), &ADDR, ipad.msg1(), &creds)).len()
            })
            .sum();
        assert_eq!(replies, 1, "only the copy at 100 ms is answered");
    }

    #[test]
    fn shell_bytes_never_make_a_new_message_1() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        let msg2 = sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds));
        let pending = host.pending_session(&DEV).unwrap();
        let mut renonced = ipad.msg1().to_vec();
        renonced[18] ^= 0xFF; // client_nonce, outside the authenticated part
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(200), &ADDR, &renonced, &creds)),
            msg2,
            "same message 1: the cached message 2"
        );
        assert_eq!(host.pending_session(&DEV), Some(pending));

        // Once timed out, a copy with changed shell bytes is still dropped.
        host.poll(T0 + Duration::from_secs(5));
        assert!(host.pending_session(&DEV).is_none());
        renonced[19] ^= 0x55;
        assert!(host
            .handle_datagram(T0 + Duration::from_secs(6), &ADDR, &renonced, &creds)
            .is_empty());
        assert!(host.pending_session(&DEV).is_none());
    }

    #[test]
    fn newer_message_1_replaces_the_pending_and_older_is_dropped() {
        let (mut host, creds) = setup();
        let a = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        let b = Initiator::start(&K, &DEV, &HOST, 2, &hello(20)).unwrap();
        host.handle_datagram(T0, &ADDR, a.msg1(), &creds);
        let first = host.pending_session(&DEV).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0, &ADDR, b.msg1(), &creds)).len(),
            1
        );
        let second = host.pending_session(&DEV).unwrap();
        assert_ne!(first, second);
        // The first attempt's message 1 is remembered and dropped now.
        assert!(host.handle_datagram(T0, &ADDR, a.msg1(), &creds).is_empty());
        let c = Initiator::start(&K, &DEV, &HOST, 3, &hello(15)).unwrap();
        assert!(host.handle_datagram(T0, &ADDR, c.msg1(), &creds).is_empty());
        assert_eq!(host.pending_session(&DEV), Some(second));
    }

    #[test]
    fn a_newer_message_1_replaces_a_confirming_pending_and_cancels_its_decision() {
        let (mut host, creds) = setup();
        let (mut old_sealer, _, s) = live(&mut host, &creds, 10, T0);
        let (_, _, n) = confirming(&mut host, &creds, 20, T0 + ms(10));
        let newer = Initiator::start(&K, &DEV, &HOST, 3, &hello(30)).unwrap();
        let out = host.handle_datagram(T0 + ms(2100), &ADDR, newer.msg1(), &creds);
        assert!(out.contains(&HostOutput::ConfirmCancelled {
            dev_id: DEV,
            session_id: n
        }));
        assert_ne!(host.pending_session(&DEV), Some(n));
        // The late decision changes nothing: the live session survives.
        assert!(host.commit(T0 + ms(2500), &DEV, n).is_empty());
        assert_eq!(host.live_session(&DEV), Some(s));
        let report = old_sealer.seal(0x15, &[0; 36]).unwrap();
        assert!(matches!(
            host.handle_datagram(T0 + ms(2600), &ADDR, &report, &creds)[..],
            [HostOutput::Deliver { .. }]
        ));
        host.check_invariants();
    }

    #[test]
    fn a_confirming_pending_still_times_out_and_a_late_commit_does_nothing() {
        let (mut host, creds) = setup();
        let (mut old_sealer, _, s) = live(&mut host, &creds, 10, T0);
        let (_, _, n) = confirming(&mut host, &creds, 20, T0 + ms(10));
        let report = old_sealer.seal(0x15, &[0; 36]).unwrap();
        host.handle_datagram(T0 + Duration::from_secs(4), &ADDR, &report, &creds);
        let out = host.poll(T0 + ms(10) + Duration::from_secs(5));
        assert!(out.contains(&HostOutput::ConfirmCancelled {
            dev_id: DEV,
            session_id: n
        }));
        assert!(host.pending_session(&DEV).is_none());
        assert!(host.commit(T0 + Duration::from_secs(6), &DEV, n).is_empty());
        assert_eq!(host.live_session(&DEV), Some(s));
    }

    #[test]
    fn a_commit_after_the_deadline_but_before_poll_does_nothing() {
        let (mut host, creds) = setup();
        let (_, _, s) = live(&mut host, &creds, 10, T0);
        let (_, _, n) = confirming(&mut host, &creds, 20, T0);
        let out = host.commit(T0 + Duration::from_secs(5), &DEV, n);
        assert_eq!(
            out,
            vec![HostOutput::ConfirmCancelled {
                dev_id: DEV,
                session_id: n
            }]
        );
        assert_eq!(host.live_session(&DEV), Some(s));
    }

    #[test]
    fn a_prepared_refusal_survives_revoke_and_seals_once_after_retirement() {
        let (mut host, creds) = setup();
        let (mut sealer, opener, n) = confirming(&mut host, &creds, 20, T0);
        assert!(host.prepare_refusal(T0, &DEV, n));
        assert!(host.pending_session(&DEV).is_none(), "pending destroyed");
        // Step 1 of the invalidation procedure ends the dev_id's sessions.
        host.revoke(T0 + ms(1), &DEV);
        // Confirm retries meanwhile get nothing: no receive key, no reply.
        let retry = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        assert!(host
            .handle_datagram(T0 + ms(50), &ADDR, &retry, &creds)
            .is_empty());
        let out = host.refuse(T0 + ms(300), &DEV, n, &creds);
        let sealed = sends(&out);
        assert_eq!(sealed.len(), 1);
        let opened = opener.open(&sealed[0]).unwrap();
        assert_eq!(opened.inner_type, wire::INNER_SESSION_REFUSED);
        // Sealed once: a second refuse finds no context, and retries get the
        // same bytes.
        assert!(host.refuse(T0 + ms(301), &DEV, n, &creds).is_empty());
        let retry = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(450), &ADDR, &retry, &creds)),
            sealed
        );
        host.check_invariants();
    }

    #[test]
    fn an_unused_refusal_context_expires() {
        let (mut host, creds) = setup();
        let (_, _, n) = confirming(&mut host, &creds, 20, T0);
        assert!(host.prepare_refusal(T0, &DEV, n));
        let out = host.poll(T0 + Duration::from_secs(5));
        assert!(out.contains(&HostOutput::ConfirmCancelled {
            dev_id: DEV,
            session_id: n
        }));
        assert!(host
            .refuse(T0 + Duration::from_secs(6), &DEV, n, &creds)
            .is_empty());
    }

    #[test]
    fn pending_times_out_after_5_s_and_its_message_1_is_dropped_for_30_s() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds);
        host.poll(T0 + ms(4999));
        assert!(host.pending_session(&DEV).is_some());
        host.poll(T0 + Duration::from_secs(5));
        assert!(host.pending_session(&DEV).is_none());
        let late = T0 + Duration::from_secs(5) + ms(29_999);
        assert!(host
            .handle_datagram(late, &ADDR, ipad.msg1(), &creds)
            .is_empty());
        host.poll(T0 + Duration::from_secs(35));
        let later = T0 + Duration::from_secs(35);
        assert_eq!(
            sends(&host.handle_datagram(later, &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
        host.check_invariants();
    }

    #[test]
    fn unknown_dev_id_and_bad_key_create_nothing() {
        let (mut host, creds) = setup();
        let other = Initiator::start(&K, &[0xEE; 16], &HOST, 1, &hello(10)).unwrap();
        assert!(host
            .handle_datagram(T0, &ADDR, other.msg1(), &creds)
            .is_empty());
        let wrong = Initiator::start(&[1; 32], &DEV, &HOST, 1, &hello(10)).unwrap();
        assert!(host
            .handle_datagram(T0, &ADDR, wrong.msg1(), &creds)
            .is_empty());
        assert_eq!(host.pending_count(), 0);
    }

    #[test]
    fn rate_limit_is_20_message_1s_per_second_per_address() {
        let (mut host, creds) = setup();
        let attempts: Vec<_> = (0..25)
            .map(|i| Initiator::start(&K, &DEV, &HOST, i, &hello(10 + u64::from(i))).unwrap())
            .collect();
        let answered = attempts
            .iter()
            .filter(|a| !host.handle_datagram(T0, &ADDR, a.msg1(), &creds).is_empty())
            .count();
        assert_eq!(answered, 20);
        let other_addr = attempts[24].msg1();
        assert!(!host.handle_datagram(T0, &2, other_addr, &creds).is_empty());
        let next_second = T0 + Duration::from_secs(1);
        let fresh = Initiator::start(&K, &DEV, &HOST, 99, &hello(500)).unwrap();
        assert!(!host
            .handle_datagram(next_second, &ADDR, fresh.msg1(), &creds)
            .is_empty());
    }

    #[test]
    fn the_rate_limit_counts_per_ip_not_per_port() {
        let mut creds = Creds::default();
        creds.keys.insert(DEV, K);
        let mut host: HostSessions<SocketAddr> =
            HostSessions::new(HostConfig::new(HOST), Box::new(SeededRandom::new(2)));
        let answered = (0..25u16)
            .filter(|&i| {
                let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10 + u64::from(i))).unwrap();
                let from = SocketAddr::from(([10, 0, 0, 7], 40_000 + i));
                !host
                    .handle_datagram(T0, &from, ipad.msg1(), &creds)
                    .is_empty()
            })
            .count();
        assert_eq!(answered, 20, "25 ports, one IP, one bucket");
        let v6 =
            |tail: u16| SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, tail, 1), 9));
        assert_eq!(v6(1).rate_bucket(), v6(2).rate_bucket(), "one /64");
        let mapped = SocketAddr::from((Ipv6Addr::from([0, 0, 0, 0, 0, 0xffff, 0x0a00, 7]), 9));
        assert_eq!(mapped.rate_bucket(), IpAddr::from([10, 0, 0, 7]));
    }

    #[test]
    fn pending_sessions_are_capped_host_wide() {
        let (mut host, mut creds) = setup();
        for i in 0..20u8 {
            creds.keys.insert([i; 16], K);
        }
        for i in 0..20u8 {
            let ipad = Initiator::start(&K, &[i; 16], &HOST, 1, &hello(10)).unwrap();
            host.handle_datagram(T0, &u32::from(i), ipad.msg1(), &creds);
        }
        assert_eq!(host.pending_count(), 16);
        host.check_invariants();
    }

    #[test]
    fn a_timestamp_not_newer_than_the_committed_one_gets_stale_and_no_pending() {
        let (mut host, creds) = setup();
        *creds.timestamp.borrow_mut() = 1000;
        let mut ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(1000)).unwrap();
        let out = sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds));
        assert_eq!(out.len(), 1);
        assert!(host.pending_session(&DEV).is_none());
        let (reply, keys) = ipad.read_msg2(&out[0]).unwrap();
        assert_eq!(
            reply,
            Msg2Payload::Stale {
                stored_timestamp_us: 1000
            }
        );
        assert!(keys.is_none());
    }

    #[test]
    fn stale_answers_are_capped_at_5_per_second_per_dev_id_and_replays_reuse_the_cache() {
        let (mut host, creds) = setup();
        *creds.timestamp.borrow_mut() = 1000;
        let answered = (0..10u32)
            .filter(|&i| {
                let ipad =
                    Initiator::start(&K, &DEV, &HOST, i, &hello(500 + u64::from(i))).unwrap();
                !host
                    .handle_datagram(T0, &(100 + i), ipad.msg1(), &creds)
                    .is_empty()
            })
            .count();
        assert_eq!(answered, 5);

        let replayed = Initiator::start(&K, &DEV, &HOST, 1, &hello(900)).unwrap();
        let later = T0 + Duration::from_secs(2);
        let first = sends(&host.handle_datagram(later, &7, replayed.msg1(), &creds));
        let again = sends(&host.handle_datagram(later, &8, replayed.msg1(), &creds));
        assert_eq!(first.len(), 1);
        assert_eq!(first, again, "the cached answer, not a new handshake");
    }

    #[test]
    fn a_timestamp_beyond_the_bound_is_dropped() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(MAX_TIMESTAMP_US + 1)).unwrap();
        assert!(host
            .handle_datagram(T0, &ADDR, ipad.msg1(), &creds)
            .is_empty());
        assert!(host.pending_session(&DEV).is_none());
        let ipad = Initiator::start(&K, &DEV, &HOST, 2, &hello(MAX_TIMESTAMP_US)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
    }

    #[test]
    fn records_under_a_pending_session_other_than_confirm_are_dropped() {
        let (mut host, creds) = setup();
        let mut ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        let msg2 = sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds)).remove(0);
        let (reply, keys) = ipad.read_msg2(&msg2).unwrap();
        let Msg2Payload::Accept(accept) = reply else {
            panic!()
        };
        let (mut sealer, _) = keys.unwrap().initiator(accept.session_id);
        let input = sealer.seal(0x20, &[0; 30]).unwrap();
        assert!(host.handle_datagram(T0, &ADDR, &input, &creds).is_empty());
        let confirm = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        let out = host.handle_datagram(T0, &ADDR, &confirm, &creds);
        assert_eq!(
            out,
            vec![HostOutput::ConfirmRequested {
                dev_id: DEV,
                session_id: accept.session_id
            }]
        );
        // A second confirm while the caller decides changes nothing.
        let confirm = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        assert!(host.handle_datagram(T0, &ADDR, &confirm, &creds).is_empty());
        // Nor does an unknown session id.
        let mut unknown = confirm.clone();
        unknown[8] ^= 1;
        assert!(host.handle_datagram(T0, &ADDR, &unknown, &creds).is_empty());
    }

    #[test]
    fn the_receive_direction_at_its_limit_ends_the_session_but_a_forged_counter_does_not() {
        let (mut host, creds) = setup();
        let (mut sealer, _, sid) = live(&mut host, &creds, 10, T0);
        sealer.skip_to(MAX_RECORDS_PER_DIRECTION - 1);
        let last = sealer.seal(0x15, &[0; 36]).unwrap();
        let mut forged = last.clone();
        *forged.last_mut().unwrap() ^= 1;
        host.handle_datagram(T0 + ms(10), &ADDR, &forged, &creds);
        assert!(
            host.poll(T0 + ms(20)).is_empty(),
            "an unauthenticated counter changes nothing"
        );
        assert_eq!(host.live_session(&DEV), Some(sid));
        host.handle_datagram(T0 + ms(30), &ADDR, &last, &creds);
        let out = host.poll(T0 + ms(40));
        assert!(out.contains(&HostOutput::Closed {
            dev_id: DEV,
            session_id: sid,
            reason: CloseReason::Exhausted
        }));
    }

    #[test]
    fn a_delayed_older_record_never_moves_the_peer_back() {
        let (mut host, creds) = setup();
        let (mut sealer, _, _) = live(&mut host, &creds, 10, T0);
        let older = sealer.seal(0x15, &[0; 36]).unwrap();
        let newer = sealer.seal(0x15, &[0; 36]).unwrap();
        host.handle_datagram(T0 + ms(10), &2, &newer, &creds);
        assert_eq!(host.seal(T0 + ms(11), &DEV, 1, &[]).unwrap().0, 2, "roamed");
        host.handle_datagram(T0 + ms(20), &1, &older, &creds);
        assert_eq!(
            host.seal(T0 + ms(21), &DEV, 1, &[]).unwrap().0,
            2,
            "not moved back"
        );
    }

    #[test]
    fn debug_never_prints_a_delivered_body() {
        let output: HostOutput<u32> = HostOutput::Deliver {
            dev_id: DEV,
            session_id: 1,
            counter: 2,
            inner_type: 0x20,
            body: b"typed secret".to_vec(),
        };
        let text = format!("{output:?}");
        assert!(text.contains("body_len: 12"), "{text}");
        assert!(!text.contains("116, 121, 112"), "{text}");
        let _ = SessionKeys::from_raw([0; 32], [1; 32]);
    }
}
