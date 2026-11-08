//! A deterministic simulation of iPads and one host running the v3
//! reconnect state machines over a lossy network, on a simulated clock.
//!
//! Nothing here sleeps or reads a real clock, so results don't depend on
//! machine load. Shared by `tests/v3_session_sim.rs` and the
//! `v3_session_sim` fuzz target.
//!
//! Device 0 is `sim.client` / `sim.key` / `sim.addr`, and its events go to
//! `sim.events` with the host's events for it. Further devices live in
//! `sim.others`, each with its own events.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use eternal_wire::v2::PacketType;
use eternal_wire::v3::client::{Client, ClientClose, ClientConfig, ClientKey, ClientOutput};
use eternal_wire::v3::host::{
    CloseReason, HostConfig, HostCredentials, HostOutput, HostSessions, PeerAddr,
};
use eternal_wire::v3::record::RecordHeader;
use eternal_wire::v3::wire::{self, Accept, HelloPayload, RecordState, SlotStatus, V3Kind};
use eternal_wire::v3::{Id16, Key32, Random, Zeroizing};

pub const HOST_ID: Id16 = [0xB1; 16];
pub const TICK: Duration = Duration::from_millis(5);
pub const REPORT_BODY: usize = 36;
pub const MEDIA_BODY: usize = 1200;
pub const HEARTBEAT_BODY: usize = 24;
/// The iPads' wall clock is `now` plus this, in µs; the host's starts equal.
pub const CLOCK_OFFSET_US: i64 = 1_762_000_000_000_000;

/// An IPv4-like address: the rate limit counts per `ip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SimAddr {
    pub ip: u16,
    pub port: u16,
}

impl PeerAddr for SimAddr {
    type Bucket = u16;
    fn rate_bucket(&self) -> u16 {
        self.ip
    }
    fn cookie_input(&self) -> Vec<u8> {
        [self.ip.to_le_bytes(), self.port.to_le_bytes()].concat()
    }
}

pub const IPAD: SimAddr = SimAddr {
    ip: 7,
    port: 50_000,
};

pub fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// SplitMix64's output step: turns related seeds into unrelated ones.
pub fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Deterministic xorshift bytes. Session ids and nonces only; the Noise
/// ephemerals come from the OS, which changes bytes but never behaviour.
pub struct XorShift(pub u64);

impl XorShift {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

impl Random for XorShift {
    fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            *byte = self.next() as u8;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    ToHost,
    ToIpad,
}

/// What a packet is, as far as the network can tell from its type and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Msg1,
    Msg2,
    /// An empty sealed message iPad → host: `SessionConfirm`.
    Confirm,
    /// A sealed message carrying a slot status: ack or refusal.
    AckOrRefused,
    Report,
    Media,
    Heartbeat,
    Bye,
    Other,
}

pub fn kind_of(dir: Dir, datagram: &[u8]) -> Kind {
    match wire::classify(datagram) {
        V3Kind::Handshake1 => Kind::Msg1,
        V3Kind::Handshake2 => Kind::Msg2,
        V3Kind::Sealed if datagram.len() >= 37 => match (dir, datagram.len() - 37) {
            (Dir::ToHost, 0) => Kind::Confirm,
            (_, 1) => Kind::Bye,
            (Dir::ToHost, REPORT_BODY) => Kind::Report,
            (Dir::ToIpad, MEDIA_BODY) => Kind::Media,
            (Dir::ToIpad, HEARTBEAT_BODY) => Kind::Heartbeat,
            (Dir::ToIpad, _) => Kind::AckOrRefused,
            _ => Kind::Other,
        },
        _ => Kind::Other,
    }
}

/// The fate of one packet on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    Deliver,
    Drop,
    /// Delivered twice, the copy `extra` later.
    Duplicate {
        extra: Duration,
    },
    /// Delivered `extra` later than the base latency.
    Delay {
        extra: Duration,
    },
}

pub struct Sent {
    pub at: Duration,
    pub dir: Dir,
    pub kind: Kind,
    /// Index among packets of this kind and direction, from 0.
    pub nth: usize,
    pub from: SimAddr,
    pub to: SimAddr,
    pub datagram: Vec<u8>,
}

pub type FaultFn = Box<dyn FnMut(&Sent) -> Fate>;

/// The host's credential store, in memory.
pub struct Store {
    pub keys: HashMap<Id16, Key32>,
    pub timestamps: RefCell<HashMap<Id16, u64>>,
    pub tombstones: RefCell<Vec<Id16>>,
}

impl HostCredentials for Store {
    fn key(&self, dev_id: &Id16) -> Option<Zeroizing<Key32>> {
        if self.tombstones.borrow().contains(dev_id) {
            return None;
        }
        self.keys.get(dev_id).copied().map(Zeroizing::new)
    }
    fn last_committed_timestamp(&self, dev_id: &Id16) -> u64 {
        self.timestamps.borrow().get(dev_id).copied().unwrap_or(0)
    }
    fn accept_template(&self, dev_id: &Id16) -> Accept {
        Accept {
            heartbeat_interval_ms: 1000,
            report_interval_ms: 500,
            liveness_timeout_ms: 3000,
            host_name: "SIM".into(),
            slot_status: self.slot_status(dev_id),
            ..Default::default()
        }
    }
    fn slot_status(&self, dev_id: &Id16) -> SlotStatus {
        let tombstones: Vec<Id16> = self
            .tombstones
            .borrow()
            .iter()
            .filter(|t| *t == dev_id)
            .copied()
            .collect();
        let held = if tombstones.is_empty() {
            vec![(*dev_id, RecordState::Active)]
        } else {
            vec![]
        };
        SlotStatus { held, tombstones }
    }
}

/// How the simulated host application answers the state machine.
#[derive(Clone, Copy)]
pub struct Policy {
    /// Refuse every confirm as a Finalize conflict.
    pub refuse: bool,
    /// Run step 1 of the invalidation procedure (`revoke` of the refused
    /// `dev_id`) between `prepare_refusal` and `refuse`.
    pub invalidate_before_refuse: bool,
    /// Time from `ConfirmRequested` to the caller's decision.
    pub decision_delay: Duration,
    /// Time from `Committed` to `ack`, and from `prepare_refusal` to
    /// `refuse` (the durable writes).
    pub durable_delay: Duration,
    /// Media cadence on a live session. The first packet after a commit is
    /// the IDR.
    pub media_every: Option<Duration>,
    /// Report cadence from each iPad while it has a live session.
    pub report_every: Option<Duration>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            refuse: false,
            invalidate_before_refuse: true,
            decision_delay: Duration::ZERO,
            durable_delay: ms(5),
            media_every: Some(ms(50)),
            report_every: Some(ms(500)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    HostCommitted {
        at: Duration,
        session_id: u32,
        replaced: Option<u32>,
    },
    HostClosed {
        at: Duration,
        session_id: u32,
        reason: CloseReason,
    },
    HostDelivered {
        at: Duration,
        session_id: u32,
        inner_type: u8,
    },
    HostConfirmRequested {
        at: Duration,
        session_id: u32,
    },
    HostConfirmCancelled {
        at: Duration,
        session_id: u32,
    },
    IpadLive {
        at: Duration,
        session_id: u32,
    },
    IpadReleased {
        at: Duration,
    },
    IpadStatus {
        at: Duration,
        status: SlotStatus,
    },
    IpadRefused {
        at: Duration,
        status: SlotStatus,
    },
    IpadAttemptFailed {
        at: Duration,
    },
    IpadPairAgain {
        at: Duration,
    },
    IpadBusy {
        at: Duration,
    },
    IpadDelivered {
        at: Duration,
        session_id: u32,
        draining: bool,
    },
    IpadClosed {
        at: Duration,
        session_id: u32,
        reason: ClientClose,
    },
}

impl Event {
    pub fn at(&self) -> Duration {
        match self {
            Event::HostCommitted { at, .. }
            | Event::HostClosed { at, .. }
            | Event::HostDelivered { at, .. }
            | Event::HostConfirmRequested { at, .. }
            | Event::HostConfirmCancelled { at, .. }
            | Event::IpadLive { at, .. }
            | Event::IpadReleased { at }
            | Event::IpadStatus { at, .. }
            | Event::IpadRefused { at, .. }
            | Event::IpadAttemptFailed { at }
            | Event::IpadPairAgain { at }
            | Event::IpadBusy { at }
            | Event::IpadDelivered { at, .. }
            | Event::IpadClosed { at, .. } => *at,
        }
    }
}

/// A further iPad.
pub struct Ipad {
    pub client: Client,
    pub key: ClientKey,
    pub addr: SimAddr,
    pub events: Vec<Event>,
    next_report: Duration,
}

enum Job {
    Decide { dev_id: Id16, session_id: u32 },
    Refuse { dev_id: Id16, session_id: u32 },
    Ack { dev_id: Id16, session_id: u32 },
}

struct Packet {
    dir: Dir,
    from: SimAddr,
    to: SimAddr,
    datagram: Vec<u8>,
}

pub struct Sim {
    pub now: Duration,
    pub host: HostSessions<SimAddr>,
    pub client: Client,
    pub key: ClientKey,
    pub addr: SimAddr,
    pub others: Vec<Ipad>,
    pub store: Store,
    pub policy: Policy,
    pub latency: Duration,
    pub fault: FaultFn,
    pub events: Vec<Event>,
    /// Host events for devices other than device 0, with their `dev_id`.
    pub other_host_events: Vec<(Id16, Event)>,
    pub sent: Vec<Sent>,
    /// Packets addressed to nobody (after a roam, or to a spoofed source).
    pub undelivered: usize,
    /// The iPad's wall clock in µs, as an offset from `now`.
    pub clock_offset_us: i64,
    in_flight: BTreeMap<(Duration, u64), Packet>,
    seq: u64,
    counts: HashMap<(Dir, Kind), usize>,
    jobs: Vec<(Duration, Job)>,
    host_live: HashMap<Id16, u32>,
    sealed: HashMap<(Dir, u32, u64), Vec<u8>>,
    rng: XorShift,
    next_media: Duration,
    next_heartbeat: Duration,
    next_report: Duration,
}

fn hello() -> HelloPayload {
    HelloPayload {
        listen_port: 50_000,
        screen_px_w: 2732,
        screen_px_h: 2048,
        refresh_hz: 120,
        device_name: "Sim iPad".into(),
        ..Default::default()
    }
}

impl Sim {
    pub fn new(seed: u64) -> Self {
        Self::with_configs(seed, HostConfig::new(HOST_ID), ClientConfig::new(HOST_ID))
    }

    pub fn with_configs(seed: u64, host: HostConfig, client: ClientConfig) -> Self {
        let mut rng = XorShift(seed | 1);
        let key = Self::new_key(&mut rng);
        let mut keys = HashMap::new();
        keys.insert(key.dev_id, key.k_dev);
        Self {
            now: Duration::from_secs(1000),
            host: HostSessions::new(
                host,
                Box::new(XorShift(splitmix(rng.next()) | 1)),
                Duration::from_secs(1000),
                CLOCK_OFFSET_US as u64 + 1_000_000_000,
            ),
            client: Client::new(client, hello(), Box::new(XorShift(rng.next() | 1))),
            key,
            addr: IPAD,
            others: Vec::new(),
            store: Store {
                keys,
                timestamps: RefCell::new(HashMap::new()),
                tombstones: RefCell::new(Vec::new()),
            },
            policy: Policy::default(),
            latency: ms(10),
            fault: Box::new(|_| Fate::Deliver),
            events: Vec::new(),
            other_host_events: Vec::new(),
            sent: Vec::new(),
            undelivered: 0,
            clock_offset_us: CLOCK_OFFSET_US,
            in_flight: BTreeMap::new(),
            seq: 0,
            counts: HashMap::new(),
            jobs: Vec::new(),
            host_live: HashMap::new(),
            sealed: HashMap::new(),
            rng,
            next_media: Duration::ZERO,
            next_heartbeat: Duration::ZERO,
            next_report: Duration::ZERO,
        }
    }

    /// The host process restarts: every session is gone, the credential
    /// store (with its timestamps) stays, and the wall clock is read again.
    pub fn restart_host(&mut self, config: HostConfig, wall_us: u64) {
        // A fresh, unrelated seed: a successor state of the same xorshift
        // sequence would replay the previous instance's session ids.
        let seed = splitmix(self.rng.next());
        self.host = HostSessions::new(config, Box::new(XorShift(seed | 1)), self.now, wall_us);
        self.host_live.clear();
    }

    /// Steps the host's wall clock to the iPads' clock.
    pub fn sync_host_wall_clock(&mut self) {
        let wall = self.clock_us();
        self.host.set_wall_clock(self.now, wall);
    }

    fn new_key(rng: &mut XorShift) -> ClientKey {
        let mut dev_id = [0u8; 16];
        let mut k_dev = [0u8; 32];
        rng.fill(&mut dev_id);
        rng.fill(&mut k_dev);
        ClientKey { dev_id, k_dev }
    }

    /// Adds a paired iPad at `addr` and returns its index in `others`.
    pub fn add_ipad(&mut self, addr: SimAddr) -> usize {
        let key = Self::new_key(&mut self.rng);
        self.store.keys.insert(key.dev_id, key.k_dev);
        let client = Client::new(
            ClientConfig::new(HOST_ID),
            hello(),
            Box::new(XorShift(self.rng.next() | 1)),
        );
        self.others.push(Ipad {
            client,
            key,
            addr,
            events: Vec::new(),
            next_report: Duration::ZERO,
        });
        self.others.len() - 1
    }

    pub fn clock_us(&self) -> u64 {
        (self.now.as_micros() as i64 + self.clock_offset_us) as u64
    }

    pub fn connect(&mut self) {
        let out = self
            .client
            .connect(self.now, self.key.clone(), self.clock_us());
        self.client_outputs(None, out);
    }

    pub fn connect_other(&mut self, index: usize) {
        let (now, clock) = (self.now, self.clock_us());
        let key = self.others[index].key.clone();
        let out = self.others[index].client.connect(now, key, clock);
        self.client_outputs(Some(index), out);
    }

    /// Connects device 0 with another key it holds for this host.
    pub fn connect_with(&mut self, key: ClientKey) {
        let out = self.client.connect(self.now, key, self.clock_us());
        self.client_outputs(None, out);
    }

    /// Device 0 moves to a new address.
    pub fn roam(&mut self, addr: SimAddr) {
        self.addr = addr;
    }

    fn send(&mut self, dir: Dir, from: SimAddr, to: SimAddr, datagram: Vec<u8>) {
        if let Ok(header) = RecordHeader::parse(&datagram) {
            // No (key, counter) is ever sealed twice: a repeat under the same
            // direction, session and counter must be the identical bytes.
            let entry = self
                .sealed
                .entry((dir, header.session_id, header.counter))
                .or_insert_with(|| datagram.clone());
            assert_eq!(*entry, datagram, "a counter sealed twice under one key");
        }
        let kind = kind_of(dir, &datagram);
        let nth = {
            let count = self.counts.entry((dir, kind)).or_insert(0);
            *count += 1;
            *count - 1
        };
        let sent = Sent {
            at: self.now,
            dir,
            kind,
            nth,
            from,
            to,
            datagram,
        };
        let fate = (self.fault)(&sent);
        let base = self.now + self.latency;
        let packet = |sent: &Sent| Packet {
            dir,
            from,
            to,
            datagram: sent.datagram.clone(),
        };
        match fate {
            Fate::Deliver => self.enqueue(base, packet(&sent)),
            Fate::Drop => {}
            Fate::Duplicate { extra } => {
                self.enqueue(base, packet(&sent));
                self.enqueue(base + extra, packet(&sent));
            }
            Fate::Delay { extra } => self.enqueue(base + extra, packet(&sent)),
        }
        self.sent.push(sent);
    }

    fn enqueue(&mut self, at: Duration, packet: Packet) {
        self.seq += 1;
        self.in_flight.insert((at, self.seq), packet);
    }

    /// Injects a datagram as if it arrived now: to the host from device 0's
    /// address, or to device 0.
    pub fn inject(&mut self, dir: Dir, datagram: &[u8]) {
        let addr = self.addr;
        self.deliver(Packet {
            dir,
            from: addr,
            to: addr,
            datagram: datagram.to_vec(),
        });
    }

    /// Injects a datagram to the host from any source address.
    pub fn inject_from(&mut self, from: SimAddr, datagram: &[u8]) {
        self.deliver(Packet {
            dir: Dir::ToHost,
            from,
            to: from,
            datagram: datagram.to_vec(),
        });
    }

    /// Device 0's app ends its session.
    pub fn disconnect(&mut self) {
        let out = self.client.disconnect(0);
        self.client_outputs(None, out);
    }

    /// The host user removes device 0's record.
    pub fn revoke(&mut self) {
        let out = self.host.revoke(self.now, &self.key.dev_id);
        self.host_outputs(out);
    }

    fn deliver(&mut self, packet: Packet) {
        match packet.dir {
            Dir::ToHost => {
                let out = self.host.handle_datagram(
                    self.now,
                    &packet.from,
                    &packet.datagram,
                    &self.store,
                );
                self.host_outputs(out);
            }
            Dir::ToIpad => {
                let clock = self.clock_us();
                if packet.to == self.addr {
                    let out = self
                        .client
                        .handle_datagram(self.now, &packet.datagram, clock);
                    self.client_outputs(None, out);
                } else if let Some(index) = self.others.iter().position(|o| o.addr == packet.to) {
                    let out = self.others[index].client.handle_datagram(
                        self.now,
                        &packet.datagram,
                        clock,
                    );
                    self.client_outputs(Some(index), out);
                } else {
                    self.undelivered += 1;
                }
            }
        }
    }

    fn host_event(&mut self, dev_id: Id16, event: Event) {
        if dev_id == self.key.dev_id {
            self.events.push(event);
        } else {
            self.other_host_events.push((dev_id, event));
        }
    }

    fn host_outputs(&mut self, out: Vec<HostOutput<SimAddr>>) {
        let now = self.now;
        for output in out {
            match output {
                HostOutput::Send { to, datagram } => {
                    self.send(Dir::ToIpad, to, to, datagram);
                }
                HostOutput::ConfirmRequested { dev_id, session_id } => {
                    self.host_event(
                        dev_id,
                        Event::HostConfirmRequested {
                            at: now,
                            session_id,
                        },
                    );
                    if self.policy.decision_delay.is_zero() {
                        self.decide(dev_id, session_id);
                    } else {
                        self.jobs.push((
                            now + self.policy.decision_delay,
                            Job::Decide { dev_id, session_id },
                        ));
                    }
                }
                HostOutput::ConfirmCancelled { dev_id, session_id } => {
                    self.host_event(
                        dev_id,
                        Event::HostConfirmCancelled {
                            at: now,
                            session_id,
                        },
                    );
                }
                HostOutput::Committed {
                    dev_id,
                    session_id,
                    replaced,
                    timestamp_us,
                    ..
                } => {
                    self.host_event(
                        dev_id,
                        Event::HostCommitted {
                            at: now,
                            session_id,
                            replaced,
                        },
                    );
                    self.store
                        .timestamps
                        .borrow_mut()
                        .insert(dev_id, timestamp_us);
                    self.host_live.insert(dev_id, session_id);
                    self.next_media = now;
                    self.jobs.push((
                        now + self.policy.durable_delay,
                        Job::Ack { dev_id, session_id },
                    ));
                }
                HostOutput::Deliver {
                    dev_id,
                    session_id,
                    inner_type,
                    ..
                } => self.host_event(
                    dev_id,
                    Event::HostDelivered {
                        at: now,
                        session_id,
                        inner_type,
                    },
                ),
                HostOutput::Closed {
                    dev_id,
                    session_id,
                    reason,
                } => {
                    if self.host_live.get(&dev_id) == Some(&session_id) {
                        self.host_live.remove(&dev_id);
                    }
                    self.host_event(
                        dev_id,
                        Event::HostClosed {
                            at: now,
                            session_id,
                            reason,
                        },
                    );
                }
            }
        }
    }

    fn decide(&mut self, dev_id: Id16, session_id: u32) {
        let now = self.now;
        if self.policy.refuse {
            if !self.host.prepare_refusal(now, &dev_id, session_id) {
                return;
            }
            // Retire the record: tombstone it, and step 1 ends its sessions.
            self.store.tombstones.borrow_mut().push(dev_id);
            if self.policy.invalidate_before_refuse {
                let out = self.host.revoke(now, &dev_id);
                self.host_outputs(out);
            }
            if self.policy.durable_delay.is_zero() {
                let out = self.host.refuse(now, &dev_id, session_id, &self.store);
                self.host_outputs(out);
            } else {
                self.jobs.push((
                    now + self.policy.durable_delay,
                    Job::Refuse { dev_id, session_id },
                ));
            }
        } else {
            let out = self.host.commit(now, &dev_id, session_id);
            self.host_outputs(out);
        }
    }

    fn client_outputs(&mut self, device: Option<usize>, out: Vec<ClientOutput>) {
        let now = self.now;
        for output in out {
            let from = match device {
                None => self.addr,
                Some(i) => self.others[i].addr,
            };
            let event = match output {
                ClientOutput::Send(datagram) => {
                    self.send(Dir::ToHost, from, from, datagram);
                    continue;
                }
                ClientOutput::ReleaseAllInput => Event::IpadReleased { at: now },
                ClientOutput::Live { session_id, .. } => {
                    match device {
                        None => self.next_report = now,
                        Some(i) => self.others[i].next_report = now,
                    }
                    Event::IpadLive {
                        at: now,
                        session_id,
                    }
                }
                ClientOutput::Status { status, .. } => Event::IpadStatus { at: now, status },
                ClientOutput::Refused { status, .. } => Event::IpadRefused { at: now, status },
                ClientOutput::AttemptFailed { .. } => Event::IpadAttemptFailed { at: now },
                ClientOutput::PairAgain { .. } => Event::IpadPairAgain { at: now },
                ClientOutput::Busy { .. } => Event::IpadBusy { at: now },
                ClientOutput::Deliver {
                    session_id,
                    draining,
                    inner_type,
                    ..
                } => {
                    if !draining && inner_type == PacketType::Media as u8 {
                        let more = match device {
                            None => self.client.first_idr_decoded(session_id),
                            Some(i) => self.others[i].client.first_idr_decoded(session_id),
                        };
                        self.client_outputs(device, more);
                    }
                    Event::IpadDelivered {
                        at: now,
                        session_id,
                        draining,
                    }
                }
                ClientOutput::Closed { session_id, reason } => Event::IpadClosed {
                    at: now,
                    session_id,
                    reason,
                },
            };
            match device {
                None => self.events.push(event),
                Some(i) => self.others[i].events.push(event),
            }
        }
    }

    /// One tick: deliver what's due, run due jobs, app traffic, then poll.
    pub fn step(&mut self) {
        self.now += TICK;
        while let Some((&(at, seq), _)) = self.in_flight.iter().next() {
            if at > self.now {
                break;
            }
            let packet = self.in_flight.remove(&(at, seq)).unwrap();
            self.deliver(packet);
        }
        let now = self.now;
        let (due, keep): (Vec<_>, Vec<_>) = self.jobs.drain(..).partition(|(at, _)| *at <= now);
        self.jobs = keep;
        for (_, job) in due {
            match job {
                Job::Decide { dev_id, session_id } => self.decide(dev_id, session_id),
                Job::Refuse { dev_id, session_id } => {
                    let out = self.host.refuse(now, &dev_id, session_id, &self.store);
                    self.host_outputs(out);
                }
                Job::Ack { dev_id, session_id } => {
                    let out = self.host.ack(now, &dev_id, session_id, &self.store);
                    self.host_outputs(out);
                }
            }
        }
        if let Some(every) = self.policy.media_every {
            if !self.host_live.is_empty() && now >= self.next_media {
                self.next_media = now + every;
                let mut devices: Vec<Id16> = self.host_live.keys().copied().collect();
                devices.sort();
                for dev_id in devices {
                    if let Some((to, datagram)) =
                        self.host
                            .seal(now, &dev_id, PacketType::Media as u8, &[0x5A; MEDIA_BODY])
                    {
                        self.send(Dir::ToIpad, to, to, datagram);
                    }
                }
            }
        }
        if !self.host_live.is_empty() && now >= self.next_heartbeat {
            // Heartbeats every second while live, as v2 does.
            self.next_heartbeat = now + ms(1000);
            let mut devices: Vec<Id16> = self.host_live.keys().copied().collect();
            devices.sort();
            for dev_id in devices {
                if let Some((to, datagram)) = self.host.seal(
                    now,
                    &dev_id,
                    PacketType::Heartbeat as u8,
                    &[0; HEARTBEAT_BODY],
                ) {
                    self.send(Dir::ToIpad, to, to, datagram);
                }
            }
        }
        if let Some(every) = self.policy.report_every {
            if self.client.live_session().is_some() && now >= self.next_report {
                self.next_report = now + every;
                if let Some(datagram) =
                    self.client
                        .seal(now, PacketType::ReceiverReport as u8, &[0; REPORT_BODY])
                {
                    let addr = self.addr;
                    self.send(Dir::ToHost, addr, addr, datagram);
                }
            }
            for i in 0..self.others.len() {
                let other = &mut self.others[i];
                if other.client.live_session().is_some() && now >= other.next_report {
                    other.next_report = now + every;
                    if let Some(datagram) =
                        other
                            .client
                            .seal(now, PacketType::ReceiverReport as u8, &[0; REPORT_BODY])
                    {
                        let addr = other.addr;
                        self.send(Dir::ToHost, addr, addr, datagram);
                    }
                }
            }
        }
        let out = self.host.poll(now);
        self.host_outputs(out);
        let clock = self.clock_us();
        let out = self.client.poll(now, clock);
        self.client_outputs(None, out);
        for i in 0..self.others.len() {
            let out = self.others[i].client.poll(now, clock);
            self.client_outputs(Some(i), out);
        }
        self.host.check_invariants();
    }

    pub fn run_for(&mut self, duration: Duration) {
        let end = self.now + duration;
        while self.now < end {
            self.step();
        }
    }

    /// Runs until `done` holds or `limit` passes. Returns whether it held.
    pub fn run_until(&mut self, limit: Duration, mut done: impl FnMut(&Sim) -> bool) -> bool {
        let end = self.now + limit;
        while self.now < end {
            self.step();
            if done(self) {
                return true;
            }
        }
        false
    }

    pub fn host_live(&self) -> Option<u32> {
        self.host.live_session(&self.key.dev_id)
    }

    pub fn both_live_on_same_session(&self) -> bool {
        self.host_live().is_some() && self.host_live() == self.client.live_session()
    }

    pub fn other_live_on_same_session(&self, index: usize) -> bool {
        let other = &self.others[index];
        let host = self.host.live_session(&other.key.dev_id);
        host.is_some() && host == other.client.live_session()
    }

    pub fn commits(&self) -> Vec<u32> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::HostCommitted { session_id, .. } => Some(*session_id),
                _ => None,
            })
            .collect()
    }

    /// Sessions the host committed for `dev_id`.
    pub fn commits_for(&self, dev_id: &Id16) -> Vec<u32> {
        let events: Box<dyn Iterator<Item = &Event>> = if *dev_id == self.key.dev_id {
            Box::new(self.events.iter())
        } else {
            Box::new(
                self.other_host_events
                    .iter()
                    .filter(move |(d, _)| d == dev_id)
                    .map(|(_, e)| e),
            )
        };
        events
            .filter_map(|e| match e {
                Event::HostCommitted { session_id, .. } => Some(*session_id),
                _ => None,
            })
            .collect()
    }

    /// Every iPad, device 0 included, only ever goes live on a session the
    /// host committed for that iPad.
    pub fn check_live_only_on_commits(&self) {
        let devices = std::iter::once((self.key.dev_id, &self.events))
            .chain(self.others.iter().map(|o| (o.key.dev_id, &o.events)));
        for (dev_id, events) in devices {
            let commits = self.commits_for(&dev_id);
            for event in events {
                if let Event::IpadLive { session_id, .. } = event {
                    assert!(
                        commits.contains(session_id),
                        "an iPad went live on {session_id}, which the host never committed for it"
                    );
                }
            }
        }
    }

    /// Commits made at or before `at` whose session never went live on
    /// device 0. A later commit may still be on its way.
    pub fn unused_commits_before(&self, at: Duration) -> usize {
        let live: Vec<u32> = self
            .events
            .iter()
            .filter_map(|e| match e {
                Event::IpadLive { session_id, .. } => Some(*session_id),
                _ => None,
            })
            .collect();
        self.events
            .iter()
            .filter(|e| {
                matches!(e, Event::HostCommitted { at: t, session_id, .. }
                    if *t <= at && !live.contains(session_id))
            })
            .count()
    }

    /// Commits made at or before `at` that device 0 never went live on and
    /// that the host kept as live until its liveness timeout or maximum age
    /// ended them, or still holds: sessions left live after the iPad had
    /// dropped their keys. Commits the host itself ended sooner (replaced by
    /// a newer commit, revoked, shut down) aren't counted.
    pub fn stranded_commits_before(&self, at: Duration) -> usize {
        let live: Vec<u32> = self
            .events
            .iter()
            .filter_map(|e| match e {
                Event::IpadLive { session_id, .. } => Some(*session_id),
                _ => None,
            })
            .collect();
        let ended_by_host: Vec<u32> = self
            .events
            .iter()
            .filter_map(|e| match e {
                Event::HostClosed {
                    session_id, reason, ..
                } if !matches!(reason, CloseReason::Liveness | CloseReason::Expired) => {
                    Some(*session_id)
                }
                _ => None,
            })
            .collect();
        self.events
            .iter()
            .filter(|e| {
                matches!(e, Event::HostCommitted { at: t, session_id, .. }
                    if *t <= at && !live.contains(session_id) && !ended_by_host.contains(session_id))
            })
            .count()
    }

    /// Commits whose session never went live on that iPad.
    pub fn unused_commits(&self) -> usize {
        let live: Vec<u32> = self
            .events
            .iter()
            .filter_map(|e| match e {
                Event::IpadLive { session_id, .. } => Some(*session_id),
                _ => None,
            })
            .collect();
        self.commits()
            .iter()
            .filter(|id| !live.contains(id))
            .count()
    }

    pub fn sent_of(&self, dir: Dir, kind: Kind) -> Vec<&Sent> {
        self.sent
            .iter()
            .filter(|s| s.dir == dir && s.kind == kind)
            .collect()
    }
}
