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

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_credstore::{CredentialSnapshot, Finalize, StoreError};
use eternal_wire::v2::control::{ByeReason, ControlMessage, CONTROL_HEADER_SIZE};
use eternal_wire::v2::{CommonPrefix, PacketType};
use eternal_wire::v3::cookie::CookieMode;
use eternal_wire::v3::host::{
    CloseReason, HostConfig, HostCredentials, HostOutput, HostSessions, PeerAddr,
};
use eternal_wire::v3::pairing::{
    PairCredentials, PairOutput, PairingConfig, PairingHost, QrPayload, StoreResult,
};
use eternal_wire::v3::wire::{Accept, SlotStatus};
use eternal_wire::v3::{Id16, Key32, Random, Zeroizing, EXT_PAIR_QR};
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

    /// The link, then the source's IP and port if it has one.
    fn cookie_input(&self) -> Vec<u8> {
        let mut out = match self.link {
            LinkId::Udp => vec![0],
            LinkId::Usb { device_id } => [&[1][..], &device_id.to_le_bytes()].concat(),
        };
        if let Some(addr) = self.addr {
            out.extend(addr.cookie_input());
        }
        out
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
        /// The record that Finalize retires, invalid in memory until this
        /// job is done.
        retires: Option<Id16>,
    },
    /// A Finalize that conflicts: it retires the record, if it still
    /// conflicts once the jobs ahead of it are done.
    Retire { dev_id: Id16, session_id: u32 },
    /// Remove, by the host user.
    Remove { dev_ids: Vec<Id16> },
    /// Expire the pending records at least 7 days old at `now_s`; `dev_ids`
    /// are those this host ended at step 1.
    Expire { now_s: u64, dev_ids: Vec<Id16> },
    /// Pairing step 5: add the pending record the iPad confirmed it stored.
    AddPending {
        dev_id: Id16,
        slot_id: Id16,
        key: SecretKey,
        replaces: Option<Id16>,
        device_name: String,
        now_s: u64,
    },
}

/// A record key in a job. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey(pub Box<Zeroizing<Key32>>);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

impl StoreJob {
    /// The records this job holds invalid in memory until it's done.
    fn holds(&self) -> &[Id16] {
        match self {
            Self::Commit { retires, .. } => retires.as_slice(),
            Self::Retire { .. } | Self::AddPending { .. } => &[],
            Self::Remove { dev_ids } | Self::Expire { dev_ids, .. } => dev_ids,
        }
    }
}

/// A job's result. `Debug` leaves the snapshot out.
pub struct JobDone {
    pub job: StoreJob,
    pub result: Result<Option<Finalize>, StoreError>,
    /// The store's snapshot after it (and after any reopen it needed).
    pub snapshot: Arc<CredentialSnapshot>,
    /// The store failed and couldn't be reopened: no pairing can be used
    /// until the host restarts.
    pub closed: bool,
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
        // Only while it still conflicts: a Finalize ahead of it that failed
        // may have left the slot as it was.
        StoreJob::Retire { dev_id, .. } if store.would_conflict(dev_id) => {
            store.finalize(dev_id).map(Some)
        }
        StoreJob::Retire { .. } => Ok(None),
        StoreJob::Remove { dev_ids } => store.remove(dev_ids).map(|()| None),
        StoreJob::Expire { now_s, .. } => store.expire(*now_s).map(|_| None),
        StoreJob::AddPending {
            dev_id,
            slot_id,
            key,
            replaces,
            device_name,
            now_s,
        } => store
            .add_pending(
                eternal_credstore::NewRecord {
                    dev_id: *dev_id,
                    slot_id: *slot_id,
                    key: key.0.clone(),
                    replaces: *replaces,
                    device_name: device_name.clone(),
                },
                *now_s,
            )
            .map(|()| None),
    };
    JobDone {
        job,
        result,
        snapshot: store.snapshot(),
        closed: false,
    }
}

/// After a job that failed in a way only a reopen settles (`NeedsReopen`, an
/// I/O error), reopens the store, as the writer does: the open finishes or
/// undoes whatever the failed operation left. `done` then carries the
/// reopened store's snapshot, or `closed` with an empty one if it can't
/// reopen. `store` is `None` once closed; later jobs fail with
/// `NeedsReopen`.
pub fn settle<D: eternal_credstore::Disk, K: eternal_credstore::Keyring>(
    store: &mut Option<eternal_credstore::CredentialStore<D, K>>,
    done: &mut JobDone,
    rng: Box<dyn Random + Send>,
) {
    if matches!(
        done.result,
        Err(StoreError::NeedsReopen | StoreError::Io(_))
    ) {
        if let Some(failed) = store.take() {
            let (disk, keyring) = failed.into_parts();
            match eternal_credstore::CredentialStore::open(disk, keyring, rng) {
                Ok((reopened, report)) => {
                    info!(recovery = ?report.recovery, "Credential store reopened");
                    *store = Some(reopened);
                }
                Err(error) => {
                    warn!(%error, "Credential store could not reopen; pairings are unavailable");
                }
            }
        }
    }
    match store {
        Some(open) => done.snapshot = open.snapshot(),
        None => {
            done.snapshot = Arc::default();
            done.closed = true;
        }
    }
}

/// Runs `job` on `store`, or fails it with `NeedsReopen` once the store is
/// closed, then [`settle`]s.
pub fn run_and_settle<D: eternal_credstore::Disk, K: eternal_credstore::Keyring>(
    store: &mut Option<eternal_credstore::CredentialStore<D, K>>,
    job: StoreJob,
    rng: impl FnOnce() -> Box<dyn Random + Send>,
) -> JobDone {
    let mut done = match store.as_mut() {
        Some(open) => run_job(open, job),
        None => JobDone {
            job,
            result: Err(StoreError::NeedsReopen),
            snapshot: Arc::default(),
            closed: true,
        },
    };
    if !done.closed {
        settle(store, &mut done, rng());
    }
    done
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
    /// Records invalid in memory since step 1 of their invalidation, with
    /// how many queued jobs hold each: a job releases only its own hold.
    invalid: HashMap<Id16, usize>,
    /// Committed timestamps not yet in the snapshot.
    timestamps: HashMap<Id16, u64>,
}

impl HostCredentials for Credentials {
    fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        if self.invalid.contains_key(dev_id) {
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
        accept.slot_status = HostCredentials::slot_status(self, dev_id);
        accept
    }

    fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
        self.snapshot.slot_status(dev_id)
    }
}

/// A Finalize the writer hasn't finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Queued {
    dev_id: Id16,
    slot: Option<Id16>,
}

impl PairCredentials for Credentials {
    fn active_in_slot(&self, slot_id: &Id16) -> Option<(Id16, Zeroizing<Key32>)> {
        let active = self.snapshot.active_in(slot_id)?;
        // A record on its way out proves nothing.
        Some((active, self.key(&active)?))
    }

    fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
        self.snapshot.slot_status(dev_id)
    }
}

/// Why the pairing window can't open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingClosed {
    /// The store can't be used.
    Unavailable,
    /// The host holds as many records as it may; remove one first.
    Full,
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
    /// QR pairing: the window and the pairing in flight.
    pairing: PairingHost<PeerId>,
    /// A job came back with the store closed: it stays closed until the
    /// host restarts.
    store_closed: bool,
    /// Finalizes handed to the writer and not yet done, oldest first: each
    /// record, its slot, and the record it retires. Conflicts and expiry
    /// are decided against these as well as the snapshot.
    queued: Vec<Queued>,
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
        Self::with_cookies(
            host_id,
            snapshot,
            pairings,
            rng,
            now,
            wall_us,
            CookieMode::UnderLoad,
        )
    }

    /// [`new`](Self::new), with message 1's cookies required as `cookies`
    /// says on both handshakes: tests run the cookie path with `Always`.
    pub(super) fn with_cookies(
        host_id: Id16,
        snapshot: Option<Arc<CredentialSnapshot>>,
        pairings: Pairings,
        rng: Box<dyn Random + Send>,
        now: Instant,
        wall_us: u64,
        cookies: CookieMode,
    ) -> Self {
        let mut config = host_config(host_id);
        config.cookies = cookies;
        Self {
            sessions: HostSessions::new(config, rng, Duration::ZERO, wall_us),
            creds: Credentials {
                snapshot: snapshot.unwrap_or_default(),
                accept: Accept::default(),
                invalid: HashMap::new(),
                timestamps: HashMap::new(),
            },
            pairings,
            streaming: None,
            outbox: VecDeque::new(),
            pairing: PairingHost::new(
                PairingConfig {
                    cookies,
                    ..PairingConfig::default()
                },
                host_id,
                env!("CARGO_PKG_VERSION"),
                "",
                Box::new(eternal_wire::v3::SystemRandom::default()),
            ),
            store_closed: false,
            queued: Vec::new(),
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

    /// Records held invalid in memory, for tests.
    pub fn held_invalid(&self) -> usize {
        self.creds.invalid.len()
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
        let kind = classify(datagram);
        if source.link != LinkId::Udp && kind != V3Kind::Other {
            // v3 over the USB tunnel comes with sealed media (phase 5).
            return actions;
        }
        let at = self.at(now);
        // Pairing only while the store can take its write; otherwise its
        // datagrams fall through and are dropped.
        let pairing = self.pairings == Pairings::Ready
            && match kind {
                V3Kind::Handshake1 => {
                    eternal_wire::v3::wire::extension_tag(datagram) == Some(EXT_PAIR_QR)
                }
                V3Kind::Sealed => eternal_wire::v3::record::RecordHeader::parse(datagram)
                    .is_ok_and(|h| Some(h.session_id) == self.pairing.session_id()),
                _ => false,
            };
        if pairing {
            let out = self
                .pairing
                .handle_datagram(at, &source, datagram, &self.creds);
            self.apply_pairing(out, now, &mut actions);
            return actions;
        }
        match kind {
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
        let out = self
            .sessions
            .handle_datagram(at, &source, datagram, &self.creds);
        self.follow_peer(session, &mut actions);
        self.apply(session, out, config, now, &mut actions);
        actions
    }

    /// Acts on the pairing's outputs: answers go out as they are (sealed, or
    /// message 2), and a confirmed record goes to the writer.
    fn apply_pairing(&mut self, out: Vec<PairOutput<PeerId>>, now: Instant, actions: &mut Actions) {
        for output in out {
            match output {
                PairOutput::Send { to, datagram } => actions.replies.push((to, datagram)),
                PairOutput::Store(pending) => self.outbox.push_back(StoreJob::AddPending {
                    dev_id: pending.dev_id,
                    slot_id: pending.slot_id,
                    key: SecretKey(pending.key),
                    replaces: pending.replaces,
                    device_name: pending.device_name,
                    now_s: self.wall_s(now),
                }),
                PairOutput::Paired { device_name, .. } => {
                    info!(device = %device_name, "Paired an iPad: it can connect now");
                }
            }
        }
    }

    /// The wall clock at `now`, in seconds since the Unix epoch.
    fn wall_s(&self, now: Instant) -> u64 {
        (self.wall_origin_us + u64::try_from(self.at(now).as_micros()).unwrap_or(0)) / 1_000_000
    }

    /// Opens the pairing window: a fresh QR for `addr`, where the host
    /// listens. Any earlier QR stops working. Refused while the store can't
    /// be used, or when the host already holds as many records as it may.
    pub fn open_pairing(
        &mut self,
        now: Instant,
        addr: std::net::SocketAddr,
        host_name: &str,
    ) -> Result<QrPayload, PairingClosed> {
        if self.pairings != Pairings::Ready {
            return Err(PairingClosed::Unavailable);
        }
        if self.creds.snapshot.len() >= eternal_wire::v3::host::MAX_CREDENTIALS {
            return Err(PairingClosed::Full);
        }
        self.pairing.set_host_name(host_name);
        Ok(self.pairing.open(self.at(now), addr))
    }

    /// Closes the pairing window: its QR stops working.
    pub fn close_pairing(&mut self) {
        self.pairing.close();
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
                HostOutput::ConfirmCancelled { .. } => {}
                HostOutput::Committed {
                    dev_id,
                    session_id,
                    timestamp_us,
                    hello,
                    peer,
                    ..
                } => {
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
                    let finalize = self.creds.snapshot.is_pending(&dev_id)
                        && !self.queued.iter().any(|q| q.dev_id == dev_id);
                    let retires = finalize
                        .then(|| self.creds.snapshot.replaces(&dev_id))
                        .flatten();
                    if let Some(old) = retires {
                        // Step 1 for the record this one replaces.
                        queue.extend(self.invalidate(session, &[old], now, actions));
                    }
                    if finalize {
                        self.queued.push(Queued {
                            dev_id,
                            slot: self.creds.snapshot.slot_of(&dev_id),
                        });
                    }
                    self.outbox.push_back(StoreJob::Commit {
                        dev_id,
                        session_id,
                        timestamp_us,
                        finalize,
                        retires,
                    });
                    actions.merge(session.begin_v3(peer, session_id, &hello, now));
                    // The session it replaced in this pass is not a lost
                    // client: the stream goes on, to the new session.
                    actions.client_lost = false;
                    actions.end_cutover = false;
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
        if self.pairings != Pairings::Ready {
            return Vec::new();
        }
        if self.would_conflict(&dev_id) {
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
            info!(
                session_id,
                "Second client turned away while a session is active"
            );
            return self.sessions.busy(at, &dev_id, session_id);
        }
        self.sessions.commit(at, &dev_id, session_id)
    }

    /// Whether finalizing `dev_id` would conflict, counting the Finalizes
    /// still being written: one queued for its slot makes that record the
    /// slot's active one, so only a record replacing it may follow (the
    /// first Finalize wins).
    fn would_conflict(&self, dev_id: &Id16) -> bool {
        let snapshot = &self.creds.snapshot;
        if self.queued.iter().any(|q| q.dev_id == *dev_id) {
            return false;
        }
        let slot = snapshot.slot_of(dev_id);
        match self
            .queued
            .iter()
            .rev()
            .find(|q| slot.is_some() && q.slot == slot)
        {
            Some(newer) => snapshot.replaces(dev_id) != Some(newer.dev_id),
            // A record it replaces that is being retired (a queued Remove or
            // Expire) won't be the slot's active record by the time this
            // Finalize runs.
            None => {
                snapshot.would_conflict(dev_id)
                    || snapshot
                        .replaces(dev_id)
                        .is_some_and(|old| self.creds.invalid.contains_key(&old))
            }
        }
    }

    /// Takes one hold on each record: invalid in memory until released.
    fn hold(&mut self, dev_ids: &[Id16]) {
        for dev_id in dev_ids {
            *self.creds.invalid.entry(*dev_id).or_insert(0) += 1;
        }
    }

    /// Releases one hold on each record.
    fn release(&mut self, dev_ids: &[Id16]) {
        for dev_id in dev_ids {
            if let Some(count) = self.creds.invalid.get_mut(dev_id) {
                *count -= 1;
                if *count == 0 {
                    self.creds.invalid.remove(dev_id);
                }
            }
        }
    }

    /// Ends every session for `dev_id`, with a sealed BYE to a live one,
    /// without changing what the store holds or what is invalid.
    fn end_sessions(
        &mut self,
        session: &mut Session,
        dev_id: &Id16,
        now: Instant,
        actions: &mut Actions,
    ) -> Vec<HostOutput<PeerId>> {
        if let Some(streaming) = self.streaming.filter(|s| s.dev_id == *dev_id) {
            self.streaming = None;
            actions.merge(session.end_v3(streaming.session_id, EndReason::Revoked, now));
        }
        self.sessions.revoke(self.at(now), dev_id)
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
    /// are invalid in memory, held by the job the caller queues with them,
    /// and their sessions end.
    fn invalidate(
        &mut self,
        session: &mut Session,
        dev_ids: &[Id16],
        now: Instant,
        actions: &mut Actions,
    ) -> Vec<HostOutput<PeerId>> {
        self.hold(dev_ids);
        let mut out = Vec::new();
        for dev_id in dev_ids {
            out.extend(self.end_sessions(session, dev_id, now, actions));
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
        // A closed store stays closed: an older answer read after the close
        // brings back no key, and acks or refuses nothing.
        self.store_closed |= done.closed;
        self.creds.snapshot = if self.store_closed {
            Arc::default()
        } else {
            done.snapshot
        };
        // What the store now holds supersedes what this job held in memory.
        let snapshot = &self.creds.snapshot;
        self.creds
            .timestamps
            .retain(|dev_id, ts| snapshot.last_committed_timestamp(dev_id) < *ts);
        self.release(done.job.holds());
        if let StoreJob::Commit {
            dev_id,
            finalize: true,
            ..
        } = &done.job
        {
            if let Some(i) = self.queued.iter().position(|q| q.dev_id == *dev_id) {
                self.queued.remove(i);
            }
        }
        let mut out = Vec::new();
        if self.store_closed {
            // The store is gone: no pairing can be used, and nothing more
            // commits until the host restarts.
            if self.pairings == Pairings::Ready {
                warn!("The credential store closed: v3 pairings are unavailable");
                self.pairings =
                    Pairings::Unavailable("the credential store could not reopen".into());
            }
            self.queued.clear();
            // No QR stays open, and a pairing in flight ends unanswered: it's
            // unknown whether its write landed.
            self.pairing.close();
            self.pairing.abandon();
            if let Some(streaming) = self.streaming {
                out.extend(self.end_sessions(session, &streaming.dev_id, now, &mut actions));
            }
            self.apply(session, out, config, now, &mut actions);
            return actions;
        }
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
                out.extend(self.end_sessions(session, &dev_id, now, &mut actions));
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
                // The reopen settled it: the snapshot says whether it's gone.
                warn!(%error, "A pairing's removal failed; the store reopened to settle it");
            }
            (StoreJob::Expire { .. }, result) => {
                self.expiring = false;
                if let Err(error) = result {
                    warn!(%error, "Could not expire old pending pairings");
                }
            }
            (StoreJob::Remove { .. }, Ok(_)) => {}
            (StoreJob::AddPending { dev_id, .. }, result) => {
                // The store's state after the job (and any reopen) decides:
                // a write that failed after it reached the disk is stored,
                // and a retry of it would only find a duplicate.
                let landed = self.creds.snapshot.key(&dev_id).is_some();
                let result = match result {
                    Ok(_) => StoreResult::Stored,
                    _ if landed => StoreResult::Stored,
                    Err(StoreError::TooMany) => StoreResult::TooMany,
                    Err(error) => {
                        warn!(%error, "Could not store a new pairing; the iPad's next confirm tries again");
                        StoreResult::Failed
                    }
                };
                let out = self.pairing.stored(at, &dev_id, result, &self.creds);
                self.apply_pairing(out, now, &mut actions);
            }
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
        self.pairing.poll(at);
        if now >= self.next_expire_check && !self.expiring {
            self.next_expire_check = now + EXPIRE_CHECK;
            let now_s = (self.wall_origin_us
                + u64::try_from(self.at(now).as_micros()).unwrap_or(0))
                / 1_000_000;
            // A record whose Finalize is being written is active once it
            // lands (the writer runs jobs in order), so it doesn't expire.
            let expiring: Vec<Id16> = self
                .creds
                .snapshot
                .expiring(now_s)
                .into_iter()
                .filter(|dev_id| !self.queued.iter().any(|q| q.dev_id == *dev_id))
                .collect();
            if !expiring.is_empty() && self.pairings == Pairings::Ready {
                self.expiring = true;
                out.extend(self.invalidate(session, &expiring, now, &mut actions));
                self.outbox.push_back(StoreJob::Expire {
                    now_s,
                    dev_ids: expiring,
                });
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
                for job in job_rx {
                    let done = run_and_settle(&mut store, job, &mut rng);
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
    /// The jobs the writer has, oldest first: their results come back in
    /// this order.
    sent: VecDeque<StoreJob>,
    /// Results for jobs no writer will run (it's gone, or there never was
    /// one): each comes back closed, as from a store that can't reopen.
    dead: Vec<JobDone>,
}

/// The result of a job no store ran: the store is closed.
fn closed(job: StoreJob) -> JobDone {
    JobDone {
        job,
        result: Err(StoreError::NeedsReopen),
        snapshot: Arc::default(),
        closed: true,
    }
}

impl V3Runtime {
    pub fn new(host: V3Host, writer: Option<Writer>) -> Self {
        Self {
            host,
            writer,
            removes: Vec::new(),
            sent: VecDeque::new(),
            dead: Vec::new(),
        }
    }

    /// Jobs the writer has, or finished, that the transport hasn't taken
    /// back yet. Simulations wait for 0 before moving their clock.
    pub fn jobs_in_flight(&self) -> usize {
        self.sent.len() + self.dead.len()
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
        for job in jobs {
            match &self.writer {
                Some(writer) if writer.jobs.send(job.clone()).is_ok() => self.sent.push_back(job),
                writer => {
                    warn!("The credential store's writer is gone: this job can't run");
                    self.dead.push(closed(job));
                    if let Some(writer) = writer {
                        writer.notify.notify_one();
                    }
                }
            }
        }
    }

    /// Every result that's in: the writer's, in order, and a closed one
    /// for each job a writer that's gone never answered.
    fn take_done(&mut self) -> Vec<JobDone> {
        let mut done = Vec::new();
        if let Some(writer) = &self.writer {
            loop {
                match writer.done.try_recv() {
                    Ok(result) => {
                        self.sent.pop_front();
                        done.push(result);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done.extend(self.sent.drain(..).map(closed));
                        break;
                    }
                }
            }
        }
        done.append(&mut self.dead);
        done
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
    runtime.flush();
    // Every result that's in, in order: the writer's answers, then its
    // unanswered jobs as closed if it's gone (a panic wakes nobody), then
    // jobs no writer took. The store-done wake may not have run yet.
    for done in runtime.take_done() {
        actions.merge(runtime.host.job_done(&mut session, done, config, now));
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
    let done = runtime.take_done();
    let mut actions = Actions::default();
    let mut session = shared.session.lock();
    for done in done {
        actions.merge(runtime.host.job_done(&mut session, done, config, now));
    }
    drop(session);
    runtime.flush();
    Some(actions)
}

/// Opens the pairing window for a QR that points at `addr`, in v3 mode.
pub fn open_pairing(
    shared: &crate::control::SharedControl,
    addr: std::net::SocketAddr,
    host_name: &str,
) -> Option<Result<QrPayload, PairingClosed>> {
    let mut v3 = shared.v3.lock();
    let runtime = v3.as_mut()?;
    Some(runtime.host.open_pairing(Instant::now(), addr, host_name))
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
/// `HKCU\<KEYRING_SUBKEY>`. `Local Settings` never roams with a roaming
/// profile, so an older copy of the blob can't come back from another
/// machine (a keyring rollback), and it stays with the local database.
pub const KEYRING_SUBKEY: &str =
    r"Software\Classes\Local Settings\Software\EternalMonitor\credstore";
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
