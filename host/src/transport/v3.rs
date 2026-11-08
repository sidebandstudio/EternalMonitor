//! The host's v3 sessions: the session table of `eternal_wire::v3::host`,
//! the credential store behind it, and the v2 [`Session`] that runs each
//! committed session's stream.
//!
//! Sans-IO, like [`Session`]: the transport feeds it datagrams and time,
//! sends what it returns, hands its store jobs to the writer and feeds the
//! results back. Tests run the same jobs on a real store.
//!
//! - **Message 1, `SessionConfirm` and every sealed record** go to the
//!   session table. Cleartext v2 control never reaches [`Session`] in v3
//!   mode, except HELLO2, which is turned away (`VersionUnsupported`,
//!   version 3): a record's `session_id` travels in the clear, so a
//!   cleartext message naming it proves nothing.
//! - **The commit rule** (design §4.4). A `SessionConfirm` for a pending
//!   session is decided from the store's snapshot, in memory: a Finalize
//!   that would conflict is refused (the record is retired durably, then
//!   `SessionRefused` goes out with its tombstone); otherwise the session
//!   commits at once, [`Session`] starts streaming to it with an IDR, and the
//!   input relay cuts over. The timestamp and any Finalize are then written,
//!   and only after that does the `SessionConfirmAck` go out. The timestamp
//!   is used in memory at once, so a replayed message 1 is stale before the
//!   write lands (§4.4, "persist after cutover").
//! - **Application records.** A record's inner type below 0x40 is a v2
//!   control message with its v2 body: it's rebuilt and handed to
//!   [`Session`], with the record counter as its `msg_seq`. Everything
//!   [`Session`] sends to a v3 session (heartbeats, pongs, stream configs) is
//!   sealed; its goodbye becomes the session table's sealed BYE.
//! - **Record operations.** Remove and Expire end the records' sessions
//!   and stop accepting their handshakes at once (step 1 of the
//!   invalidation procedure), then the store retires them. A Finalize that
//!   replaces a record does the same for the record it replaces.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_credstore::{CredentialSnapshot, Finalize, StoreError};
use eternal_wire::v2::control::{ByeReason, ControlMessage, CONTROL_HEADER_SIZE};
use eternal_wire::v2::{CommonPrefix, PacketType};
use eternal_wire::v3::host::{
    CloseReason, HostConfig, HostCredentials, HostOutput, HostSessions, PeerAddr,
};
use eternal_wire::v3::wire::{Accept, SlotStatus};
use eternal_wire::v3::{Id16, Key32, Random, Zeroizing};
use tracing::{info, warn};

use super::link::{LinkId, PeerId};
use super::session::{
    Actions, ConfigSource, EndReason, Session, HEARTBEAT_INTERVAL, HOLD_LIMIT, LIVENESS_TIMEOUT,
    REPORT_INTERVAL_MS,
};

impl PeerAddr for PeerId {
    type Bucket = (LinkId, Option<IpAddr>);

    /// The link and the source IP, as [`std::net::SocketAddr`]'s bucket.
    fn rate_bucket(&self) -> Self::Bucket {
        (self.link, self.addr.map(|addr| addr.rate_bucket()))
    }
}

/// How often pending records are checked for expiry.
const EXPIRE_CHECK: Duration = Duration::from_secs(60);

/// The session table's settings on the host. A live session is ended by
/// [`Session`]'s own liveness rule (on hold after 3 s, ended after the hold
/// limit), so the table's timeout is the hold limit: an iPad whose uplink
/// stalls while it still decodes keeps its keys as long as v2 keeps its
/// session.
pub fn host_config(host_id: Id16) -> HostConfig {
    HostConfig {
        liveness_timeout: HOLD_LIMIT,
        ..HostConfig::new(host_id)
    }
}

/// Durable work for the store's one writer, in the order handed out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreJob {
    /// A session committed: persist its timestamp, then Finalize a pending
    /// record.
    Commit {
        dev_id: Id16,
        session_id: u32,
        timestamp_us: u64,
        finalize: bool,
    },
    /// A Finalize that conflicts: it retires the record.
    Retire { dev_id: Id16, session_id: u32 },
    /// Remove, by the host user.
    Remove { dev_ids: Vec<Id16> },
    /// Expire the pending records at least 7 days old at `now_s`.
    Expire { now_s: u64 },
}

/// A job's result. `Debug` leaves the snapshot out.
pub struct JobDone {
    pub job: StoreJob,
    pub result: Result<Option<Finalize>, StoreError>,
    /// The store's snapshot after it.
    pub snapshot: Arc<CredentialSnapshot>,
}

impl std::fmt::Debug for JobDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobDone")
            .field("job", &self.job)
            .field("result", &self.result)
            .finish_non_exhaustive()
    }
}

/// Runs `job` on the store. Used by the writer thread and by tests.
pub fn run_job<D: eternal_credstore::Disk, K: eternal_credstore::Keyring>(
    store: &mut eternal_credstore::CredentialStore<D, K>,
    job: StoreJob,
) -> JobDone {
    let result = match &job {
        StoreJob::Commit {
            dev_id,
            timestamp_us,
            finalize,
            ..
        } => store
            .persist_timestamp(dev_id, *timestamp_us)
            .and_then(|()| finalize.then(|| store.finalize(dev_id)).transpose()),
        StoreJob::Retire { dev_id, .. } => store.finalize(dev_id).map(Some),
        StoreJob::Remove { dev_ids } => store.remove(dev_ids).map(|()| None),
        StoreJob::Expire { now_s } => store.expire(*now_s).map(|_| None),
    };
    JobDone {
        job,
        result,
        snapshot: store.snapshot(),
    }
}

/// Whether the host can use its pairings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pairings {
    Ready,
    /// The store couldn't open (its keyring can't be read, or the disk
    /// failed). Nothing was changed; no iPad can connect until it opens.
    Unavailable(String),
}

/// What the session table reads: the store's snapshot, with what changed in
/// memory since it was taken.
struct Credentials {
    snapshot: Arc<CredentialSnapshot>,
    accept: Accept,
    /// Records invalid in memory since step 1 of their invalidation.
    invalid: HashSet<Id16>,
    /// Committed timestamps not yet in the snapshot.
    timestamps: HashMap<Id16, u64>,
}

impl HostCredentials for Credentials {
    fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        if self.invalid.contains(dev_id) {
            return None;
        }
        self.snapshot.key(dev_id)
    }

    fn last_committed_timestamp(&self, dev_id: &Id16) -> u64 {
        self.snapshot
            .last_committed_timestamp(dev_id)
            .max(self.timestamps.get(dev_id).copied().unwrap_or(0))
    }

    fn accept_template(&self, dev_id: &Id16) -> Accept {
        let mut accept = self.accept.clone();
        accept.slot_status = self.slot_status(dev_id);
        accept
    }

    fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
        self.snapshot.slot_status(dev_id)
    }
}

/// The v3 session [`Session`] streams to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Streaming {
    dev_id: Id16,
    session_id: u32,
}

pub struct V3Host {
    sessions: HostSessions<PeerId>,
    creds: Credentials,
    pairings: Pairings,
    streaming: Option<Streaming>,
    /// Jobs for the writer, oldest first.
    outbox: VecDeque<StoreJob>,
    /// Decisions owed on confirms that wait for nothing: a busy host lets
    /// them time out.
    busy_logged: HashSet<u32>,
    origin: Instant,
    /// The wall clock at `origin`, in µs since the Unix epoch.
    wall_origin_us: u64,
    next_expire_check: Instant,
    expiring: bool,
}

/// Microseconds since the Unix epoch.
pub fn wall_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

impl V3Host {
    /// `snapshot` is the opened store's, or `None` when it couldn't open
    /// (`pairings` says why). `wall_us` is the wall clock at `now` ([`wall_us`]).
    pub fn new(
        host_id: Id16,
        snapshot: Option<Arc<CredentialSnapshot>>,
        pairings: Pairings,
        rng: Box<dyn Random + Send>,
        now: Instant,
        wall_us: u64,
    ) -> Self {
        Self {
            sessions: HostSessions::new(host_config(host_id), rng, Duration::ZERO, wall_us),
            creds: Credentials {
                snapshot: snapshot.unwrap_or_default(),
                accept: Accept::default(),
                invalid: HashSet::new(),
                timestamps: HashMap::new(),
            },
            pairings,
            streaming: None,
            outbox: VecDeque::new(),
            busy_logged: HashSet::new(),
            origin: now,
            wall_origin_us: wall_us,
            next_expire_check: now,
            expiring: false,
        }
    }

    pub fn pairings(&self) -> &Pairings {
        &self.pairings
    }

    /// The store's current snapshot (for the host's list of records).
    pub fn snapshot(&self) -> Arc<CredentialSnapshot> {
        self.creds.snapshot.clone()
    }

    /// The session table, for tests.
    pub fn table(&self) -> &HostSessions<PeerId> {
        &self.sessions
    }

    /// The jobs for the writer, oldest first.
    pub fn take_jobs(&mut self) -> Vec<StoreJob> {
        self.outbox.drain(..).collect()
    }

    fn at(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.origin)
    }

    fn refresh_accept(&mut self, session: &Session, config: &impl ConfigSource) {
        let mut stream_config = config.stream_config();
        if let Some(info) = session.client_info() {
            stream_config.fps = crate::control::merge_fps(
                u32::from(stream_config.fps),
                u32::from(info.preferred_fps),
            ) as u16;
        }
        self.creds.accept = Accept {
            session_id: 0,
            heartbeat_interval_ms: HEARTBEAT_INTERVAL.as_millis() as u16,
            report_interval_ms: REPORT_INTERVAL_MS,
            liveness_timeout_ms: LIVENESS_TIMEOUT.as_millis() as u16,
            stream_config,
            host_caps: eternal_wire::v2::control::HOSTCAP_NACK | config.host_caps(),
            host_name: config.host_name(),
            slot_status: SlotStatus::default(),
        };
    }

    /// Handles one inbound datagram in v3 mode. HELLO2 goes to [`Session`]
    /// to be turned away; any other cleartext control is dropped.
    pub fn handle_datagram(
        &mut self,
        session: &mut Session,
        source: PeerId,
        datagram: &[u8],
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        use eternal_wire::v3::wire::{classify, V3Kind};
        let mut actions = Actions::default();
        if source.link != LinkId::Udp {
            // v3 over the USB tunnel comes with sealed media (phase 5).
            return actions;
        }
        match classify(datagram) {
            // Message 2 carries the host's HelloAck fields.
            V3Kind::Handshake1 => self.refresh_accept(session, config),
            V3Kind::Sealed => {}
            V3Kind::Handshake2 => return actions,
            V3Kind::Other => {
                if let Ok((header, message @ ControlMessage::Hello2(_))) =
                    eternal_wire::v2::control::parse_control(datagram)
                {
                    let refused = session.handle_datagram(source, &header, message, config, now);
                    actions.merge(refused);
                }
                return actions;
            }
        }
        let at = self.at(now);
        let out = self
            .sessions
            .handle_datagram(at, &source, datagram, &self.creds);
        self.follow_peer(session, &mut actions);
        self.apply(session, out, config, now, &mut actions);
        actions
    }

    /// The live session moved: [`Session`] and the media follow.
    fn follow_peer(&mut self, session: &mut Session, actions: &mut Actions) {
        let Some(streaming) = self.streaming else {
            return;
        };
        if let Some(peer) = self.sessions.live_peer(&streaming.dev_id).copied() {
            actions.merge(session.move_v3_peer(streaming.session_id, peer));
        }
    }

    /// Acts on the session table's outputs.
    fn apply(
        &mut self,
        session: &mut Session,
        out: Vec<HostOutput<PeerId>>,
        config: &impl ConfigSource,
        now: Instant,
        actions: &mut Actions,
    ) {
        let mut queue: VecDeque<HostOutput<PeerId>> = out.into();
        while let Some(output) = queue.pop_front() {
            match output {
                HostOutput::Send { to, datagram } => actions.replies.push((to, datagram)),
                HostOutput::ConfirmRequested { dev_id, session_id } => {
                    queue.extend(self.decide(session, dev_id, session_id, now));
                }
                HostOutput::ConfirmCancelled { session_id, .. } => {
                    self.busy_logged.remove(&session_id);
                }
                HostOutput::Committed {
                    dev_id,
                    session_id,
                    timestamp_us,
                    hello,
                    peer,
                    ..
                } => {
                    self.busy_logged.remove(&session_id);
                    if let Some(other) = self.streaming.filter(|s| s.dev_id != dev_id) {
                        // The same iPad under another of its records is
                        // superseded; another iPad's session on hold gives
                        // up the stream.
                        let (close, end) = if self.same_slot(&other.dev_id, &dev_id) {
                            (CloseReason::Superseded, EndReason::Superseded)
                        } else {
                            (CloseReason::Liveness, EndReason::Liveness)
                        };
                        self.streaming = None;
                        let at = self.at(now);
                        queue.extend(self.sessions.end(at, &other.dev_id, close));
                        actions.merge(session.end_v3(other.session_id, end, now));
                    }
                    let entry = self.creds.timestamps.entry(dev_id).or_insert(0);
                    *entry = (*entry).max(timestamp_us);
                    let finalize = self.creds.snapshot.is_pending(&dev_id);
                    if let Some(old) = finalize
                        .then(|| self.creds.snapshot.replaces(&dev_id))
                        .flatten()
                    {
                        // Step 1 for the record this one replaces.
                        queue.extend(self.invalidate(session, &[old], now, actions));
                    }
                    self.outbox.push_back(StoreJob::Commit {
                        dev_id,
                        session_id,
                        timestamp_us,
                        finalize,
                    });
                    actions.merge(session.begin_v3(peer, session_id, &hello, now));
                    actions.cutover = Some(session_id);
                    self.streaming = Some(Streaming { dev_id, session_id });
                }
                HostOutput::Deliver {
                    dev_id,
                    session_id,
                    counter,
                    inner_type,
                    body,
                } => {
                    if self.streaming != Some(Streaming { dev_id, session_id }) {
                        continue;
                    }
                    let Some(source) = self.sessions.live_peer(&dev_id).copied() else {
                        continue;
                    };
                    let Some(datagram) = rebuild_control(inner_type, session_id, counter, &body)
                    else {
                        continue;
                    };
                    match eternal_wire::v2::control::parse_control(&datagram) {
                        Ok((_, ControlMessage::Hello2(_))) | Err(_) => {}
                        Ok((header, message)) => {
                            let delivered =
                                session.handle_datagram(source, &header, message, config, now);
                            actions.merge(delivered);
                        }
                    }
                }
                HostOutput::Closed {
                    dev_id,
                    session_id,
                    reason,
                } => {
                    if self.streaming == Some(Streaming { dev_id, session_id }) {
                        self.streaming = None;
                        let reason = match reason {
                            CloseReason::Superseded => EndReason::Superseded,
                            CloseReason::Revoked => EndReason::Revoked,
                            CloseReason::Liveness => EndReason::Liveness,
                            CloseReason::Shutdown => EndReason::Shutdown,
                            CloseReason::PeerBye => EndReason::Goodbye(ByeReason::UserDisconnect),
                            CloseReason::Expired | CloseReason::Exhausted => EndReason::Liveness,
                        };
                        if reason != EndReason::Superseded {
                            actions.merge(session.end_v3(session_id, reason, now));
                        }
                    }
                }
            }
        }
        self.seal(session, now, actions);
    }

    /// The commit rule's decision, from memory alone.
    fn decide(
        &mut self,
        session: &Session,
        dev_id: Id16,
        session_id: u32,
        now: Instant,
    ) -> Vec<HostOutput<PeerId>> {
        let at = self.at(now);
        if self.creds.snapshot.would_conflict(&dev_id) {
            if self.sessions.prepare_refusal(at, &dev_id, session_id) {
                info!(
                    session_id,
                    "A pairing that conflicts at Finalize: retiring it"
                );
                self.outbox
                    .push_back(StoreJob::Retire { dev_id, session_id });
            }
            return Vec::new();
        }
        let busy = session.is_active()
            && !session.on_hold()
            && self
                .streaming
                .is_none_or(|s| !self.same_slot(&s.dev_id, &dev_id));
        if busy {
            if self.busy_logged.insert(session_id) {
                info!(session_id, "Second client waits while a session is active");
            }
            return Vec::new();
        }
        self.sessions.commit(at, &dev_id, session_id)
    }

    /// Whether two records are the same iPad's: they share a slot.
    fn same_slot(&self, a: &Id16, b: &Id16) -> bool {
        a == b
            || self
                .creds
                .snapshot
                .slot_of(a)
                .is_some_and(|slot| self.creds.snapshot.slot_of(b) == Some(slot))
    }

    /// Step 1 of the invalidation procedure for `dev_ids`: from here they
    /// are invalid in memory, and their sessions end.
    fn invalidate(
        &mut self,
        session: &mut Session,
        dev_ids: &[Id16],
        now: Instant,
        actions: &mut Actions,
    ) -> Vec<HostOutput<PeerId>> {
        let at = self.at(now);
        let mut out = Vec::new();
        for dev_id in dev_ids {
            self.creds.invalid.insert(*dev_id);
            out.extend(self.sessions.revoke(at, dev_id));
            if let Some(streaming) = self.streaming.filter(|s| s.dev_id == *dev_id) {
                self.streaming = None;
                actions.merge(session.end_v3(streaming.session_id, EndReason::Revoked, now));
            }
        }
        out
    }

    /// Remove, by the host user: ends the records' sessions now, then the
    /// store retires them.
    pub fn remove(
        &mut self,
        session: &mut Session,
        dev_ids: &[Id16],
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let out = self.invalidate(session, dev_ids, now, &mut actions);
        self.outbox.push_back(StoreJob::Remove {
            dev_ids: dev_ids.to_vec(),
        });
        self.apply(session, out, config, now, &mut actions);
        actions
    }

    /// A store job finished.
    pub fn job_done(
        &mut self,
        session: &mut Session,
        done: JobDone,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let at = self.at(now);
        self.creds.snapshot = done.snapshot;
        // What the store now holds supersedes the overlays.
        let snapshot = &self.creds.snapshot;
        self.creds
            .timestamps
            .retain(|dev_id, ts| snapshot.last_committed_timestamp(dev_id) < *ts);
        self.creds
            .invalid
            .retain(|dev_id| snapshot.key(dev_id).is_some());
        let mut out = Vec::new();
        match (done.job, done.result) {
            (
                StoreJob::Commit {
                    dev_id, session_id, ..
                },
                Ok(None | Some(Finalize::AlreadyActive | Finalize::Promoted { .. })),
            ) => out.extend(self.sessions.ack(at, &dev_id, session_id, &self.creds)),
            (StoreJob::Commit { dev_id, .. }, result) => {
                // Not durable, or it conflicted after all: the session
                // never gets its ack and ends now.
                match result {
                    Ok(_) => {
                        warn!("A committed pairing conflicted at Finalize: ending its session")
                    }
                    Err(error) => warn!(%error, "Could not store a committed session: ending it"),
                }
                out.extend(self.invalidate(session, &[dev_id], now, &mut actions));
                self.creds.invalid.remove(&dev_id);
            }
            (
                StoreJob::Retire {
                    dev_id, session_id, ..
                },
                result,
            ) => {
                if let Err(error) = &result {
                    warn!(%error, "Could not retire a conflicting pairing");
                }
                if matches!(result, Ok(Some(Finalize::Conflict))) {
                    out.extend(self.sessions.refuse(at, &dev_id, session_id, &self.creds));
                }
            }
            (StoreJob::Remove { .. }, Err(error)) => {
                warn!(%error, "Could not remove a pairing; it stays invalid until the store reopens");
            }
            (StoreJob::Expire { .. }, result) => {
                self.expiring = false;
                if let Err(error) = result {
                    warn!(%error, "Could not expire old pending pairings");
                }
            }
            (StoreJob::Remove { .. }, Ok(_)) => {}
        }
        self.apply(session, out, config, now, &mut actions);
        actions
    }

    /// Runs the timers: the session table's, and the expiry of old pending
    /// records. Call it on every transport tick.
    pub fn tick(
        &mut self,
        session: &mut Session,
        config: &impl ConfigSource,
        now: Instant,
    ) -> Actions {
        let mut actions = Actions::default();
        let at = self.at(now);
        let mut out = self.sessions.poll(at);
        if now >= self.next_expire_check && !self.expiring {
            self.next_expire_check = now + EXPIRE_CHECK;
            let now_s = (self.wall_origin_us
                + u64::try_from(self.at(now).as_micros()).unwrap_or(0))
                / 1_000_000;
            let expiring = self.creds.snapshot.expiring(now_s);
            if !expiring.is_empty() {
                self.expiring = true;
                out.extend(self.invalidate(session, &expiring, now, &mut actions));
                self.outbox.push_back(StoreJob::Expire { now_s });
            }
        }
        self.apply(session, out, config, now, &mut actions);
        actions
    }

    /// Seals what [`Session`] decided to send: control replies to the v3
    /// session it streams to go sealed; its goodbye becomes the session
    /// table's sealed BYE. Call it on every [`Actions`] that [`Session`]
    /// produced outside this module (its own ticks, stream config
    /// notifies).
    pub fn seal(&mut self, session: &Session, now: Instant, actions: &mut Actions) {
        let at = self.at(now);
        let mut sealed = Vec::new();
        for (to, datagram) in std::mem::take(&mut actions.replies) {
            match split_control(&datagram) {
                // A refused HELLO2 (session 0) and every record already
                // sealed go out as they are.
                None => sealed.push((to, datagram)),
                Some((_, 0, _)) => sealed.push((to, datagram)),
                Some((packet_type, session_id, body)) => {
                    let Some(streaming) = self.streaming.filter(|s| s.session_id == session_id)
                    else {
                        // A cleartext answer for a session v3 ended: none.
                        continue;
                    };
                    if packet_type == PacketType::Bye as u8 {
                        continue;
                    }
                    if let Some((peer, record)) =
                        self.sessions.seal(at, &streaming.dev_id, packet_type, body)
                    {
                        sealed.push((peer, record));
                    }
                }
            }
        }
        actions.replies = sealed;
        if let Some((_, goodbye)) = actions.goodbye.take() {
            if let (Some(streaming), Some((packet_type, session_id, body))) =
                (self.streaming, split_control(&goodbye))
            {
                if packet_type == PacketType::Bye as u8 && session_id == streaming.session_id {
                    let reason = if body.first() == Some(&(ByeReason::HostShuttingDown as u8)) {
                        CloseReason::Shutdown
                    } else {
                        CloseReason::Liveness
                    };
                    self.streaming = None;
                    for output in self.sessions.end(at, &streaming.dev_id, reason) {
                        if let HostOutput::Send { to, datagram } = output {
                            // Sent like v2's goodbye, a few copies apart.
                            actions.goodbye = Some((to, datagram));
                        }
                    }
                }
            }
        }
        // [`Session`] ended the stream on its own (liveness, an update, a
        // lost link): the v3 session ends with it.
        if let Some(streaming) = self.streaming {
            if session.v3_session_id() != Some(streaming.session_id) {
                self.streaming = None;
                for output in self
                    .sessions
                    .end(at, &streaming.dev_id, CloseReason::Liveness)
                {
                    if let HostOutput::Send { to, datagram } = output {
                        actions.replies.push((to, datagram));
                    }
                }
            }
        }
        if self.streaming.is_none() && actions.client_lost {
            actions.end_cutover = true;
        }
    }

    /// Ends every session, for shutdown.
    pub fn shutdown(&mut self, now: Instant) -> Vec<(PeerId, Vec<u8>)> {
        let at = self.at(now);
        self.streaming = None;
        self.sessions
            .shutdown(at)
            .into_iter()
            .filter_map(|output| match output {
                HostOutput::Send { to, datagram } => Some((to, datagram)),
                _ => None,
            })
            .collect()
    }
}

/// A v2 control datagram's type, `session_id` and body.
fn split_control(datagram: &[u8]) -> Option<(u8, u32, &[u8])> {
    if datagram.len() < CONTROL_HEADER_SIZE {
        return None;
    }
    let prefix = CommonPrefix::decode(datagram).ok()?;
    if prefix.packet_type as u8 >= eternal_wire::v3::SEALED_TYPE {
        return None;
    }
    let session_id = u32::from_le_bytes(datagram[8..12].try_into().ok()?);
    Some((
        prefix.packet_type as u8,
        session_id,
        &datagram[CONTROL_HEADER_SIZE..],
    ))
}

/// The v2 control datagram a record carries: its inner type and body, under
/// the session's id, with the record counter as `msg_seq` (counters stay
/// below 2^32). `None` for a type that isn't a v2 control message.
fn rebuild_control(inner_type: u8, session_id: u32, counter: u64, body: &[u8]) -> Option<Vec<u8>> {
    let packet_type = PacketType::from_u8(inner_type)?;
    if matches!(
        packet_type,
        PacketType::Media | PacketType::MediaFec | PacketType::Audio | PacketType::Error
    ) {
        return None;
    }
    let payload_len = u16::try_from(body.len()).ok()?;
    let msg_seq = u32::try_from(counter).ok()?;
    let mut datagram = vec![0u8; CONTROL_HEADER_SIZE + body.len()];
    CommonPrefix {
        packet_type,
        flags: 0,
        payload_len,
    }
    .encode_into(&mut datagram);
    datagram[8..12].copy_from_slice(&session_id.to_le_bytes());
    datagram[12..16].copy_from_slice(&msg_seq.to_le_bytes());
    datagram[CONTROL_HEADER_SIZE..].copy_from_slice(body);
    Some(datagram)
}

/// The store's one writer: a thread that runs jobs in order and reopens
/// the store after a failure that needs it.
pub struct Writer {
    jobs: std::sync::mpsc::Sender<StoreJob>,
    done: std::sync::mpsc::Receiver<JobDone>,
    notify: Arc<tokio::sync::Notify>,
}

impl Writer {
    pub fn spawn<D, K>(
        store: eternal_credstore::CredentialStore<D, K>,
        rng: impl FnMut() -> Box<dyn Random + Send> + Send + 'static,
    ) -> std::io::Result<Self>
    where
        D: eternal_credstore::Disk + Send + 'static,
        K: eternal_credstore::Keyring + Send + 'static,
    {
        let (jobs, job_rx) = std::sync::mpsc::channel::<StoreJob>();
        let (done_tx, done) = std::sync::mpsc::channel();
        let notify = Arc::new(tokio::sync::Notify::new());
        let woken = Arc::clone(&notify);
        std::thread::Builder::new()
            .name("credential-store".into())
            .spawn(move || {
                let mut rng = rng;
                let mut store = Some(store);
                let mut last = store.as_ref().map(|s| s.snapshot()).unwrap_or_default();
                for job in job_rx {
                    let done = match store.as_mut() {
                        Some(open) => run_job(open, job),
                        None => JobDone {
                            job,
                            result: Err(StoreError::NeedsReopen),
                            snapshot: last.clone(),
                        },
                    };
                    if matches!(done.result, Err(StoreError::NeedsReopen | StoreError::Io(_))) {
                        // Settle whatever the failed operation left.
                        if let Some(failed) = store.take() {
                            let (disk, keyring) = failed.into_parts();
                            match eternal_credstore::CredentialStore::open(disk, keyring, rng()) {
                                Ok((reopened, report)) => {
                                    info!(recovery = ?report.recovery, "Credential store reopened");
                                    store = Some(reopened);
                                }
                                Err(error) => {
                                    warn!(%error, "Credential store could not reopen; pairings are unavailable");
                                }
                            }
                        }
                    }
                    last = store.as_ref().map_or(done.snapshot.clone(), |s| s.snapshot());
                    let done = JobDone {
                        snapshot: last.clone(),
                        ..done
                    };
                    if done_tx.send(done).is_err() {
                        break;
                    }
                    woken.notify_one();
                }
            })?;
        Ok(Self { jobs, done, notify })
    }
}

/// v3 on this host: the session table and its store's writer. Lives in
/// `SharedControl::v3` across pipeline restarts, as [`Session`] does.
pub struct V3Runtime {
    pub host: V3Host,
    writer: Option<Writer>,
    /// Remove requests from outside the transport, taken on its next tick.
    removes: Vec<Vec<Id16>>,
    /// Jobs handed to the writer whose results the transport hasn't taken.
    in_flight: usize,
}

impl V3Runtime {
    pub fn new(host: V3Host, writer: Option<Writer>) -> Self {
        Self {
            host,
            writer,
            removes: Vec::new(),
            in_flight: 0,
        }
    }

    /// Jobs the writer has, or finished, that the transport hasn't taken
    /// back yet. Simulations wait for 0 before moving their clock.
    pub fn jobs_in_flight(&self) -> usize {
        self.in_flight
    }

    /// Wakes the transport when a job finished.
    pub fn notify(&self) -> Option<Arc<tokio::sync::Notify>> {
        self.writer.as_ref().map(|w| Arc::clone(&w.notify))
    }

    /// Asks the transport to remove these records on its next tick.
    pub fn request_remove(&mut self, dev_ids: Vec<Id16>) {
        self.removes.push(dev_ids);
    }

    fn flush(&mut self) {
        let jobs = self.host.take_jobs();
        match &self.writer {
            Some(writer) => {
                for job in jobs {
                    if writer.jobs.send(job).is_ok() {
                        self.in_flight += 1;
                    }
                }
            }
            None if !jobs.is_empty() => {
                warn!(jobs = jobs.len(), "No credential store to run these jobs");
            }
            None => {}
        }
    }
}

/// Feeds one datagram to the v3 host, if v3 is on. `None` with v3 off.
pub fn on_datagram(
    shared: &crate::control::SharedControl,
    source: PeerId,
    datagram: &[u8],
    config: &impl ConfigSource,
    now: Instant,
) -> Option<Actions> {
    let mut v3 = shared.v3.lock();
    let runtime = v3.as_mut()?;
    let actions = {
        let mut session = shared.session.lock();
        runtime
            .host
            .handle_datagram(&mut session, source, datagram, config, now)
    };
    runtime.flush();
    Some(actions)
}

/// The session's tick, in v3 mode: its sends sealed, the session table's
/// timers run, and any requested Remove started.
pub fn on_tick(
    shared: &crate::control::SharedControl,
    config: &impl ConfigSource,
    send_heartbeat: bool,
    now: Instant,
) -> Option<Actions> {
    let mut v3 = shared.v3.lock();
    let runtime = v3.as_mut()?;
    let mut session = shared.session.lock();
    let mut actions = session.tick(config, send_heartbeat, now);
    runtime.host.seal(&session, now, &mut actions);
    actions.merge(runtime.host.tick(&mut session, config, now));
    for dev_ids in std::mem::take(&mut runtime.removes) {
        actions.merge(runtime.host.remove(&mut session, &dev_ids, config, now));
    }
    drop(session);
    runtime.flush();
    Some(actions)
}

/// Feeds every finished store job back, in v3 mode.
pub fn on_jobs_done(
    shared: &crate::control::SharedControl,
    config: &impl ConfigSource,
    now: Instant,
) -> Option<Actions> {
    let mut v3 = shared.v3.lock();
    let runtime = v3.as_mut()?;
    let done: Vec<JobDone> = runtime
        .writer
        .as_ref()
        .map(|w| w.done.try_iter().collect())
        .unwrap_or_default();
    runtime.in_flight -= done.len();
    let mut actions = Actions::default();
    let mut session = shared.session.lock();
    for done in done {
        actions.merge(runtime.host.job_done(&mut session, done, config, now));
    }
    drop(session);
    runtime.flush();
    Some(actions)
}

/// Seals control datagrams [`Session`] made outside a pass of this module
/// (a stream config notify), in v3 mode; unchanged with v3 off.
pub fn seal_replies(
    shared: &crate::control::SharedControl,
    replies: Vec<(PeerId, Vec<u8>)>,
    now: Instant,
) -> Vec<(PeerId, Vec<u8>)> {
    let mut v3 = shared.v3.lock();
    let Some(runtime) = v3.as_mut() else {
        return replies;
    };
    let session = shared.session.lock();
    let mut actions = Actions {
        replies,
        ..Actions::default()
    };
    runtime.host.seal(&session, now, &mut actions);
    actions.replies
}

/// The goodbye an update left for a stopped stream, sealed in v3 mode.
pub fn take_goodbye(
    shared: &crate::control::SharedControl,
    now: Instant,
) -> Option<(PeerId, Vec<u8>)> {
    let goodbye = shared.session.lock().take_goodbye();
    let mut v3 = shared.v3.lock();
    let Some(runtime) = v3.as_mut() else {
        return goodbye;
    };
    let session = shared.session.lock();
    let mut actions = Actions {
        goodbye,
        ..Actions::default()
    };
    runtime.host.seal(&session, now, &mut actions);
    actions.goodbye
}

/// Why this build can't turn v3 on, if it can't.
pub fn unavailable_reason() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("the Mac keychain backend waits for its signed round-trip check")
    } else if cfg!(windows) {
        None
    } else {
        Some("there is no OS keyring for it on this platform")
    }
}

/// Where the store and the host id live: `%LOCALAPPDATA%\EternalMonitor\credentials`
/// (local, never roaming).
pub fn store_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|local| {
        std::path::PathBuf::from(local)
            .join("EternalMonitor")
            .join("credentials")
    })
}

/// This host's `host_id`, made once per install and kept in `dir`.
pub fn host_id(dir: &std::path::Path, rng: &mut dyn Random) -> std::io::Result<Id16> {
    let path = dir.join("host-id");
    match std::fs::read(&path) {
        Ok(bytes) if bytes.len() == 16 => {
            let mut id = [0u8; 16];
            id.copy_from_slice(&bytes);
            return Ok(id);
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "host-id is damaged",
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut id = [0u8; 16];
    rng.fill(&mut id);
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("host-id.tmp");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&id)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(id)
}

/// Turns v3 on with this store: from here every HELLO2 is turned away, and
/// v3 iPads connect with the records it holds. A store that can't open
/// leaves the host in v3 mode with its pairings unavailable; nothing on disk
/// or in the keyring is changed.
pub fn enable_with<D, K>(
    shared: &crate::control::SharedControl,
    host_id: Id16,
    disk: D,
    keyring: K,
    mut rng: impl FnMut() -> Box<dyn Random + Send> + Send + 'static,
) where
    D: eternal_credstore::Disk + Send + 'static,
    K: eternal_credstore::Keyring + Send + 'static,
{
    let now = Instant::now();
    let (snapshot, pairings, writer) =
        match eternal_credstore::CredentialStore::open(disk, keyring, rng()) {
            Ok((store, report)) => {
                if !report.recovery.is_empty() || !report.discarded.is_empty() {
                    info!(recovery = ?report.recovery, discarded = report.discarded.len(),
                        "Credential store recovered at open");
                }
                let snapshot = store.snapshot();
                match Writer::spawn(store, rng) {
                    Ok(writer) => (Some(snapshot), Pairings::Ready, Some(writer)),
                    Err(error) => (None, Pairings::Unavailable(error.to_string()), None),
                }
            }
            Err(error) => (None, Pairings::Unavailable(error.to_string()), None),
        };
    if let Pairings::Unavailable(reason) = &pairings {
        warn!(%reason, "v3 pairings unavailable: nothing was changed; no iPad can connect until the store opens");
    } else {
        info!("v3 transport on: iPads connect with their v3 pairings");
    }
    let host = V3Host::new(
        host_id,
        snapshot,
        pairings,
        Box::new(eternal_wire::v3::SystemRandom::default()),
        now,
        wall_us(),
    );
    shared.session.lock().require_v3();
    *shared.v3.lock() = Some(V3Runtime::new(host, writer));
}

/// The keyring's place on Windows: a DPAPI-protected value under
/// `HKCU\<KEYRING_SUBKEY>`.
pub const KEYRING_SUBKEY: &str = r"Software\EternalMonitor\credstore";
pub const KEYRING_VALUE: &str = "keyring";

/// The `transport_v3` setting: turns v3 on where this build can, or says
/// why not and stays on v2.
pub fn enable(shared: &crate::control::SharedControl) {
    if let Some(reason) = unavailable_reason() {
        warn!(
            reason,
            "The v3 transport setting is on, but v3 can't run here: staying on v2"
        );
        return;
    }
    let Some(dir) = store_dir() else {
        warn!("The v3 transport setting is on, but LOCALAPPDATA isn't set: staying on v2");
        return;
    };
    enable_in(shared, &dir, KEYRING_SUBKEY);
}

/// [`enable`] with the store in `dir` and the keyring under
/// `HKCU\<keyring_subkey>`.
pub fn enable_in(
    shared: &crate::control::SharedControl,
    dir: &std::path::Path,
    keyring_subkey: &str,
) {
    let mut rng = eternal_wire::v3::SystemRandom::default();
    let host_id = match host_id(dir, &mut rng) {
        Ok(id) => id,
        Err(error) => {
            warn!(%error, "The v3 host id can't be read: staying on v2");
            return;
        }
    };
    let disk = match eternal_credstore::StdDisk::new(dir) {
        Ok(disk) => disk,
        Err(error) => {
            warn!(%error, "The v3 credential folder can't be used: staying on v2");
            return;
        }
    };
    #[cfg(windows)]
    {
        let keyring = eternal_credstore::keyring::DpapiRegistry::new(keyring_subkey, KEYRING_VALUE);
        enable_with(shared, host_id, disk, keyring, || {
            Box::new(eternal_wire::v3::SystemRandom::default())
        });
    }
    #[cfg(not(windows))]
    let _ = (shared, host_id, disk, keyring_subkey);
}

#[cfg(test)]
mod tests;
