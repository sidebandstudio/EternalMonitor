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
//! Message 1 work is budgeted before any Diffie-Hellman: unknown `dev_id`s
//! are dropped uncharged, known ones are limited per source IP, and cached
//! answers (message 2, `Stale`) have their own per-source budgets, so
//! replays from other sources never use up the iPad's.
//!
//! Per `dev_id` there is at most one live and one pending session. Every
//! packet for a `session_id` whose keys are gone is dropped without a reply.
//! Application records only flow under a committed session.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
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
    /// Shortest gap between two resends of a cached message 2 to one
    /// source.
    pub msg2_resend_interval: Duration,
    /// New `Stale` answers per `dev_id` per second.
    pub stale_per_second: u32,
    /// Resends of a cached `Stale` answer per source per second.
    pub stale_replays_per_second: u32,
    /// Shortest gap between two re-sealed `SessionConfirmAck`s.
    pub confirm_ack_interval: Duration,
    /// How long a sealed `SessionRefused` is kept: resent on the
    /// [`REFUSAL_RESENDS`] schedule, and to confirm retries from its address.
    pub refusal_lifetime: Duration,
    /// Shortest gap between two resends of it to confirm retries.
    pub refusal_resend_interval: Duration,
    /// Message 1s for a known `dev_id` per source per second, before any
    /// Diffie-Hellman. The source is [`PeerAddr::rate_bucket`]. Sources that
    /// don't fit the table share one budget of the same size.
    pub msg1_per_second: u32,
    /// Authenticated message 1s answered (each costs a Diffie-Hellman for
    /// message 2) per second, host-wide. Charged only after message 1
    /// authenticates, so forged message 1s can't use it up. A message 1 from
    /// its `dev_id`'s live session address doesn't count against it.
    pub dh_per_second: u32,
    /// Message 1s per second read from a `dev_id`'s live session address,
    /// before authentication. Its reserved budget of `msg1_per_second` is
    /// charged only after authentication.
    pub reserved_precrypto_per_second: u32,
    /// Cached `Stale`, `TooNew` and `PairAgain` replays per second,
    /// host-wide, to sources other than the one whose message 1 got them.
    pub unverified_replies_per_second: u32,
    /// How far ahead of the later of the host's wall clock and the
    /// committed timestamp a message 1 timestamp may be.
    pub max_timestamp_ahead: Duration,
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
            stale_replays_per_second: 5,
            confirm_ack_interval: Duration::from_millis(50),
            refusal_lifetime: Duration::from_secs(5),
            refusal_resend_interval: Duration::from_millis(100),
            msg1_per_second: 20,
            dh_per_second: 200,
            reserved_precrypto_per_second: 2000,
            unverified_replies_per_second: 50,
            max_timestamp_ahead: Duration::from_secs(24 * 60 * 60),
            max_pending: 16,
            liveness_timeout: Duration::from_secs(3),
            max_age: SESSION_MAX_AGE,
        }
    }
}

/// A peer address. The message 1 rate limits count per [`rate_bucket`]: the
/// source IP, never the port, so one machine can't spread over many
/// buckets, and two machines never share one.
///
/// [`rate_bucket`]: PeerAddr::rate_bucket
pub trait PeerAddr: Clone + Eq + Hash {
    type Bucket: Clone + Eq + Hash;
    fn rate_bucket(&self) -> Self::Bucket;
}

impl PeerAddr for SocketAddr {
    type Bucket = IpAddr;

    /// The full IP address; a v4-mapped IPv6 address counts as its IPv4
    /// address. Prefixes aren't grouped: Tailscale nodes, link-local
    /// addresses and SLAAC hosts on one LAN all share a /64.
    fn rate_bucket(&self) -> IpAddr {
        match self.ip() {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(v6),
            },
            v4 => v4,
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
/// record database; tests back it with a map. Every method is called on the
/// receive path, before any rate limit for unknown `dev_id`s, so each must
/// be an in-memory lookup: no disk or network I/O.
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

struct PendingSession<A: PeerAddr> {
    session_id: u32,
    sealer: Sealer,
    opener: Opener,
    msg1_key: Digest,
    msg2: Vec<u8>,
    /// When the cached message 2 last went to each source. Each source has
    /// its own allowance; sources beyond the capacity get nothing.
    msg2_sent: BoundedTimes<<A as PeerAddr>::Bucket>,
    deadline: Duration,
    timestamp_us: u64,
    hello: HelloPayload,
    /// The source of the newest authenticated `SessionConfirm`: where a
    /// refusal goes, and the live session's first destination. Other
    /// records under a pending session never move it.
    peer: A,
    /// The counter of the confirm that last set `peer`.
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

struct Device<A: PeerAddr> {
    live: Option<LiveSession<A>>,
    pending: Option<PendingSession<A>>,
    /// The message 1 budget for the live session's own address, apart from
    /// every shared budget: a high cap before authentication, and the
    /// per-source limit charged only after it.
    reserved_precrypto: RateWindow,
    reserved: RateWindow,
    /// The newest timestamp committed in memory, in case the caller hasn't
    /// persisted it yet.
    committed_timestamp: u64,
}

impl<A: PeerAddr> Default for Device<A> {
    fn default() -> Self {
        Self {
            live: None,
            pending: None,
            reserved_precrypto: RateWindow {
                start: Duration::ZERO,
                count: 0,
            },
            reserved: RateWindow {
                start: Duration::ZERO,
                count: 0,
            },
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

/// When a sealed `SessionRefused` is sent again, after the first send, to
/// the address that confirmed. An abandoned attempt sends nothing, so these
/// don't wait for a retry.
pub const REFUSAL_RESENDS: [Duration; 5] = [
    Duration::from_millis(100),
    Duration::from_millis(300),
    Duration::from_millis(700),
    Duration::from_millis(1500),
    Duration::from_millis(3100),
];

/// A sealed `SessionRefused` kept for resending after its keys are gone.
struct Refusal<A> {
    session_id: u32,
    peer: A,
    sealed: Vec<u8>,
    first_sent: Duration,
    /// Next entry of [`REFUSAL_RESENDS`].
    next_resend: usize,
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

/// Per-`dev_id` budget for answers that make no pending session (`Stale`,
/// `TooNew`, `PairAgain`), with recent answers cached so a replayed message 1
/// is answered without another Diffie-Hellman.
struct StaleBudget<A: PeerAddr> {
    window: RateWindow,
    /// Newest last.
    cached: Vec<CachedAnswer<<A as PeerAddr>::Bucket>>,
}

struct CachedAnswer<B> {
    key: Digest,
    msg2: Vec<u8>,
    /// The source whose message 1 got this answer.
    origin: B,
    until: Duration,
}

/// Cached `Stale` answers per `dev_id`.
const MAX_CACHED_STALE: usize = 8;

/// Per-source rate windows, expired in insertion order (O(1) per source).
/// An unexpired window is never evicted: when the table is full, sources
/// that don't fit share one overflow budget, so evicting can never reset a
/// source's count.
struct SourceLimiter<B> {
    windows: HashMap<B, RateWindow>,
    order: VecDeque<(B, Duration)>,
    overflow: RateWindow,
    limit: u32,
}

impl<B: Clone + Eq + Hash> SourceLimiter<B> {
    fn new(limit: u32) -> Self {
        Self {
            windows: HashMap::new(),
            order: VecDeque::new(),
            overflow: RateWindow {
                start: Duration::ZERO,
                count: 0,
            },
            limit,
        }
    }

    fn expire(&mut self, now: Duration) {
        while let Some((bucket, start)) = self.order.front() {
            if now.saturating_sub(*start) < Duration::from_secs(1) {
                break;
            }
            if self.windows.get(bucket).is_some_and(|w| w.start == *start) {
                self.windows.remove(bucket);
            }
            self.order.pop_front();
        }
    }

    fn allow(&mut self, now: Duration, bucket: &B) -> bool {
        self.expire(now);
        if let Some(window) = self.windows.get_mut(bucket) {
            return window.allow(now, self.limit);
        }
        if self.windows.len() >= MAX_RATE_ENTRIES {
            return self.overflow.allow(now, self.limit);
        }
        let mut window = RateWindow {
            start: now,
            count: 0,
        };
        let allowed = window.allow(now, self.limit);
        self.windows.insert(bucket.clone(), window);
        self.order.push_back((bucket.clone(), now));
        allowed
    }

    fn prune(&mut self, now: Duration) {
        self.expire(now);
    }
}

/// Times per key with a fixed capacity. A full table takes no new keys, so
/// no key's allowance is ever reset by eviction.
struct BoundedTimes<B> {
    times: HashMap<B, Duration>,
    capacity: usize,
}

impl<B: Clone + Eq + Hash> BoundedTimes<B> {
    fn new(capacity: usize) -> Self {
        Self {
            times: HashMap::new(),
            capacity,
        }
    }

    fn get(&self, key: &B) -> Option<Duration> {
        self.times.get(key).copied()
    }

    fn is_full(&self) -> bool {
        self.times.len() >= self.capacity
    }

    /// Records `at` for `key`. A new key is refused when full.
    fn insert(&mut self, key: B, at: Duration) -> bool {
        if !self.times.contains_key(&key) && self.is_full() {
            return false;
        }
        self.times.insert(key, at);
        true
    }
}

/// Sources tracked by each per-source limit.
const MAX_RATE_ENTRIES: usize = 1024;
/// Sources a pending session remembers message 2 resends for.
const MAX_MSG2_SOURCES: usize = 16;
/// Remembered timed-out message 1s.
const MAX_EXPIRED_MSG1: usize = 4096;
/// Remembered message 1s that got an answer without a pending session.
/// Sized for every credential the host can hold (64) at the answer rate
/// (5/s each) for the 30 s they're remembered: 64 × 5 × 30 = 9,600. When
/// full, no new answer is given; an unexpired entry is never evicted.
const MAX_ANSWERED_MSG1: usize = 16_384;

pub struct HostSessions<A: PeerAddr> {
    config: HostConfig,
    rng: Box<dyn Random + Send>,
    devices: HashMap<Id16, Device<A>>,
    sessions: HashMap<u32, Id16>,
    expired_msg1: HashMap<Digest, Duration>,
    refusal_contexts: Vec<RefusalContext<A>>,
    refusals: Vec<Refusal<A>>,
    msg1_rate: SourceLimiter<A::Bucket>,
    stale_replay_rate: SourceLimiter<A::Bucket>,
    stale: HashMap<Id16, StaleBudget<A>>,
    /// Message 1s that already got a `Stale`, `TooNew` or `PairAgain`
    /// answer, for 30 s, with expiry order. A recognised replay whose cached
    /// answer is gone is dropped without a Diffie-Hellman.
    answered: HashMap<Digest, Duration>,
    answered_order: VecDeque<(Digest, Duration)>,
    dh_budget: RateWindow,
    unverified_replies: RateWindow,
    /// The host's wall clock: µs since the Unix epoch at a given `now`.
    wall_clock: (Duration, u64),
    msg1_reads: u64,
    dh_performed: u64,
}

impl<A: PeerAddr> HostSessions<A> {
    /// `wall_us` is the host's wall clock, in µs since the Unix epoch, at
    /// `now`. Message 1 timestamps are bounded against it.
    pub fn new(
        config: HostConfig,
        rng: Box<dyn Random + Send>,
        now: Duration,
        wall_us: u64,
    ) -> Self {
        Self {
            msg1_rate: SourceLimiter::new(config.msg1_per_second),
            stale_replay_rate: SourceLimiter::new(config.stale_replays_per_second),
            config,
            rng,
            devices: HashMap::new(),
            sessions: HashMap::new(),
            expired_msg1: HashMap::new(),
            refusal_contexts: Vec::new(),
            refusals: Vec::new(),
            stale: HashMap::new(),
            answered: HashMap::new(),
            answered_order: VecDeque::new(),
            dh_budget: RateWindow {
                start: Duration::ZERO,
                count: 0,
            },
            unverified_replies: RateWindow {
                start: Duration::ZERO,
                count: 0,
            },
            wall_clock: (now, wall_us),
            msg1_reads: 0,
            dh_performed: 0,
        }
    }

    /// Steps the host's wall clock: `wall_us` at `now`.
    pub fn set_wall_clock(&mut self, now: Duration, wall_us: u64) {
        self.wall_clock = (now, wall_us);
    }

    /// The latest message 1 timestamp accepted: the later of the host's wall
    /// clock and the committed timestamp, plus
    /// [`HostConfig::max_timestamp_ahead`]. Measured from the committed
    /// value too, so a host clock behind it never locks the device out.
    fn timestamp_limit(&self, now: Duration, committed: u64) -> u64 {
        let (at, wall) = self.wall_clock;
        let elapsed = u64::try_from(now.saturating_sub(at).as_micros()).unwrap_or(u64::MAX);
        let ahead = u64::try_from(self.config.max_timestamp_ahead.as_micros()).unwrap_or(u64::MAX);
        wall.saturating_add(elapsed)
            .max(committed)
            .saturating_add(ahead)
    }

    /// Message 1s read (authenticated or not) so far.
    pub fn msg1_reads(&self) -> u64 {
        self.msg1_reads
    }

    /// Authenticated message 1s answered so far.
    pub fn handshakes_performed(&self) -> u64 {
        self.dh_performed
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

    fn stale_budget(&mut self, dev_id: &Id16, now: Duration) -> &mut StaleBudget<A> {
        self.stale.entry(*dev_id).or_insert(StaleBudget {
            window: RateWindow {
                start: now,
                count: 0,
            },
            cached: Vec::new(),
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
        let bucket = from.rate_bucket();
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
        let unverified_limit = self.config.unverified_replies_per_second;
        if let Some(device) = self.devices.get_mut(&dev_id) {
            if let Some(p) = device.pending.as_mut().filter(|p| p.msg1_key == key) {
                // A retry of the message 1 that made this pending session:
                // the cached message 2 again. Each source has its own 100 ms
                // allowance, whichever sent first; sources beyond
                // MAX_MSG2_SOURCES get nothing.
                let due = p
                    .msg2_sent
                    .get(&bucket)
                    .map_or(!p.msg2_sent.is_full(), |t| {
                        now.saturating_sub(t) >= resend_interval
                    });
                if due && p.msg2_sent.insert(bucket, now) {
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
        if let Some(answer) = self
            .stale
            .get(&dev_id)
            .and_then(|b| b.cached.iter().find(|c| c.key == key && now < c.until))
        {
            // A replay of a message 1 that got an answer without a pending
            // session: the cached answer, within per-source budgets.
            let msg2 = answer.msg2.clone();
            let from_origin = answer.origin == bucket;
            if self.stale_replay_rate.allow(now, &bucket)
                && (from_origin || self.unverified_replies.allow(now, unverified_limit))
            {
                out.push(HostOutput::Send {
                    to: from.clone(),
                    datagram: msg2,
                });
            }
            return;
        }
        if self.answered.get(&key).is_some_and(|&until| now < until) {
            // Answered before, and its cached answer is gone: drop it, with
            // no Diffie-Hellman.
            return;
        }
        // Unknown dev_ids cost nothing and are never charged to a source.
        let Some(k_dev) = creds.key(&dev_id) else {
            return;
        };
        // The live session's own address has a reserved budget, apart from
        // the shared ones; everyone else is limited per source and host-wide.
        let reserved = self.devices.get(&dev_id).is_some_and(|d| {
            d.live
                .as_ref()
                .is_some_and(|l| l.peer.rate_bucket() == bucket)
        });
        // Before authentication: cheap per-source limits only.
        if reserved {
            let cap = self.config.reserved_precrypto_per_second;
            let device = self.devices.get_mut(&dev_id).expect("has a live session");
            if !device.reserved_precrypto.allow(now, cap) {
                return;
            }
        } else if !self.msg1_rate.allow(now, &bucket) {
            return;
        }
        let has_pending = self
            .devices
            .get(&dev_id)
            .is_some_and(|d| d.pending.is_some());
        if !has_pending && self.pending_count() >= self.config.max_pending {
            return;
        }
        self.msg1_reads += 1;
        let Ok(responder) = Responder::read_msg1(&msg1, &k_dev, &self.config.host_id) else {
            return;
        };
        drop(k_dev);
        // Authenticated: now the budgets that forged message 1s must never
        // reach. Answering costs a Diffie-Hellman.
        if reserved {
            let limit = self.config.msg1_per_second;
            let device = self.devices.get_mut(&dev_id).expect("has a live session");
            if !device.reserved.allow(now, limit) {
                return;
            }
        } else if !self.dh_budget.allow(now, self.config.dh_per_second) {
            return;
        }
        self.dh_performed += 1;
        let timestamp_us = responder.payload().timestamp_us;
        if timestamp_us >= MAX_TIMESTAMP_US {
            return;
        }
        let committed = creds.last_committed_timestamp(&dev_id).max(
            self.devices
                .get(&dev_id)
                .map_or(0, |d| d.committed_timestamp),
        );
        let no_pending = if committed >= MAX_TIMESTAMP_US - 1 {
            // A committed value at the ceiling has no valid successor: this
            // key can never connect again. Pairing again issues a new key.
            Some(Msg2Payload::PairAgain)
        } else if let Some(limit) =
            Some(self.timestamp_limit(now, committed)).filter(|&limit| timestamp_us > limit)
        {
            Some(Msg2Payload::TooNew { limit_us: limit })
        } else if timestamp_us <= committed {
            Some(Msg2Payload::Stale {
                stored_timestamp_us: committed,
            })
        } else {
            None
        };
        if let Some(reply) = no_pending {
            self.answer_without_pending(now, from, &dev_id, key, responder, &reply, out);
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
            msg2_sent: {
                let mut sent = BoundedTimes::new(MAX_MSG2_SOURCES);
                sent.insert(bucket, now);
                sent
            },
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

    /// Answers a message 1 with `Stale`, `TooNew` or `PairAgain`, within the
    /// `dev_id`'s budget, caches the answer, and remembers the message 1.
    #[allow(clippy::too_many_arguments)]
    fn answer_without_pending(
        &mut self,
        now: Duration,
        from: &A,
        dev_id: &Id16,
        key: Digest,
        responder: Responder,
        reply: &Msg2Payload,
        out: &mut Vec<HostOutput<A>>,
    ) {
        self.expire_answered(now);
        if self.answered.len() >= MAX_ANSWERED_MSG1 && !self.answered.contains_key(&key) {
            // Full of unexpired entries: refuse new work rather than forget
            // a message 1 that was already answered.
            return;
        }
        let limit = self.config.stale_per_second;
        if !self.stale_budget(dev_id, now).window.allow(now, limit) {
            return;
        }
        let Ok((msg2, _)) = responder.write_msg2(reply) else {
            return;
        };
        let until = now + self.config.expired_msg1_lifetime;
        let origin = from.rate_bucket();
        let budget = self.stale_budget(dev_id, now);
        if budget.cached.len() >= MAX_CACHED_STALE {
            budget.cached.remove(0);
        }
        budget.cached.push(CachedAnswer {
            key,
            msg2: msg2.clone(),
            origin,
            until,
        });
        // Always queued, also when replacing an expired entry for the same
        // message 1, so every entry expires.
        self.answered.insert(key, until);
        self.answered_order.push_back((key, until));
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
            // Only SessionConfirm commits, and only a confirm moves the
            // address. Anything else under a pending session is dropped.
            if let Ok(Some(SessionMessage::Confirm)) =
                SessionMessage::parse(opened.inner_type, &opened.body)
            {
                if p.peer_counter.is_none_or(|c| opened.counter > c) {
                    p.peer = from.clone();
                    p.peer_counter = Some(opened.counter);
                }
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
                first_sent: now,
                next_resend: 0,
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
        // Each sealed refusal goes out again on its schedule, to the address
        // that confirmed, whether or not anything more arrives from there.
        for refusal in &mut self.refusals {
            let mut due = false;
            while REFUSAL_RESENDS
                .get(refusal.next_resend)
                .is_some_and(|&after| now >= refusal.first_sent + after)
            {
                refusal.next_resend += 1;
                due = true;
            }
            if due && now < refusal.until {
                refusal.last_sent = now;
                out.push(HostOutput::Send {
                    to: refusal.peer.clone(),
                    datagram: refusal.sealed.clone(),
                });
            }
        }
        self.refusals.retain(|r| r.until > now);
        self.msg1_rate.prune(now);
        self.stale_replay_rate.prune(now);
        self.stale.retain(|_, b| {
            b.cached.retain(|c| now < c.until);
            !b.cached.is_empty() || now.saturating_sub(b.window.start) < Duration::from_secs(1)
        });
        self.expire_answered(now);
        out
    }

    fn expire_answered(&mut self, now: Duration) {
        while let Some(&(key, until)) = self.answered_order.front() {
            if now < until {
                break;
            }
            if self.answered.get(&key) == Some(&until) {
                self.answered.remove(&key);
            }
            self.answered_order.pop_front();
        }
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
            HostSessions::new(
                HostConfig::new(HOST),
                Box::new(SeededRandom::new(1)),
                T0,
                WALL,
            ),
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
    /// The host's wall clock at T0, in µs. Test timestamps are far below it.
    const WALL: u64 = 1_762_000_000_000_000;

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
        let mut host: HostSessions<SocketAddr> = HostSessions::new(
            HostConfig::new(HOST),
            Box::new(SeededRandom::new(2)),
            T0,
            WALL,
        );
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
        let mapped = SocketAddr::from((
            std::net::Ipv6Addr::from([0, 0, 0, 0, 0, 0xffff, 0x0a00, 7]),
            9,
        ));
        assert_eq!(mapped.rate_bucket(), IpAddr::from([10, 0, 0, 7]));
    }

    #[test]
    fn ipv6_peers_sharing_a_prefix_never_share_a_bucket() {
        use std::net::Ipv6Addr;
        let at = |segments: [u16; 8]| SocketAddr::from((Ipv6Addr::from(segments), 41_641));
        // Two Tailscale nodes, two link-local peers, two SLAAC hosts on one /64.
        for (a, b) in [
            (
                [0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0x1234, 0x0001],
                [0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0x5678, 0x0002],
            ),
            (
                [0xfe80, 0, 0, 0, 0x1, 0x2, 0x3, 0x4],
                [0xfe80, 0, 0, 0, 0x5, 0x6, 0x7, 0x8],
            ),
            (
                [0x2001, 0xdb8, 1, 2, 0xa, 0xb, 0xc, 0xd],
                [0x2001, 0xdb8, 1, 2, 0xe, 0xf, 0x1, 0x2],
            ),
        ] {
            assert_ne!(at(a).rate_bucket(), at(b).rate_bucket());
        }

        // And the limits act that way: a flood from one Tailscale node never
        // touches another node's budget.
        let mut creds = Creds::default();
        creds.keys.insert(DEV, K);
        let mut host: HostSessions<SocketAddr> = HostSessions::new(
            HostConfig::new(HOST),
            Box::new(SeededRandom::new(4)),
            T0,
            WALL,
        );
        let attacker = at([0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0x6666, 0x0001]);
        let ipad_addr = at([0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0x1234, 0x0001]);
        for i in 0..30u64 {
            let forged = Initiator::start(&[9; 32], &DEV, &HOST, 1, &hello(i)).unwrap();
            host.handle_datagram(T0, &attacker, forged.msg1(), &creds);
        }
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        assert!(!host
            .handle_datagram(T0, &ipad_addr, ipad.msg1(), &creds)
            .is_empty());
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
    fn a_timestamp_at_or_beyond_the_bound_is_dropped() {
        let (mut host, creds) = setup();
        for ts in [MAX_TIMESTAMP_US, MAX_TIMESTAMP_US + 1, u64::MAX] {
            let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(ts)).unwrap();
            assert!(host
                .handle_datagram(T0, &ADDR, ipad.msg1(), &creds)
                .is_empty());
            assert!(host.pending_session(&DEV).is_none());
        }
        let ipad = Initiator::start(&K, &DEV, &HOST, 2, &hello(MAX_TIMESTAMP_US - 1)).unwrap();
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

    /// A handshake up to message 2; returns the iPad's sealer and opener.
    fn accepted(
        host: &mut HostSessions<u32>,
        creds: &Creds,
        ts: u64,
        from: u32,
    ) -> (Sealer, Opener, u32) {
        let mut ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(ts)).unwrap();
        let msg2 = sends(&host.handle_datagram(T0, &from, ipad.msg1(), creds)).remove(0);
        let (reply, keys) = ipad.read_msg2(&msg2).unwrap();
        let Msg2Payload::Accept(accept) = reply else {
            panic!()
        };
        let (sealer, opener) = keys.unwrap().initiator(accept.session_id);
        (sealer, opener, accept.session_id)
    }

    fn sends_to(out: &[HostOutput<u32>]) -> Vec<u32> {
        out.iter()
            .filter_map(|o| match o {
                HostOutput::Send { to, .. } => Some(*to),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_refusal_goes_to_the_address_that_confirmed_not_one_that_sent_other_records() {
        let (mut host, creds) = setup();
        let (mut sealer, _, sid) = accepted(&mut host, &creds, 10, 1);
        let confirm = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        let other = sealer.seal(0x15, &[0; 36]).unwrap(); // higher counter
        host.handle_datagram(T0, &2, &other, &creds);
        host.handle_datagram(T0, &1, &confirm, &creds);
        assert!(host.prepare_refusal(T0, &DEV, sid));
        let out = host.refuse(T0 + ms(10), &DEV, sid, &creds);
        assert_eq!(sends_to(&out), vec![1]);
    }

    #[test]
    fn during_a_delayed_decision_the_refusal_follows_the_newest_confirm() {
        let (mut host, creds) = setup();
        let (mut sealer, _, sid) = accepted(&mut host, &creds, 10, 1);
        let first = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        host.handle_datagram(T0, &1, &first, &creds);
        // The iPad moves while the decision is pending and retries from there.
        let retry = sealer.seal(wire::INNER_SESSION_CONFIRM, &[]).unwrap();
        host.handle_datagram(T0 + ms(100), &3, &retry, &creds);
        // A delayed copy of the first confirm doesn't move it back.
        host.handle_datagram(T0 + ms(150), &1, &first, &creds);
        let out = host.refuse(T0 + ms(800), &DEV, sid, &creds);
        assert_eq!(sends_to(&out), vec![3]);
    }

    #[test]
    fn message_2_resends_are_budgeted_per_source() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0, &1, ipad.msg1(), &creds)).len(),
            1
        );
        // A spoofed replay of that message 1 from elsewhere...
        assert_eq!(
            sends_to(&host.handle_datagram(T0 + ms(120), &9, ipad.msg1(), &creds)),
            vec![9]
        );
        assert!(host
            .handle_datagram(T0 + ms(130), &9, ipad.msg1(), &creds)
            .is_empty());
        // ...never uses up the iPad's own resend.
        assert_eq!(
            sends_to(&host.handle_datagram(T0 + ms(150), &1, ipad.msg1(), &creds)),
            vec![1]
        );
    }

    #[test]
    fn a_committed_timestamp_at_the_ceiling_means_pair_again() {
        let (mut host, creds) = setup();
        *creds.timestamp.borrow_mut() = MAX_TIMESTAMP_US - 1;
        for ts in [1, MAX_TIMESTAMP_US - 1] {
            let mut ipad = Initiator::start(&K, &DEV, &HOST, ts as u32, &hello(ts)).unwrap();
            let out = sends(&host.handle_datagram(T0, &(ts as u32), ipad.msg1(), &creds));
            assert_eq!(out.len(), 1);
            assert_eq!(ipad.read_msg2(&out[0]).unwrap().0, Msg2Payload::PairAgain);
            assert!(host.pending_session(&DEV).is_none());
        }
    }

    #[test]
    fn a_timestamp_too_far_ahead_of_the_wall_clock_gets_too_new() {
        let (mut host, creds) = setup();
        let wall = 1_762_000_000_000_000u64;
        host.set_wall_clock(T0, wall);
        let day = 24 * 60 * 60 * 1_000_000u64;
        let mut ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(wall + 2 * day)).unwrap();
        let out = sends(&host.handle_datagram(T0 + ms(1000), &ADDR, ipad.msg1(), &creds));
        assert_eq!(
            ipad.read_msg2(&out[0]).unwrap().0,
            Msg2Payload::TooNew {
                limit_us: wall + 1_000_000 + day
            }
        );
        assert!(host.pending_session(&DEV).is_none());
        let ipad = Initiator::start(&K, &DEV, &HOST, 2, &hello(wall + day / 2)).unwrap();
        host.handle_datagram(T0 + ms(1000), &ADDR, ipad.msg1(), &creds);
        assert!(host.pending_session(&DEV).is_some(), "within the bound");
    }

    #[test]
    fn cached_answers_to_other_sources_share_a_host_wide_budget() {
        let (mut host, creds) = setup();
        *creds.timestamp.borrow_mut() = 1000;
        let old = Initiator::start(&K, &DEV, &HOST, 1, &hello(500)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0, &1, old.msg1(), &creds)).len(),
            1
        );
        // Replays of it from 200 other sources within a second.
        let replies: usize = (100..300u32)
            .map(|source| {
                sends(&host.handle_datagram(T0 + ms(10), &source, old.msg1(), &creds)).len()
            })
            .sum();
        assert_eq!(replies, 50, "unverified replies are capped host-wide");
        // The source that sent it first keeps its own allowance.
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(20), &1, old.msg1(), &creds)).len(),
            1
        );
        // And the iPad's own new stale attempt is still answered.
        let fresh = Initiator::start(&K, &DEV, &HOST, 2, &hello(600)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(20), &1, fresh.msg1(), &creds)).len(),
            1
        );
    }

    #[test]
    fn cached_message_2_goes_to_a_bounded_number_of_sources() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        host.handle_datagram(T0, &1, ipad.msg1(), &creds);
        let replies: usize = (100..300u32)
            .map(|source| {
                sends(&host.handle_datagram(T0 + ms(10), &source, ipad.msg1(), &creds)).len()
            })
            .sum();
        assert_eq!(
            replies,
            MAX_MSG2_SOURCES - 1,
            "16 sources per pending session"
        );
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(150), &1, ipad.msg1(), &creds)).len(),
            1,
            "a source already seen keeps its allowance"
        );
    }

    #[test]
    fn unknown_dev_ids_are_never_charged() {
        let (mut host, creds) = setup();
        for source in 0..2000u32 {
            let junk = Initiator::start(&K, &[0xEE; 16], &HOST, 1, &hello(1)).unwrap();
            host.handle_datagram(T0, &(10_000 + source), junk.msg1(), &creds);
        }
        assert!(host.msg1_rate.windows.is_empty());
        assert_eq!(host.handshakes_performed(), 0);
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
    }

    #[test]
    fn a_full_rate_table_never_resets_a_sources_count() {
        let (_, creds) = setup();
        // Only the per-source table decides here.
        let mut config = HostConfig::new(HOST);
        config.dh_per_second = u32::MAX;
        let mut host: HostSessions<u32> =
            HostSessions::new(config, Box::new(SeededRandom::new(5)), T0, WALL);
        let forged = |i: u64| Initiator::start(&[7; 32], &DEV, &HOST, 1, &hello(i)).unwrap();
        // Source 1 uses its whole second.
        for i in 0..20 {
            host.handle_datagram(T0, &1, forged(i).msg1(), &creds);
        }
        // 1,100 other sources fill the table past its size.
        for source in 0..1100u32 {
            host.handle_datagram(T0 + ms(1), &(1000 + source), forged(5).msg1(), &creds);
        }
        // Source 1 is still at its limit: nothing evicted its window, so its
        // next message 1 isn't even read.
        let before = host.msg1_reads();
        host.handle_datagram(T0 + ms(2), &1, forged(99).msg1(), &creds);
        assert_eq!(host.msg1_reads(), before);
    }

    #[test]
    fn the_live_sessions_own_address_has_a_reserved_budget() {
        let (mut host, creds) = setup();
        live(&mut host, &creds, 10, T0);
        // Flood: forged message 1s for this dev_id from 2,000 sources use up
        // the shared budgets.
        for source in 0..2000u32 {
            let forged =
                Initiator::start(&[7; 32], &DEV, &HOST, 1, &hello(u64::from(source))).unwrap();
            host.handle_datagram(T0 + ms(5), &(5000 + source), forged.msg1(), &creds);
        }
        // The iPad reconnecting from its live address still gets through.
        let ipad = Initiator::start(&K, &DEV, &HOST, 2, &hello(20)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(6), &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
    }

    #[test]
    fn a_replay_whose_cached_answer_was_evicted_is_dropped_without_crypto() {
        let (mut host, creds) = setup();
        *creds.timestamp.borrow_mut() = 1000;
        let old: Vec<_> = (0..12u64)
            .map(|i| Initiator::start(&K, &DEV, &HOST, i as u32, &hello(100 + i)).unwrap())
            .collect();
        // Twelve stale attempts, answered 5 per second: the first 4 fall out
        // of the 8-entry cache.
        for (i, attempt) in old.iter().enumerate() {
            let at = T0 + Duration::from_secs(i as u64 / 5);
            assert_eq!(
                sends(&host.handle_datagram(at, &1, attempt.msg1(), &creds)).len(),
                1
            );
        }
        let before = host.handshakes_performed();
        let replay = host.handle_datagram(T0 + Duration::from_secs(3), &1, old[0].msg1(), &creds);
        assert!(replay.is_empty());
        assert_eq!(host.handshakes_performed(), before, "no Diffie-Hellman");
    }

    #[test]
    fn a_sealed_refusal_is_resent_on_its_schedule_without_any_retry() {
        let (mut host, creds) = setup();
        let (_, opener, sid) = confirming(&mut host, &creds, 20, T0);
        assert!(host.prepare_refusal(T0, &DEV, sid));
        let first = sends(&host.refuse(T0, &DEV, sid, &creds));
        assert_eq!(first.len(), 1);
        let mut copies = Vec::new();
        for step in 1..=60u64 {
            let out = host.poll(T0 + ms(step * 100));
            for o in out {
                if let HostOutput::Send { to, datagram } = o {
                    assert_eq!(to, ADDR);
                    assert_eq!(datagram, first[0], "the same bytes");
                    copies.push(step * 100);
                }
            }
        }
        assert_eq!(copies, vec![100, 300, 700, 1500, 3100]);
        assert!(opener.open(&first[0]).is_ok());
    }

    #[test]
    fn forged_message_1s_never_use_the_host_wide_handshake_budget() {
        let (mut host, creds) = setup();
        // Forged message 1s for a known dev_id from 100 sources at their
        // full per-source rate, for 10 s.
        let forged: Vec<Vec<u8>> = (0..2000u64)
            .map(|i| {
                Initiator::start(&[7; 32], &DEV, &HOST, 1, &hello(i))
                    .unwrap()
                    .msg1()
                    .to_vec()
            })
            .collect();
        for second in 0..10u64 {
            for source in 0..100u32 {
                for i in 0..20usize {
                    let msg1 = &forged[source as usize * 20 + i];
                    host.handle_datagram(
                        T0 + Duration::from_secs(second),
                        &(5000 + source),
                        msg1,
                        &creds,
                    );
                }
            }
        }
        assert_eq!(
            host.msg1_reads(),
            20_000,
            "all read: the per-source limits allow it"
        );
        assert_eq!(host.handshakes_performed(), 0, "none authenticated");
        // A fresh connect in the middle of it is answered.
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        let at = T0 + Duration::from_secs(9);
        assert_eq!(
            sends(&host.handle_datagram(at, &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
    }

    #[test]
    fn authenticated_handshakes_are_capped_host_wide() {
        let (mut host, mut creds) = setup();
        let ids: Vec<Id16> = (0..300u16)
            .map(|i| {
                let mut id = [0u8; 16];
                id[..2].copy_from_slice(&i.to_le_bytes());
                id
            })
            .collect();
        for id in &ids {
            creds.keys.insert(*id, K);
        }
        let mut config = HostConfig::new(HOST);
        config.max_pending = 1000;
        let mut host2: HostSessions<u32> =
            HostSessions::new(config, Box::new(SeededRandom::new(9)), T0, WALL);
        for (i, id) in ids.iter().enumerate() {
            let ipad = Initiator::start(&K, id, &HOST, 1, &hello(10)).unwrap();
            host2.handle_datagram(T0, &(i as u32), ipad.msg1(), &creds);
        }
        assert_eq!(host2.handshakes_performed(), 200);
        let _ = &mut host;
    }

    #[test]
    fn spoofing_the_live_address_never_drains_its_reserved_budget() {
        let (mut host, creds) = setup();
        live(&mut host, &creds, 10, T0);
        // Forged message 1s for this dev_id from the live address itself.
        for i in 0..1500u64 {
            let forged = Initiator::start(&[7; 32], &DEV, &HOST, 1, &hello(100 + i)).unwrap();
            host.handle_datagram(T0 + ms(5), &ADDR, forged.msg1(), &creds);
        }
        let ipad = Initiator::start(&K, &DEV, &HOST, 2, &hello(20)).unwrap();
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(6), &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
    }

    #[test]
    fn the_too_new_limit_counts_from_the_committed_timestamp_too() {
        let (mut host, creds) = setup();
        let day = 24 * 60 * 60 * 1_000_000u64;
        // Committed 3 days ahead of the host's clock (a clock set back, or a
        // restart with a persisted value).
        *creds.timestamp.borrow_mut() = WALL + 3 * day;
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(WALL + 3 * day + 5)).unwrap();
        host.handle_datagram(T0, &ADDR, ipad.msg1(), &creds);
        assert!(
            host.pending_session(&DEV).is_some(),
            "within committed + 24 h"
        );
    }

    #[test]
    fn no_answered_message_1_is_forgotten_within_30_s() {
        let (mut host, mut creds) = setup();
        *creds.timestamp.borrow_mut() = 1_000_000;
        // 28 credentials, each answered Stale 5 times a second for 30 s.
        let ids: Vec<Id16> = (0..28u8).map(|i| [0x80 | i; 16]).collect();
        for id in &ids {
            creds.keys.insert(*id, K);
        }
        let mut first = None;
        for tick in 0..150u64 {
            let at = T0 + ms(tick * 200);
            for (n, id) in ids.iter().enumerate() {
                let ipad = Initiator::start(&K, id, &HOST, 1, &hello(1 + tick)).unwrap();
                if first.is_none() {
                    first = Some(ipad.msg1().to_vec());
                }
                host.handle_datagram(at, &(100 + n as u32), ipad.msg1(), &creds);
            }
            host.poll(at);
        }
        // The very first one, replayed just before 30 s, is still recognised.
        let reads = host.msg1_reads();
        let out = host.handle_datagram(T0 + ms(29_900), &999, &first.unwrap(), &creds);
        assert!(out.is_empty());
        assert_eq!(host.msg1_reads(), reads);
    }

    #[test]
    fn a_re_answered_message_1_still_expires() {
        let (mut host, creds) = setup();
        *creds.timestamp.borrow_mut() = 1000;
        let old = Initiator::start(&K, &DEV, &HOST, 1, &hello(500)).unwrap();
        host.handle_datagram(T0, &1, old.msg1(), &creds);
        // Just after it expires, before any poll prunes it: answered again.
        let again = T0 + Duration::from_secs(30) + ms(1);
        assert_eq!(
            sends(&host.handle_datagram(again, &1, old.msg1(), &creds)).len(),
            1
        );
        host.poll(T0 + Duration::from_secs(61));
        assert!(host.answered.is_empty(), "the second entry expired too");
        assert!(host.answered_order.is_empty());
    }

    #[test]
    fn every_source_has_its_own_message_2_resend_allowance() {
        let (mut host, creds) = setup();
        let ipad = Initiator::start(&K, &DEV, &HOST, 1, &hello(10)).unwrap();
        // A copy raced in from another source creates the pending session;
        // the iPad's own copy follows.
        host.handle_datagram(T0, &77, ipad.msg1(), &creds);
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(5), &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
        // Replays from 200 more sources...
        for source in 100..300u32 {
            host.handle_datagram(T0 + ms(10), &source, ipad.msg1(), &creds);
        }
        // ...never take the iPad's own allowance: its retry gets the resend.
        assert!(host
            .handle_datagram(T0 + ms(30), &ADDR, ipad.msg1(), &creds)
            .is_empty());
        assert_eq!(
            sends(&host.handle_datagram(T0 + ms(120), &ADDR, ipad.msg1(), &creds)).len(),
            1
        );
    }
}
