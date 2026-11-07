//! A deterministic simulation of one iPad and one host running the v3
//! reconnect state machines over a lossy network, on a simulated clock.
//!
//! Nothing here sleeps or reads a real clock, so results don't depend on
//! machine load. Shared by `tests/v3_session_sim.rs` and the
//! `v3_session_sim` fuzz target.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use eternal_wire::v2::PacketType;
use eternal_wire::v3::client::{Client, ClientClose, ClientConfig, ClientKey, ClientOutput};
use eternal_wire::v3::host::{CloseReason, HostConfig, HostCredentials, HostOutput, HostSessions};
use eternal_wire::v3::wire::{self, Accept, HelloPayload, RecordState, SlotStatus, V3Kind};
use eternal_wire::v3::{Id16, Key32, Random};

pub const HOST_ID: Id16 = [0xB1; 16];
pub const IPAD: u32 = 7;
pub const TICK: Duration = Duration::from_millis(5);
pub const REPORT_BODY: usize = 36;
pub const MEDIA_BODY: usize = 1200;

pub fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
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
    fn key(&self, dev_id: &Id16) -> Option<Key32> {
        if self.tombstones.borrow().contains(dev_id) {
            return None;
        }
        self.keys.get(dev_id).copied()
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
        let tombstones = self.tombstones.borrow().clone();
        let held = if tombstones.contains(dev_id) {
            vec![]
        } else {
            vec![(*dev_id, RecordState::Active)]
        };
        SlotStatus { held, tombstones }
    }
}

/// How the simulated host application answers the state machine.
#[derive(Clone, Copy)]
pub struct Policy {
    /// Refuse every confirm as a Finalize conflict.
    pub refuse: bool,
    /// Time from `Committed` to `ack` (the durable writes).
    pub durable_delay: Duration,
    /// Media cadence on a live session. The first packet after a commit is
    /// the IDR.
    pub media_every: Option<Duration>,
    /// Report cadence from the iPad while live.
    pub report_every: Option<Duration>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            refuse: false,
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

enum Job {
    Ack { session_id: u32 },
}

pub struct Sim {
    pub now: Duration,
    pub host: HostSessions<u32>,
    pub client: Client,
    pub store: Store,
    pub key: ClientKey,
    pub policy: Policy,
    pub latency: Duration,
    pub fault: FaultFn,
    pub events: Vec<Event>,
    pub sent: Vec<Sent>,
    /// The iPad's wall clock in µs, as an offset from `now`.
    pub clock_offset_us: i64,
    in_flight: BTreeMap<(Duration, u64), (Dir, Vec<u8>)>,
    seq: u64,
    counts: HashMap<(Dir, Kind), usize>,
    jobs: Vec<(Duration, Job)>,
    host_live: Option<u32>,
    next_media: Duration,
    next_report: Duration,
}

impl Sim {
    pub fn new(seed: u64) -> Self {
        Self::with_configs(seed, HostConfig::new(HOST_ID), ClientConfig::new(HOST_ID))
    }

    pub fn with_configs(seed: u64, host: HostConfig, client: ClientConfig) -> Self {
        let mut rng = XorShift(seed | 1);
        let mut dev_id = [0u8; 16];
        let mut k_dev = [0u8; 32];
        rng.fill(&mut dev_id);
        rng.fill(&mut k_dev);
        let key = ClientKey { dev_id, k_dev };
        let mut keys = HashMap::new();
        keys.insert(dev_id, k_dev);
        let hello = HelloPayload {
            listen_port: 50_000,
            screen_px_w: 2732,
            screen_px_h: 2048,
            refresh_hz: 120,
            device_name: "Sim iPad".into(),
            ..Default::default()
        };
        Self {
            now: Duration::from_secs(1000),
            host: HostSessions::new(host, Box::new(XorShift(rng.next() | 1))),
            client: Client::new(client, hello, Box::new(XorShift(rng.next() | 1))),
            store: Store {
                keys,
                timestamps: RefCell::new(HashMap::new()),
                tombstones: RefCell::new(Vec::new()),
            },
            key,
            policy: Policy::default(),
            latency: ms(10),
            fault: Box::new(|_| Fate::Deliver),
            events: Vec::new(),
            sent: Vec::new(),
            clock_offset_us: 1_762_000_000_000_000,
            in_flight: BTreeMap::new(),
            seq: 0,
            counts: HashMap::new(),
            jobs: Vec::new(),
            host_live: None,
            next_media: Duration::ZERO,
            next_report: Duration::ZERO,
        }
    }

    pub fn clock_us(&self) -> u64 {
        (self.now.as_micros() as i64 + self.clock_offset_us) as u64
    }

    pub fn connect(&mut self) {
        let out = self
            .client
            .connect(self.now, self.key.clone(), self.clock_us());
        self.client_outputs(out);
    }

    fn send(&mut self, dir: Dir, datagram: Vec<u8>) {
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
            datagram,
        };
        let fate = (self.fault)(&sent);
        let base = self.now + self.latency;
        match fate {
            Fate::Deliver => self.enqueue(base, dir, sent.datagram.clone()),
            Fate::Drop => {}
            Fate::Duplicate { extra } => {
                self.enqueue(base, dir, sent.datagram.clone());
                self.enqueue(base + extra, dir, sent.datagram.clone());
            }
            Fate::Delay { extra } => self.enqueue(base + extra, dir, sent.datagram.clone()),
        }
        self.sent.push(sent);
    }

    fn enqueue(&mut self, at: Duration, dir: Dir, datagram: Vec<u8>) {
        self.seq += 1;
        self.in_flight.insert((at, self.seq), (dir, datagram));
    }

    /// The host user removes the iPad's record.
    pub fn revoke(&mut self) {
        let out = self.host.revoke(self.now, &self.key.dev_id);
        self.host_outputs(out);
    }

    /// Injects a datagram as if it arrived from the network now.
    pub fn inject(&mut self, dir: Dir, datagram: &[u8]) {
        self.deliver(dir, datagram.to_vec());
    }

    fn deliver(&mut self, dir: Dir, datagram: Vec<u8>) {
        match dir {
            Dir::ToHost => {
                let out = self
                    .host
                    .handle_datagram(self.now, &IPAD, &datagram, &self.store);
                self.host_outputs(out);
            }
            Dir::ToIpad => {
                let out = self
                    .client
                    .handle_datagram(self.now, &datagram, self.clock_us());
                self.client_outputs(out);
            }
        }
    }

    fn host_outputs(&mut self, out: Vec<HostOutput<u32>>) {
        let now = self.now;
        for output in out {
            match output {
                HostOutput::Send { datagram, .. } => self.send(Dir::ToIpad, datagram),
                HostOutput::ConfirmRequested { dev_id, session_id } => {
                    self.events.push(Event::HostConfirmRequested {
                        at: now,
                        session_id,
                    });
                    let next = if self.policy.refuse {
                        self.store.tombstones.borrow_mut().push(dev_id);
                        self.host.refuse(now, &dev_id, session_id, &self.store)
                    } else {
                        self.host.commit(now, &dev_id, session_id)
                    };
                    self.host_outputs(next);
                }
                HostOutput::Committed {
                    dev_id,
                    session_id,
                    replaced,
                    timestamp_us,
                    ..
                } => {
                    self.events.push(Event::HostCommitted {
                        at: now,
                        session_id,
                        replaced,
                    });
                    self.store
                        .timestamps
                        .borrow_mut()
                        .insert(dev_id, timestamp_us);
                    self.host_live = Some(session_id);
                    self.next_media = now;
                    self.jobs
                        .push((now + self.policy.durable_delay, Job::Ack { session_id }));
                }
                HostOutput::Deliver {
                    session_id,
                    inner_type,
                    ..
                } => self.events.push(Event::HostDelivered {
                    at: now,
                    session_id,
                    inner_type,
                }),
                HostOutput::Closed {
                    session_id, reason, ..
                } => {
                    if self.host_live == Some(session_id) {
                        self.host_live = None;
                    }
                    self.events.push(Event::HostClosed {
                        at: now,
                        session_id,
                        reason,
                    });
                }
            }
        }
    }

    fn client_outputs(&mut self, out: Vec<ClientOutput>) {
        let now = self.now;
        for output in out {
            match output {
                ClientOutput::Send(datagram) => self.send(Dir::ToHost, datagram),
                ClientOutput::ReleaseAllInput => self.events.push(Event::IpadReleased { at: now }),
                ClientOutput::Live { session_id, .. } => {
                    self.events.push(Event::IpadLive {
                        at: now,
                        session_id,
                    });
                    self.next_report = now;
                }
                ClientOutput::Status { status, .. } => {
                    self.events.push(Event::IpadStatus { at: now, status })
                }
                ClientOutput::Refused { status, .. } => {
                    self.events.push(Event::IpadRefused { at: now, status })
                }
                ClientOutput::AttemptFailed { .. } => {
                    self.events.push(Event::IpadAttemptFailed { at: now })
                }
                ClientOutput::Deliver {
                    session_id,
                    draining,
                    inner_type,
                    ..
                } => {
                    self.events.push(Event::IpadDelivered {
                        at: now,
                        session_id,
                        draining,
                    });
                    if !draining && inner_type == PacketType::Media as u8 {
                        let more = self.client.first_idr_decoded(session_id);
                        self.client_outputs(more);
                    }
                }
                ClientOutput::Closed { session_id, reason } => {
                    self.events.push(Event::IpadClosed {
                        at: now,
                        session_id,
                        reason,
                    })
                }
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
            let (dir, datagram) = self.in_flight.remove(&(at, seq)).unwrap();
            self.deliver(dir, datagram);
        }
        let now = self.now;
        let (due, keep): (Vec<_>, Vec<_>) = self.jobs.drain(..).partition(|(at, _)| *at <= now);
        self.jobs = keep;
        for (_, job) in due {
            match job {
                Job::Ack { session_id } => {
                    let out = self
                        .host
                        .ack(self.now, &self.key.dev_id, session_id, &self.store);
                    self.host_outputs(out);
                }
            }
        }
        if let (Some(every), Some(_)) = (self.policy.media_every, self.host_live) {
            if self.now >= self.next_media {
                self.next_media = self.now + every;
                if let Some((_, datagram)) = self.host.seal(
                    self.now,
                    &self.key.dev_id,
                    PacketType::Media as u8,
                    &[0x5A; MEDIA_BODY],
                ) {
                    self.send(Dir::ToIpad, datagram);
                }
            }
        }
        if let Some(every) = self.policy.report_every {
            if self.client.live_session().is_some() && self.now >= self.next_report {
                self.next_report = self.now + every;
                if let Some(datagram) = self.client.seal(
                    self.now,
                    PacketType::ReceiverReport as u8,
                    &[0; REPORT_BODY],
                ) {
                    self.send(Dir::ToHost, datagram);
                }
            }
        }
        let out = self.host.poll(self.now);
        self.host_outputs(out);
        let out = self.client.poll(self.now, self.clock_us());
        self.client_outputs(out);
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

    pub fn commits(&self) -> Vec<u32> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::HostCommitted { session_id, .. } => Some(*session_id),
                _ => None,
            })
            .collect()
    }

    pub fn sent_of(&self, dir: Dir, kind: Kind) -> Vec<&Sent> {
        self.sent
            .iter()
            .filter(|s| s.dir == dir && s.kind == kind)
            .collect()
    }
}
