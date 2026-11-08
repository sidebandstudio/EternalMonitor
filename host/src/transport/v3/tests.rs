//! The v3 host against a real credential store and the reference iPad
//! (`eternal_wire::v3::client`), in memory: commits, acks after the write,
//! sealed heartbeats, input through the v2 session, reconnects, roaming,
//! the three record operations, and crashes inside each of them.

use super::*;
use std::net::SocketAddr;

use eternal_credstore::keyring::MemKeyring;
use eternal_credstore::testing::{check_invariants, Live, Store, World};
use eternal_credstore::{Disk, NewRecord, Recovery};
use eternal_wire::v2::control::{encode_control, InputEvent, StreamConfig, FEATURE_WANTS_INPUT};
use eternal_wire::v3::client::{Client, ClientClose, ClientConfig, ClientKey, ClientOutput};
use eternal_wire::v3::wire::{HelloPayload, RecordState};
use eternal_wire::v3::SeededRandom;

const HOST_ID: Id16 = [0xB0; 16];
const DAY_S: u64 = 24 * 60 * 60;
/// The wall clock at the start of every test, in seconds.
const WALL_S: u64 = 1_800_000_000;
const STEP: Duration = Duration::from_millis(10);

fn id(n: u8) -> Id16 {
    [n; 16]
}

fn key(n: u8) -> Key32 {
    [n; 32]
}

struct TestConfig;

impl ConfigSource for TestConfig {
    fn stream_config(&self) -> StreamConfig {
        StreamConfig {
            stream_epoch: 1,
            width: 1920,
            height: 1080,
            fps: 60,
            codec: 0,
            flags: 0,
            bitrate_bps: 10_000_000,
        }
    }

    fn host_name(&self) -> String {
        "PC".into()
    }
}

fn record(dev: u8, slot: u8, replaces: Option<u8>) -> NewRecord {
    NewRecord {
        dev_id: id(dev),
        slot_id: id(slot),
        key: Box::new(Zeroizing::new(key(dev))),
        replaces: replaces.map(id),
        device_name: format!("iPad {dev}"),
    }
}

/// A world holding `records` (dev, slot, replaces, active, days old) at
/// rest.
fn world(records: &[(u8, u8, Option<u8>, bool, u64)]) -> World {
    let empty = World {
        keyring: MemKeyring::flushing(),
        ..World::default()
    };
    let (mut store, _, live) = empty.open();
    for &(dev, slot, replaces, active, days) in records {
        store
            .add_pending(record(dev, slot, replaces), WALL_S - days * DAY_S)
            .unwrap();
        if active {
            store.finalize(&id(dev)).unwrap();
        }
    }
    drop(store);
    live.at_rest()
}

/// The host, its v2 session and its store.
struct Rig {
    host: V3Host,
    session: Session,
    /// `None` once it couldn't reopen.
    store: Option<Store>,
    live: Live,
    start: Instant,
    now: Duration,
    /// Jobs wait for [`Rig::pump`] instead of running at once.
    hold_jobs: bool,
    /// A failed step is a passing fault: the writer's reopen succeeds.
    /// Otherwise the store stays failed.
    transient: bool,
    /// Everything the host decided, merged.
    actions: Vec<Actions>,
}

impl Rig {
    fn new(world: &World) -> Self {
        let (store, _, live) = world.open();
        Self::with(store, live)
    }

    fn with(store: Store, live: Live) -> Self {
        Self::with_cookies(store, live, CookieMode::UnderLoad)
    }

    fn with_cookies(store: Store, live: Live, cookies: CookieMode) -> Self {
        let start = Instant::now();
        let mut session = Session::new(100);
        session.require_v3();
        let host = V3Host::with_cookies(
            HOST_ID,
            Some(store.snapshot()),
            Pairings::Ready,
            Box::new(SeededRandom::new(9)),
            start,
            WALL_S * 1_000_000,
            cookies,
        );
        Self {
            host,
            session,
            store: Some(store),
            live,
            start,
            now: Duration::ZERO,
            hold_jobs: false,
            transient: true,
            actions: Vec::new(),
        }
    }

    fn at(&self) -> Instant {
        self.start + self.now
    }

    /// The store, while it's open.
    fn store(&self) -> &Store {
        self.store.as_ref().expect("the store is open")
    }

    /// Runs every job the host handed out, as the writer does: in order,
    /// reopening the store after a failure that needs it.
    fn pump(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        let mut sends = Vec::new();
        while let Some(more) = self.pump_one() {
            sends.extend(more);
        }
        sends
    }

    /// Runs the oldest job only; `None` with none queued.
    fn pump_one(&mut self) -> Option<Vec<(PeerId, Vec<u8>)>> {
        let job = self.host.outbox.pop_front()?;
        let done = match self.store.as_mut() {
            Some(store) => {
                let mut done = run_job(store, job);
                if self.transient {
                    self.live.steps.crash_at.set(None);
                }
                super::settle(&mut self.store, &mut done, Box::new(SeededRandom::new(11)));
                done
            }
            None => run_and_settle(&mut self.store, job, || Box::new(SeededRandom::new(11))),
        };
        if let Some(store) = &self.store {
            check_invariants(store);
        }
        let at = self.at();
        let actions = self.host.job_done(&mut self.session, done, &TestConfig, at);
        Some(self.take(actions))
    }

    fn take(&mut self, mut actions: Actions) -> Vec<(PeerId, Vec<u8>)> {
        let mut sends = std::mem::take(&mut actions.replies);
        sends.extend(actions.goodbye.take());
        self.actions.push(actions);
        sends
    }

    fn receive(&mut self, from: SocketAddr, datagram: &[u8]) -> Vec<(PeerId, Vec<u8>)> {
        self.receive_on(PeerId::udp(from), datagram)
    }

    fn receive_on(&mut self, from: PeerId, datagram: &[u8]) -> Vec<(PeerId, Vec<u8>)> {
        let at = self.at();
        let actions = self
            .host
            .handle_datagram(&mut self.session, from, datagram, &TestConfig, at);
        let mut sends = self.take(actions);
        if !self.hold_jobs {
            sends.extend(self.pump());
        }
        sends
    }

    /// The transport's tick: the session's (with its heartbeat), sealed,
    /// then the session table's.
    fn tick(&mut self, heartbeat: bool) -> Vec<(PeerId, Vec<u8>)> {
        let at = self.at();
        let mut actions = self.session.tick(&TestConfig, heartbeat, at);
        self.host.seal(&self.session, at, &mut actions);
        actions.merge(self.host.tick(&mut self.session, &TestConfig, at));
        let mut sends = self.take(actions);
        if !self.hold_jobs {
            sends.extend(self.pump());
        }
        sends
    }

    fn remove(&mut self, dev_ids: &[Id16]) -> Vec<(PeerId, Vec<u8>)> {
        let at = self.at();
        let actions = self
            .host
            .remove(&mut self.session, dev_ids, &TestConfig, at);
        let mut sends = self.take(actions);
        sends.extend(self.pump());
        sends
    }

    fn any_action(&self, f: impl Fn(&Actions) -> bool) -> bool {
        self.actions.iter().any(f)
    }

    fn state(&self, dev: u8) -> Option<RecordState> {
        self.store()
            .records()
            .into_iter()
            .find(|r| r.dev_id == id(dev))
            .map(|r| r.state)
    }

    fn tombstoned(&self, dev: u8) -> bool {
        self.store()
            .tombstones()
            .iter()
            .any(|(_, ids)| ids.contains(&id(dev)))
    }
}

/// The reference iPad at an address.
struct Pad {
    client: Client,
    addr: SocketAddr,
    /// Everything it reported, other than sends.
    seen: Vec<ClientOutput>,
}

impl Pad {
    fn new(port: u16) -> Self {
        let hello = HelloPayload {
            listen_port: port,
            feature_caps: FEATURE_WANTS_INPUT,
            screen_px_w: 2732,
            screen_px_h: 2048,
            device_id: 77,
            device_name: "Test iPad".into(),
            ..Default::default()
        };
        Self {
            client: Client::new(
                ClientConfig::new(HOST_ID),
                hello,
                Box::new(SeededRandom::new(u64::from(port))),
            ),
            addr: SocketAddr::from(([10, 0, 0, 5], port)),
            seen: Vec::new(),
        }
    }

    fn clock(rig: &Rig) -> u64 {
        WALL_S * 1_000_000 + u64::try_from(rig.now.as_micros()).unwrap()
    }

    fn live(&self) -> Option<u32> {
        self.client.live_session()
    }

    fn saw(&self, f: impl Fn(&ClientOutput) -> bool) -> bool {
        self.seen.iter().any(f)
    }

    fn delivered(&self, inner_type: u8) -> usize {
        self.seen
            .iter()
            .filter(
                |o| matches!(o, ClientOutput::Deliver { inner_type: t, .. } if *t == inner_type),
            )
            .count()
    }
}

/// Carries datagrams both ways until nothing is in flight.
fn settle(rig: &mut Rig, pad: &mut Pad, outputs: Vec<ClientOutput>) {
    let mut to_host: VecDeque<Vec<u8>> = VecDeque::new();
    let absorb = |pad: &mut Pad, outputs: Vec<ClientOutput>, to_host: &mut VecDeque<Vec<u8>>| {
        for output in outputs {
            match output {
                ClientOutput::Send(datagram) => to_host.push_back(datagram),
                other => pad.seen.push(other),
            }
        }
    };
    absorb(pad, outputs, &mut to_host);
    while let Some(datagram) = to_host.pop_front() {
        for (to, reply) in rig.receive(pad.addr, &datagram) {
            assert_eq!(to.link, LinkId::Udp);
            let clock = Pad::clock(rig);
            let outputs = pad.client.handle_datagram(rig.now, &reply, clock);
            absorb(pad, outputs, &mut to_host);
        }
    }
}

/// Hands host datagrams to the iPad and carries on.
fn to_pad(rig: &mut Rig, pad: &mut Pad, sends: Vec<(PeerId, Vec<u8>)>) {
    let clock = Pad::clock(rig);
    let mut outputs = Vec::new();
    for (to, datagram) in sends {
        assert_eq!(to.link, LinkId::Udp);
        outputs.extend(pad.client.handle_datagram(rig.now, &datagram, clock));
    }
    settle(rig, pad, outputs);
}

/// Sends a pairing message 1 and gives back what answers it, after the
/// cookie round trip if the host asks for one.
fn pair_msg1(rig: &mut Rig, from: SocketAddr, ipad: &mut PairingClient) -> Vec<(PeerId, Vec<u8>)> {
    let first = rig.receive(from, ipad.msg1());
    match first.as_slice() {
        [(_, reply)] => match ipad.handle_cookie(reply) {
            Some(msg1) => rig.receive(from, &msg1),
            None => first,
        },
        _ => first,
    }
}

/// Moves time on in steps, ticking both sides like the real loops do.
fn advance(rig: &mut Rig, pad: &mut Pad, by: Duration) {
    let end = rig.now + by;
    while rig.now < end {
        rig.now += STEP;
        let ms = rig.now.as_millis();
        let heartbeat = ms.is_multiple_of(1000);
        if ms.is_multiple_of(250) || heartbeat {
            let sends = rig.tick(heartbeat);
            to_pad(rig, pad, sends);
        }
        let clock = Pad::clock(rig);
        let outputs = pad.client.poll(rig.now, clock);
        settle(rig, pad, outputs);
    }
}

fn connect(rig: &mut Rig, pad: &mut Pad, dev: u8) {
    let clock = Pad::clock(rig);
    let outputs = pad.client.connect(
        rig.now,
        ClientKey {
            dev_id: id(dev),
            k_dev: key(dev),
        },
        clock,
    );
    settle(rig, pad, outputs);
}

fn input(event_id: u32) -> InputEvent {
    InputEvent {
        input_ver: 1,
        kind: crate::input::KIND_TOUCH,
        phase: crate::input::PHASE_BEGAN,
        buttons: 1,
        event_id,
        x_norm: 100,
        y_norm: 100,
        pressure_x1000: 0,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: 0,
        tilt_y: 0,
    }
}

fn send_input(rig: &mut Rig, pad: &mut Pad, event_id: u32) -> Option<(u32, InputEvent)> {
    let event = input(event_id);
    let body = &encode_control(0, 0, &ControlMessage::InputEvent(event))[CONTROL_HEADER_SIZE..];
    let record = pad
        .client
        .seal(rig.now, PacketType::InputEvent as u8, body)
        .expect("live");
    rig.actions.clear();
    let sends = rig.receive(pad.addr, &record);
    to_pad(rig, pad, sends);
    rig.actions.iter().find_map(|a| a.input)
}

#[test]
fn a_commit_streams_at_once_and_acks_after_the_write() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, false, 0)]));
    rig.hold_jobs = true;
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);

    // Committed: streaming, cut over, an IDR, before anything is durable.
    let session_id = rig.session.v3_session_id().expect("committed");
    assert!(rig.any_action(|a| a.cutover == Some(session_id)));
    assert!(rig.any_action(|a| a.force_idr));
    assert!(rig.any_action(|a| a.new_target == Some(PeerId::udp(pad.addr))));
    assert_eq!(rig.state(0x21), Some(RecordState::Pending));
    assert!(
        !pad.saw(|o| matches!(o, ClientOutput::Status { .. })),
        "no ack before the write"
    );

    // The write lands: Finalize made it active, and only now the ack.
    let sends = rig.pump();
    assert_eq!(rig.state(0x21), Some(RecordState::Active));
    assert!(rig.store().snapshot().last_committed_timestamp(&id(0x21)) > 0);
    to_pad(&mut rig, &mut pad, sends);
    assert_eq!(pad.live(), Some(session_id));
    assert!(pad.saw(|o| matches!(o, ClientOutput::Status { status, .. }
        if status.held == vec![(id(0x21), RecordState::Active)])));
    rig.host.table().check_invariants();
}

#[test]
fn heartbeats_go_sealed_and_keep_a_quiet_session_alive() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    let session_id = pad.live().expect("live");

    // Ten seconds with no input: the iPad's own 3 s liveness check, which
    // counts only what the host sends, never fires, because the host's
    // heartbeat arrives sealed every second.
    let sends = rig.tick(true);
    for (_, datagram) in &sends {
        assert_eq!(
            datagram[3],
            eternal_wire::v3::SEALED_TYPE,
            "nothing in the clear"
        );
    }
    to_pad(&mut rig, &mut pad, sends);
    let before = pad.delivered(PacketType::Heartbeat as u8);
    // The iPad reports every 500 ms, as the real one does, which keeps the
    // host's side alive.
    for _ in 0..20 {
        advance(&mut rig, &mut pad, Duration::from_millis(500));
        let report = eternal_wire::v2::control::ReceiverReport {
            frames_complete: 1,
            ..Default::default()
        };
        let body =
            &encode_control(0, 0, &ControlMessage::ReceiverReport(report))[CONTROL_HEADER_SIZE..];
        let record = pad
            .client
            .seal(rig.now, PacketType::ReceiverReport as u8, body)
            .unwrap();
        let sends = rig.receive(pad.addr, &record);
        to_pad(&mut rig, &mut pad, sends);
    }
    assert!(pad.delivered(PacketType::Heartbeat as u8) >= before + 9);
    assert_eq!(pad.live(), Some(session_id));
    assert!(!pad.saw(|o| matches!(o, ClientOutput::Closed { .. })));
}

#[test]
fn input_reaches_the_session_only_sealed() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    let session_id = pad.live().expect("live");

    let (relayed_session, event) = send_input(&mut rig, &mut pad, 41).expect("relayed");
    assert_eq!((relayed_session, event.event_id), (session_id, 41));

    // The same message in the clear, naming the live session, is dropped:
    // the session id in a record header proves nothing.
    let forged = encode_control(session_id, 9, &ControlMessage::InputEvent(input(42)));
    rig.actions.clear();
    let sends = rig.receive(pad.addr, &forged);
    assert!(sends.is_empty());
    assert!(!rig.any_action(|a| a.input.is_some()));

    // A replayed record is dropped too.
    let record = {
        let body =
            &encode_control(0, 0, &ControlMessage::InputEvent(input(43)))[CONTROL_HEADER_SIZE..];
        pad.client
            .seal(rig.now, PacketType::InputEvent as u8, body)
            .unwrap()
    };
    rig.actions.clear();
    rig.receive(pad.addr, &record);
    assert!(rig.any_action(|a| a.input.is_some()));
    rig.actions.clear();
    rig.receive(pad.addr, &record);
    assert!(!rig.any_action(|a| a.input.is_some()), "a replay");
}

fn v2_hello() -> Vec<u8> {
    let hello = eternal_wire::v2::control::Hello2 {
        proto_min: 2,
        proto_max: 2,
        client_nonce: 5,
        listen_port: 50_000,
        decoder_caps: 0,
        feature_caps: 0,
        screen_px_w: 0,
        screen_px_h: 0,
        screen_pt_w: 0,
        screen_pt_h: 0,
        refresh_hz: 0,
        device_name: "Old iPad".into(),
        device_id: 0,
        preferred_fps: 0,
        auth_token: [0; 16],
        pairing_code: 0,
    };
    encode_control(0, 1, &ControlMessage::Hello2(hello))
}

fn assert_version_3_refusal(sends: &[(PeerId, Vec<u8>)]) {
    assert_eq!(sends.len(), 1);
    let (_, ControlMessage::HelloAck(ack)) =
        eternal_wire::v2::control::parse_control(&sends[0].1).unwrap()
    else {
        panic!("a HelloAck");
    };
    assert_eq!(
        ack.status,
        eternal_wire::v2::control::HelloStatus::VersionUnsupported
    );
    assert_eq!(ack.accepted_version, 3);
}

#[test]
fn a_v2_hello_is_turned_away_naming_version_3() {
    let mut rig = Rig::new(&world(&[]));
    let sends = rig.receive(SocketAddr::from(([10, 0, 0, 9], 50_000)), &v2_hello());
    assert_version_3_refusal(&sends);
    assert!(!rig.session.is_active());
}

#[test]
fn a_v2_hello_over_usb_is_turned_away_too() {
    let mut rig = Rig::new(&world(&[]));
    let usb = PeerId::usb(17);
    let sends = rig.receive_on(usb, &v2_hello());
    assert_eq!(sends[0].0, usb);
    assert_version_3_refusal(&sends);
    assert!(!rig.session.is_active());
}

#[test]
fn a_failed_commit_write_keeps_a_queued_removal() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    rig.hold_jobs = true;
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21); // its commit's write waits
    let at = rig.at();
    let actions = rig
        .host
        .remove(&mut rig.session, &[id(0x21)], &TestConfig, at);
    rig.take(actions);

    // The commit's write fails; the Remove behind it hasn't run.
    let base = rig.live.steps.count.get();
    rig.live.steps.crash_at.set(Some(base));
    rig.pump_one().unwrap();
    assert_eq!(
        rig.state(0x21),
        Some(RecordState::Active),
        "not removed yet"
    );
    let handshakes = rig.host.table().handshakes_performed();
    connect(&mut rig, &mut pad, 0x21);
    assert_eq!(
        rig.host.table().handshakes_performed(),
        handshakes,
        "still invalid: the Remove holds it"
    );

    rig.pump();
    assert!(rig.tombstoned(0x21));
    assert_eq!(rig.host.held_invalid(), 0);
}

#[test]
fn a_second_finalize_conflicts_with_one_still_being_written() {
    // Slot 1: active 0x11, and two pending records that both replace it.
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x12, 1, Some(0x11), false, 0),
        (0x13, 1, Some(0x11), false, 0),
    ]));
    rig.hold_jobs = true;
    let mut first = Pad::new(50_000);
    connect(&mut rig, &mut first, 0x12);
    let streaming = rig.session.v3_session_id().expect("0x12 committed");

    // 0x13 confirms before 0x12's Finalize is written.
    let mut second = Pad::new(50_001);
    connect(&mut rig, &mut second, 0x13);
    assert_eq!(
        rig.session.v3_session_id(),
        Some(streaming),
        "0x12 untouched"
    );

    let sends = rig.pump();
    to_pad(&mut rig, &mut second, sends);
    assert_eq!(rig.state(0x12), Some(RecordState::Active), "the first wins");
    assert!(rig.tombstoned(0x11));
    assert!(rig.tombstoned(0x13), "the second is retired");
    assert!(
        second.saw(|o| matches!(o, ClientOutput::Refused { dev_id, status }
        if *dev_id == id(0x13) && status.tombstones.contains(&id(0x13))))
    );
    assert!(!second.saw(|o| matches!(o, ClientOutput::Closed { .. })));
    assert_eq!(rig.session.v3_session_id(), Some(streaming));
    assert!(rig.host.table().live_session(&id(0x12)).is_some());
}

#[test]
fn a_confirm_replacing_a_record_being_removed_is_refused() {
    // Slot 1: active 0x11, and 0x12 that replaces it.
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x12, 1, Some(0x11), false, 0),
    ]));
    rig.hold_jobs = true;
    let at = rig.at();
    let actions = rig
        .host
        .remove(&mut rig.session, &[id(0x11)], &TestConfig, at);
    rig.take(actions);

    // 0x12 confirms before the Remove is written: by the time its Finalize
    // ran, the record it replaces would be gone, so it conflicts.
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x12);
    assert!(rig.session.v3_session_id().is_none(), "not committed");
    let sends = rig.pump();
    to_pad(&mut rig, &mut pad, sends);
    assert!(
        pad.saw(|o| matches!(o, ClientOutput::Refused { dev_id, status }
        if *dev_id == id(0x12) && status.tombstones.contains(&id(0x12))))
    );
    assert!(rig.tombstoned(0x11) && rig.tombstoned(0x12));
    assert_eq!(rig.host.held_invalid(), 0);
}

/// A writer thread that's gone: every job comes back closed, whether it
/// couldn't be handed over or was never answered, and the pairings become
/// unavailable with nothing left held.
#[test]
fn a_writer_thats_gone_closes_the_store() {
    for answered_never in [false, true] {
        let world = world(&[(0x21, 2, None, true, 0)]);
        let (store, _, _) = world.open();
        let (jobs, job_rx) = std::sync::mpsc::channel();
        let (done_tx, done) = std::sync::mpsc::channel();
        // Either nothing takes jobs, or the thread took them and died.
        let _keep = answered_never.then_some(job_rx);
        drop(done_tx);
        let writer = Writer {
            jobs,
            done,
            notify: Arc::new(tokio::sync::Notify::new()),
        };
        let start = Instant::now();
        let host = V3Host::new(
            HOST_ID,
            Some(store.snapshot()),
            Pairings::Ready,
            Box::new(SeededRandom::new(9)),
            start,
            WALL_S * 1_000_000,
        );
        let mut runtime = V3Runtime::new(host, Some(writer));
        let mut session = Session::new(100);
        session.require_v3();
        runtime
            .host
            .remove(&mut session, &[id(0x21)], &TestConfig, start);
        runtime.flush();
        assert_eq!(runtime.jobs_in_flight(), 1);
        assert_eq!(runtime.host.held_invalid(), 1);
        let done = runtime.take_done();
        assert_eq!(done.len(), 1);
        assert!(done[0].closed && done[0].result.is_err());
        assert_eq!(runtime.jobs_in_flight(), 0);
        for done in done {
            runtime
                .host
                .job_done(&mut session, done, &TestConfig, start);
        }
        assert!(matches!(runtime.host.pairings(), Pairings::Unavailable(_)));
        assert!(runtime.host.snapshot().is_empty());
        assert_eq!(runtime.host.held_invalid(), 0);
    }
}

#[test]
fn an_expiry_check_racing_a_finalize_leaves_the_record_active() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, false, 8)]));
    rig.hold_jobs = true;
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21); // its Finalize waits
    let streaming = rig.session.v3_session_id().expect("committed");
    rig.tick(false); // the expiry check, on the old snapshot
    assert_eq!(rig.session.v3_session_id(), Some(streaming), "not ended");
    rig.pump();
    assert_eq!(rig.session.v3_session_id(), Some(streaming));
    assert!(rig.host.table().live_session(&id(0x21)).is_some());
    assert_eq!(rig.state(0x21), Some(RecordState::Active));
    assert!(!rig.tombstoned(0x21));
    assert_eq!(rig.host.held_invalid(), 0);
    rig.hold_jobs = false;
    connect(&mut rig, &mut pad, 0x21);
    assert!(pad.live().is_some(), "it still connects");
}

#[test]
fn a_replacing_commit_keeps_the_stream_going_to_the_new_session() {
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x12, 1, Some(0x11), false, 0),
    ]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x11);
    rig.actions.clear();
    connect(&mut rig, &mut pad, 0x12);
    let session_id = rig.session.v3_session_id().expect("the new session");
    let pass = rig
        .actions
        .iter()
        .find(|a| a.cutover == Some(session_id))
        .expect("the commit's pass");
    assert_eq!(pass.new_target, Some(PeerId::udp(pad.addr)));
    assert!(!pass.client_lost, "the stream goes on");
    assert!(!pass.end_cutover);
}

#[test]
fn a_takeover_of_a_held_session_keeps_the_stream_going() {
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x21, 2, None, true, 0),
    ]));
    let mut first = Pad::new(50_000);
    connect(&mut rig, &mut first, 0x11);
    // A report with frames, so the session holds when it goes quiet.
    let report = eternal_wire::v2::control::ReceiverReport {
        frames_complete: 1,
        ..Default::default()
    };
    let body =
        &encode_control(0, 0, &ControlMessage::ReceiverReport(report))[CONTROL_HEADER_SIZE..];
    let record = first
        .client
        .seal(rig.now, PacketType::ReceiverReport as u8, body)
        .unwrap();
    rig.receive(first.addr, &record);
    for _ in 0..14 {
        rig.now += Duration::from_millis(250);
        rig.tick(false);
    }
    assert!(rig.session.on_hold());

    let mut second = Pad::new(50_001);
    second.addr = SocketAddr::from(([10, 0, 0, 6], 50_001));
    rig.actions.clear();
    connect(&mut rig, &mut second, 0x21);
    let session_id = rig.session.v3_session_id().expect("taken over");
    let pass = rig
        .actions
        .iter()
        .find(|a| a.cutover == Some(session_id))
        .expect("the commit's pass");
    assert_eq!(pass.new_target, Some(PeerId::udp(second.addr)));
    assert!(!pass.client_lost);
}

#[test]
fn a_store_that_cant_reopen_makes_pairings_unavailable() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    let session_id = pad.live().expect("live");

    // Every store step fails from here, the reopen's too.
    rig.transient = false;
    let base = rig.live.steps.count.get();
    rig.live.steps.crash_at.set(Some(base));
    let sends = rig.remove(&[id(0x21)]);
    to_pad(&mut rig, &mut pad, sends);
    assert!(rig.store.is_none(), "it couldn't reopen");
    assert!(matches!(rig.host.pairings(), Pairings::Unavailable(_)));
    assert!(rig.host.snapshot().is_empty());
    assert!(
        pad.saw(|o| matches!(o, ClientOutput::Closed { session_id: s, .. } if *s == session_id))
    );

    // Nothing connects any more.
    let handshakes = rig.host.table().handshakes_performed();
    connect(&mut rig, &mut pad, 0x21);
    assert_eq!(rig.host.table().handshakes_performed(), handshakes);
    assert!(!rig.session.is_active());
}

/// The writer thread itself: a store that fails and can't reopen says so,
/// and every later job fails at once.
#[test]
fn the_writer_reports_a_store_it_cant_reopen() {
    use eternal_credstore::disk::MemDisk;
    use eternal_credstore::CredentialStore;
    let world = world(&[(0x21, 2, None, true, 0)]);
    // How many disk calls an open makes, so the next one after it fails.
    let calls = {
        let mut disk = world.disk.clone();
        disk.crash_at(usize::MAX);
        let (store, _) =
            CredentialStore::open(disk, world.keyring.clone(), Box::new(SeededRandom::new(1)))
                .unwrap();
        store.into_parts().0.calls()
    };
    let mut disk: MemDisk = world.disk.clone();
    disk.crash_at(calls);
    let (store, _) =
        CredentialStore::open(disk, world.keyring.clone(), Box::new(SeededRandom::new(1))).unwrap();
    let writer = Writer::spawn(store, || Box::new(SeededRandom::new(2))).unwrap();
    for _ in 0..2 {
        writer
            .jobs
            .send(StoreJob::Remove {
                dev_ids: vec![id(0x21)],
            })
            .unwrap();
        let done = writer.done.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(done.result.is_err());
        assert!(done.closed);
        assert!(done.snapshot.is_empty());
    }
}

#[test]
fn a_reconnect_supersedes_and_cuts_over_once() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    let first = pad.live().expect("live");
    advance(&mut rig, &mut pad, Duration::from_millis(100));

    rig.actions.clear();
    connect(&mut rig, &mut pad, 0x21);
    let second = pad.live().expect("live again");
    assert_ne!(first, second);
    assert_eq!(rig.session.v3_session_id(), Some(second));
    let cutovers: Vec<u32> = rig.actions.iter().filter_map(|a| a.cutover).collect();
    assert_eq!(cutovers, vec![second], "one cutover, to the new session");
    assert!(
        !pad.saw(|o| matches!(
            o,
            ClientOutput::Closed {
                reason: ClientClose::HostBye(_),
                ..
            }
        )),
        "no BYE on supersede"
    );
    assert!(rig.any_action(|a| a
        .ended
        .as_ref()
        .is_some_and(|e| e.reason == EndReason::Superseded)));
    // Input under the old session is gone with its keys.
    assert_eq!(
        send_input(&mut rig, &mut pad, 7).map(|(s, _)| s),
        Some(second)
    );
}

#[test]
fn media_and_control_follow_a_roaming_ipad() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    let session_id = pad.live().expect("live");

    pad.addr = SocketAddr::from(([10, 0, 7, 7], 50_000));
    rig.actions.clear();
    let (relayed, _) = send_input(&mut rig, &mut pad, 3).expect("input from the new address");
    assert_eq!(relayed, session_id);
    assert!(rig.any_action(|a| a.moved_target == Some(PeerId::udp(pad.addr))));
    assert!(
        !rig.any_action(|a| a.new_target.is_some() || a.cutover.is_some()),
        "a move is not a new session: nothing is released"
    );
    let sends = rig.tick(true);
    assert!(sends.iter().all(|(to, _)| *to == PeerId::udp(pad.addr)));
}

#[test]
fn remove_ends_the_session_at_once_and_retires_the_record() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    let session_id = pad.live().expect("live");

    rig.actions.clear();
    let sends = rig.remove(&[id(0x21)]);
    to_pad(&mut rig, &mut pad, sends);
    assert!(pad.saw(|o| matches!(o, ClientOutput::Closed { session_id: s, reason: ClientClose::HostBye(_) } if *s == session_id)));
    assert!(rig.any_action(|a| a.client_lost && a.end_cutover));
    assert!(!rig.session.is_active());
    assert!(rig.tombstoned(0x21));
    assert_eq!(rig.state(0x21), None);

    // The key can't connect again: its message 1s get no answer at all.
    connect(&mut rig, &mut pad, 0x21);
    advance(&mut rig, &mut pad, Duration::from_secs(3));
    assert!(pad.saw(|o| matches!(o, ClientOutput::AttemptFailed { .. })));
    assert!(pad.live().is_none());
}

#[test]
fn a_removed_key_is_refused_while_the_remove_is_still_being_written() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    assert!(pad.live().is_some());

    // Step 1 only: the store hasn't run the Remove yet.
    rig.hold_jobs = true;
    let at = rig.at();
    let actions = rig
        .host
        .remove(&mut rig.session, &[id(0x21)], &TestConfig, at);
    let sends = rig.take(actions);
    to_pad(&mut rig, &mut pad, sends);
    assert_eq!(
        rig.state(0x21),
        Some(RecordState::Active),
        "not written yet"
    );
    let handshakes = rig.host.table().handshakes_performed();
    connect(&mut rig, &mut pad, 0x21);
    assert_eq!(
        rig.host.table().handshakes_performed(),
        handshakes,
        "dropped before any Diffie-Hellman"
    );
    assert!(rig.host.table().pending_session(&id(0x21)).is_none());
    assert!(rig.host.table().live_session(&id(0x21)).is_none());
    assert!(!rig.session.is_active());

    rig.pump();
    assert!(rig.tombstoned(0x21));
}

#[test]
fn a_conflicting_pairing_is_refused_with_its_tombstone() {
    // Slot 1: active 0x11, and a pending 0x13 that replaces nothing, so
    // its Finalize conflicts.
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x13, 1, None, false, 0),
    ]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x13);
    assert!(pad.live().is_none());
    assert!(
        pad.saw(|o| matches!(o, ClientOutput::Refused { dev_id, status }
        if *dev_id == id(0x13) && status.tombstones.contains(&id(0x13))))
    );
    assert!(rig.tombstoned(0x13));
    assert_eq!(rig.state(0x11), Some(RecordState::Active), "untouched");
    assert!(!rig.session.is_active(), "never committed");

    // The active record still connects.
    connect(&mut rig, &mut pad, 0x11);
    assert!(pad.live().is_some());
}

#[test]
fn a_replacing_pairing_retires_the_record_it_replaces() {
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x12, 1, Some(0x11), false, 0),
    ]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x11);
    assert!(pad.live().is_some());

    // The same iPad finalizes its new record: its old session is superseded,
    // not a second client, and the old record is retired.
    connect(&mut rig, &mut pad, 0x12);
    let session_id = pad.live().expect("live under the new record");
    assert_eq!(rig.session.v3_session_id(), Some(session_id));
    assert_eq!(rig.state(0x12), Some(RecordState::Active));
    assert!(rig.tombstoned(0x11));
    assert!(pad.saw(|o| matches!(o, ClientOutput::Status { status, .. }
        if status.tombstones.contains(&id(0x11)))));
}

#[test]
fn a_second_ipad_is_told_the_host_is_busy() {
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x21, 2, None, true, 0),
    ]));
    let mut first = Pad::new(50_000);
    connect(&mut rig, &mut first, 0x11);
    let streaming = rig.session.v3_session_id();
    let mut second = Pad::new(50_001);
    second.addr = SocketAddr::from(([10, 0, 0, 6], 50_001));
    connect(&mut rig, &mut second, 0x21);
    assert!(second.live().is_none());
    assert!(second.saw(|o| matches!(o, ClientOutput::Busy { dev_id } if *dev_id == id(0x21))));
    assert!(rig.host.table().pending_session(&id(0x21)).is_none());
    assert_eq!(rig.session.v3_session_id(), streaming);
    assert_eq!(
        rig.state(0x21),
        Some(RecordState::Active),
        "nothing changed"
    );
    // Once the first iPad is gone, the second connects.
    let sends = rig.remove(&[id(0x11)]);
    to_pad(&mut rig, &mut first, sends);
    connect(&mut rig, &mut second, 0x21);
    assert!(second.live().is_some());
}

#[test]
fn old_pending_records_expire_and_their_keys_stop_working() {
    let mut rig = Rig::new(&world(&[
        (0x21, 2, None, false, 8),
        (0x31, 3, None, false, 1),
    ]));
    let mut pad = Pad::new(50_000);
    rig.tick(false);
    assert!(rig.tombstoned(0x21));
    assert_eq!(
        rig.state(0x31),
        Some(RecordState::Pending),
        "not old enough"
    );
    connect(&mut rig, &mut pad, 0x21);
    advance(&mut rig, &mut pad, Duration::from_secs(3));
    assert!(pad.live().is_none());
}

#[test]
fn liveness_ends_the_v3_session_with_a_sealed_goodbye() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    let session_id = pad.live().expect("live");
    // The iPad goes away: nothing more from it.
    let mut sends = Vec::new();
    for _ in 0..(HOLD_LIMIT.as_millis() / 250 + 2) {
        rig.now += Duration::from_millis(250);
        sends.extend(rig.tick(rig.now.as_millis().is_multiple_of(1000)));
    }
    assert!(!rig.session.is_active());
    assert!(rig.host.table().live_session(&id(0x21)).is_none());
    assert!(sends
        .iter()
        .all(|(_, d)| d[3] == eternal_wire::v3::SEALED_TYPE));
    to_pad(&mut rig, &mut pad, sends);
    assert!(
        pad.saw(|o| matches!(o, ClientOutput::Closed { session_id: s, .. } if *s == session_id))
    );
}

/// Fails the store at every step of a job, with the session live. The store
/// reopens as the writer does: the record is exactly before or after the
/// job, nothing is left invalid in memory, the session never gets an ack
/// for state that isn't durable, and a restart finds the same state.
///
/// With `in_commit` the crash points are those of the commit itself;
/// otherwise the session commits first and `job` is killed.
/// Each crash point runs with and without message 1's cookie round trip.
fn crash_each_step(
    records: &[(u8, u8, Option<u8>, bool, u64)],
    dev: u8,
    in_commit: bool,
    job: fn(&mut Rig, &mut Pad),
    check: fn(&Store, bool),
) {
    for cookies in [CookieMode::UnderLoad, CookieMode::Always] {
        crash_each_step_with(records, dev, in_commit, job, check, cookies);
    }
}

fn crash_each_step_with(
    records: &[(u8, u8, Option<u8>, bool, u64)],
    dev: u8,
    in_commit: bool,
    job: fn(&mut Rig, &mut Pad),
    check: fn(&Store, bool),
    cookies: CookieMode,
) {
    let world = world(records);
    for k in 0.. {
        let (store, _, live) = world.open();
        let mut rig = Rig::with_cookies(store, live, cookies);
        let mut pad = Pad::new(50_000);
        if !in_commit {
            connect(&mut rig, &mut pad, dev);
        }
        let base = rig.live.steps.count.get();
        rig.live.steps.crash_at.set(Some(base + k));
        if in_commit {
            connect(&mut rig, &mut pad, dev);
        }
        job(&mut rig, &mut pad);
        let crashed = rig.live.steps.count.get() > base + k;
        rig.live.steps.crash_at.set(None);
        assert_eq!(
            rig.host.table().cookie_replies() > 0,
            cookies == CookieMode::Always,
            "the cookie path ran only when required"
        );
        check_invariants(rig.store());
        check(rig.store(), crashed);
        assert_eq!(rig.host.held_invalid(), 0, "every job released its holds");
        let restarted = rig.live.same_boot();
        let (reopened, report, _) = restarted.open();
        check_invariants(&reopened);
        check(&reopened, crashed);
        if !crashed {
            assert!(report.recovery.is_empty() || report.recovery == vec![Recovery::RotatedAgain]);
            break;
        }
        assert!(k < 200, "a job of bounded steps");
    }
}

#[test]
fn a_crash_in_a_commit_never_acks_what_isnt_durable() {
    crash_each_step(
        &[(0x21, 2, None, false, 0)],
        0x21,
        true,
        |_, _| {},
        |store, crashed| {
            let state = store
                .records()
                .into_iter()
                .find(|r| r.dev_id == id(0x21))
                .map(|r| r.state);
            if crashed {
                assert!(matches!(
                    state,
                    Some(RecordState::Pending | RecordState::Active)
                ));
            } else {
                assert_eq!(state, Some(RecordState::Active));
            }
        },
    );
    // And a session whose write failed got no ack, and ended.
    let world = world(&[(0x21, 2, None, false, 0)]);
    let (store, _, live) = world.open();
    let mut rig = Rig::with(store, live);
    let base = rig.live.steps.count.get();
    rig.live.steps.crash_at.set(Some(base));
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21);
    assert!(
        !pad.saw(|o| matches!(o, ClientOutput::Status { .. })),
        "no ack"
    );
    assert!(!rig.session.is_active());
    assert!(rig.host.table().live_session(&id(0x21)).is_none());
}

#[test]
fn a_crash_in_a_conflict_refusal_leaves_it_retired_or_untouched() {
    crash_each_step(
        &[(0x11, 1, None, true, 0), (0x13, 1, None, false, 0)],
        0x13,
        true,
        |_, _| {},
        |store, crashed| {
            let held = store.records().iter().any(|r| r.dev_id == id(0x13));
            let tombstoned = store
                .tombstones()
                .iter()
                .any(|(_, ids)| ids.contains(&id(0x13)));
            assert_ne!(held, tombstoned, "exactly before or after");
            if !crashed {
                assert!(tombstoned);
            }
            assert!(store.records().iter().any(|r| r.dev_id == id(0x11)));
        },
    );
}

#[test]
fn a_crash_in_a_remove_mid_session_ends_the_session_either_way() {
    crash_each_step(
        &[(0x21, 2, None, true, 0)],
        0x21,
        false,
        |rig, pad| {
            let session_id = pad.live().expect("live before the remove");
            let sends = rig.remove(&[id(0x21)]);
            to_pad(rig, pad, sends);
            assert!(pad.saw(
                |o| matches!(o, ClientOutput::Closed { session_id: s, .. } if *s == session_id)
            ));
            assert!(!rig.session.is_active());
        },
        |store, crashed| {
            let held = store.records().iter().any(|r| r.dev_id == id(0x21));
            let tombstoned = store
                .tombstones()
                .iter()
                .any(|(_, ids)| ids.contains(&id(0x21)));
            assert_ne!(held, tombstoned, "exactly before or after");
            if !crashed {
                assert!(tombstoned);
            }
        },
    );
}

#[test]
fn a_crash_in_an_expiry_leaves_it_retired_or_untouched() {
    crash_each_step(
        &[(0x11, 1, None, true, 0), (0x21, 2, None, false, 8)],
        0x11,
        false,
        |rig, _| {
            rig.tick(false);
        },
        |store, crashed| {
            let held = store.records().iter().any(|r| r.dev_id == id(0x21));
            let tombstoned = store
                .tombstones()
                .iter()
                .any(|(_, ids)| ids.contains(&id(0x21)));
            assert_ne!(held, tombstoned);
            if !crashed {
                assert!(tombstoned);
            }
            assert!(store.records().iter().any(|r| r.dev_id == id(0x11)));
        },
    );
}

#[test]
fn a_store_that_cant_open_leaves_pairings_unavailable_and_changes_nothing() {
    let mut world = world(&[(0x21, 2, None, true, 0)]);
    world.keyring.put(Some(b"not a keyring blob".to_vec()));
    let before = world.clone();
    let opened = world.open_live(1, None, false).1;
    let Err(error) = opened else {
        panic!("an unreadable keyring can't open");
    };
    assert!(matches!(error, StoreError::KeyringUnreadable(_)));
    let host = V3Host::new(
        HOST_ID,
        None,
        Pairings::Unavailable(error.to_string()),
        Box::new(SeededRandom::new(1)),
        Instant::now(),
        WALL_S * 1_000_000,
    );
    assert!(matches!(host.pairings(), Pairings::Unavailable(_)));
    assert!(host.snapshot().is_empty());
    assert_eq!(
        world.keyring.get(),
        before.keyring.get(),
        "the blob is untouched"
    );
    assert_eq!(world.disk.list().unwrap(), before.disk.list().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn the_mac_stays_on_v2_until_its_keychain_check_passes() {
    let shared = crate::control::SharedControl::new(0, 10_000_000);
    enable(&shared);
    assert!(shared.v3.lock().is_none());
    assert!(unavailable_reason().is_some());
}

/// Natively on Windows: the store in the folder given (the host uses
/// `%LOCALAPPDATA%`), its keyring a DPAPI value under a test key, and a
/// commit through the real writer thread, acked once it's durable.
#[cfg(windows)]
#[test]
fn on_windows_a_commit_goes_through_dpapi_and_the_writer() {
    use eternal_credstore::keyring::DpapiRegistry;
    use eternal_credstore::{CredentialStore, Keyring, StdDisk};

    const SUBKEY: &str =
        r"Software\Classes\Local Settings\Software\EternalMonitor-Test-V3\credstore";
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = DpapiRegistry::new(SUBKEY, KEYRING_VALUE).delete();
            use windows::core::w;
            use windows::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};
            // SAFETY: a constant NUL-terminated key name.
            unsafe {
                let _ = RegDeleteTreeW(
                    HKEY_CURRENT_USER,
                    w!(r"Software\Classes\Local Settings\Software\EternalMonitor-Test-V3"),
                );
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    // The host's own folder is local app data, never roaming. (The test
    // writes to a folder of its own instead.)
    let local = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap());
    assert_eq!(
        store_dir().unwrap(),
        local.join("EternalMonitor").join("credentials")
    );
    let dir = std::env::temp_dir().join(format!("em-v3-native-{}", std::process::id()));
    let _cleanup = Cleanup(dir.clone());

    // A pending record, written as pairing will.
    {
        let (mut store, _) = CredentialStore::open(
            StdDisk::new(&dir).unwrap(),
            DpapiRegistry::new(SUBKEY, KEYRING_VALUE),
            Box::new(eternal_wire::v3::SystemRandom::default()),
        )
        .unwrap();
        store.add_pending(record(0x21, 2, None), WALL_S).unwrap();
    }
    assert!(DpapiRegistry::new(SUBKEY, KEYRING_VALUE)
        .load()
        .unwrap()
        .is_some());

    let shared = crate::control::SharedControl::new(0, 10_000_000);
    enable_in(&shared, &dir, SUBKEY);
    let host_id = std::fs::read(dir.join("host-id")).unwrap();
    assert_eq!(host_id.len(), 16);
    assert!(matches!(
        shared.v3.lock().as_ref().unwrap().host.pairings(),
        Pairings::Ready
    ));

    let mut config = ClientConfig::new(host_id.try_into().unwrap());
    config.msg1_window = Duration::from_secs(5);
    let mut client = Client::new(
        config,
        HelloPayload::default(),
        Box::new(SeededRandom::new(1)),
    );
    let start = Instant::now();
    let from = PeerId::udp(SocketAddr::from(([10, 0, 0, 5], 50_000)));
    let clock = || wall_us();
    let mut to_host: VecDeque<Vec<u8>> = client
        .connect(
            Duration::ZERO,
            ClientKey {
                dev_id: id(0x21),
                k_dev: key(0x21),
            },
            clock(),
        )
        .into_iter()
        .filter_map(|o| match o {
            ClientOutput::Send(d) => Some(d),
            _ => None,
        })
        .collect();
    let mut acked = false;
    let limit = std::time::Instant::now() + Duration::from_secs(10);
    while !acked {
        assert!(std::time::Instant::now() < limit, "no ack from the writer");
        let mut replies = Vec::new();
        while let Some(datagram) = to_host.pop_front() {
            let actions =
                on_datagram(&shared, from, &datagram, &TestConfig, Instant::now()).unwrap();
            replies.extend(actions.replies);
        }
        std::thread::sleep(Duration::from_millis(5));
        replies.extend(
            on_jobs_done(&shared, &TestConfig, Instant::now())
                .unwrap()
                .replies,
        );
        for (_, datagram) in replies {
            for output in client.handle_datagram(start.elapsed(), &datagram, clock()) {
                match output {
                    ClientOutput::Send(d) => to_host.push_back(d),
                    ClientOutput::Status { status, .. } => {
                        acked = status.held == vec![(id(0x21), RecordState::Active)];
                    }
                    _ => {}
                }
            }
        }
    }
    drop(shared);

    // Durable: a store opened again on the same folder and key has it active.
    let (store, _) = CredentialStore::open(
        StdDisk::new(&dir).unwrap(),
        DpapiRegistry::new(SUBKEY, KEYRING_VALUE),
        Box::new(eternal_wire::v3::SystemRandom::default()),
    )
    .unwrap();
    assert_eq!(
        store.records().first().map(|r| r.state),
        Some(RecordState::Active)
    );
}

/// A writer thread over a store of its own: it waits for `go`, answers
/// `answer` jobs, takes one more and panics, as the real writer would on a
/// bug inside a job.
fn panicking_writer(
    world: &World,
    answer: usize,
) -> (
    Writer,
    Arc<CredentialSnapshot>,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let (mut store, _) = eternal_credstore::CredentialStore::open(
        world.disk.clone(),
        world.keyring.clone(),
        Box::new(SeededRandom::new(4)),
    )
    .unwrap();
    let snapshot = store.snapshot();
    let (jobs, job_rx) = std::sync::mpsc::channel::<StoreJob>();
    let (done_tx, done) = std::sync::mpsc::channel::<JobDone>();
    let (go, go_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let _ = go_rx.recv();
        for _ in 0..answer {
            let job = job_rx.recv().unwrap();
            done_tx.send(run_job(&mut store, job)).unwrap();
        }
        let _job = job_rx.recv().unwrap();
        panic!("the writer panics in a job");
    });
    let writer = Writer {
        jobs,
        done,
        notify: Arc::new(tokio::sync::Notify::new()),
    };
    (writer, snapshot, go, thread)
}

fn runtime_with(
    writer: Writer,
    snapshot: Arc<CredentialSnapshot>,
    start: Instant,
) -> crate::control::SharedControl {
    let host = V3Host::new(
        HOST_ID,
        Some(snapshot),
        Pairings::Ready,
        Box::new(SeededRandom::new(9)),
        start,
        WALL_S * 1_000_000,
    );
    let shared = crate::control::SharedControl::new(0, 10_000_000);
    shared.session.lock().require_v3();
    *shared.v3.lock() = Some(V3Runtime::new(host, Some(writer)));
    shared
}

/// The writer panics after taking a job: nothing wakes the transport, but
/// its next tick sees the writer gone and closes the store.
#[test]
fn a_writer_that_panics_mid_job_is_seen_on_the_next_tick() {
    let world = world(&[(0x21, 2, None, true, 0)]);
    let (writer, snapshot, go, thread) = panicking_writer(&world, 0);
    let start = Instant::now();
    let shared = runtime_with(writer, snapshot, start);
    shared
        .v3
        .lock()
        .as_mut()
        .unwrap()
        .request_remove(vec![id(0x21)]);
    on_tick(&shared, &TestConfig, false, start).unwrap();
    assert_eq!(shared.v3.lock().as_ref().unwrap().jobs_in_flight(), 1);
    go.send(()).unwrap();
    assert!(thread.join().is_err(), "the writer panicked");

    on_tick(
        &shared,
        &TestConfig,
        false,
        start + Duration::from_millis(250),
    )
    .unwrap();
    let v3 = shared.v3.lock();
    let runtime = v3.as_ref().unwrap();
    assert_eq!(runtime.jobs_in_flight(), 0);
    assert_eq!(runtime.host.held_invalid(), 0);
    assert!(matches!(runtime.host.pairings(), Pairings::Unavailable(_)));
    assert!(runtime.host.snapshot().is_empty(), "no key left");
}

/// The writer is going (its job receiver is gone, its answers not yet):
/// an answer still unread when a later job finds no writer comes before
/// that job's close, and nothing read after the close brings a key back.
#[test]
fn a_close_never_comes_before_an_older_answer() {
    let world = world(&[(0x21, 2, None, true, 0), (0x22, 3, None, true, 0)]);
    let (mut store, _) = eternal_credstore::CredentialStore::open(
        world.disk.clone(),
        world.keyring.clone(),
        Box::new(SeededRandom::new(4)),
    )
    .unwrap();
    let (jobs, job_rx) = std::sync::mpsc::channel::<StoreJob>();
    let (done_tx, done) = std::sync::mpsc::channel::<JobDone>();
    let writer = Writer {
        jobs,
        done,
        notify: Arc::new(tokio::sync::Notify::new()),
    };
    let start = Instant::now();
    let shared = runtime_with(writer, store.snapshot(), start);
    let remove = |dev: u8| {
        shared
            .v3
            .lock()
            .as_mut()
            .unwrap()
            .request_remove(vec![id(dev)]);
    };

    // The writer takes a Remove and answers it, but nothing reads the
    // answer yet; then its job receiver goes.
    remove(0x21);
    on_tick(&shared, &TestConfig, false, start).unwrap();
    let job = job_rx.recv().unwrap();
    done_tx.send(run_job(&mut store, job)).unwrap();
    drop(job_rx);

    // The next job finds no writer: it closes, after the older answer.
    remove(0x22);
    on_tick(
        &shared,
        &TestConfig,
        false,
        start + Duration::from_millis(250),
    )
    .unwrap();
    let check = |what: &str| {
        let v3 = shared.v3.lock();
        let runtime = v3.as_ref().unwrap();
        assert_eq!(runtime.jobs_in_flight(), 0, "{what}");
        assert_eq!(runtime.host.held_invalid(), 0, "{what}");
        assert!(
            matches!(runtime.host.pairings(), Pairings::Unavailable(_)),
            "{what}"
        );
        assert!(
            runtime.host.snapshot().is_empty(),
            "{what}: no key comes back"
        );
    };
    check("after the tick");
    // The store-done wake runs late, and the writer's last end goes.
    on_jobs_done(&shared, &TestConfig, start + Duration::from_millis(260)).unwrap();
    check("after the late wake");
    drop(done_tx);
    on_jobs_done(&shared, &TestConfig, start + Duration::from_millis(270)).unwrap();
    check("after the writer's end");
}

/// Once a job comes back with the store closed, an answer read after it
/// (from before the close) changes nothing: no key comes back, and the
/// commit it finished gets no ack.
#[test]
fn a_closed_store_stays_closed() {
    let mut rig = Rig::new(&world(&[(0x21, 2, None, false, 0)]));
    rig.hold_jobs = true;
    let mut pad = Pad::new(50_000);
    connect(&mut rig, &mut pad, 0x21); // its commit's write waits
    let at = rig.at();
    let actions = rig.host.job_done(
        &mut rig.session,
        JobDone {
            job: StoreJob::Remove { dev_ids: vec![] },
            result: Err(StoreError::NeedsReopen),
            snapshot: Arc::default(),
            closed: true,
        },
        &TestConfig,
        at,
    );
    let sends = rig.take(actions);
    to_pad(&mut rig, &mut pad, sends);
    assert!(matches!(rig.host.pairings(), Pairings::Unavailable(_)));

    // The commit's answer, from a store that was still open.
    let sends = rig.pump();
    to_pad(&mut rig, &mut pad, sends);
    assert_eq!(rig.state(0x21), Some(RecordState::Active), "it was written");
    assert!(rig.host.snapshot().is_empty(), "but no key comes back");
    assert!(
        !pad.saw(|o| matches!(o, ClientOutput::Status { .. })),
        "no ack"
    );
    assert!(matches!(rig.host.pairings(), Pairings::Unavailable(_)));
    assert_eq!(rig.host.held_invalid(), 0);
}

use eternal_wire::v3::pairing::{PairResult, PairingClient};

fn host_addr() -> SocketAddr {
    SocketAddr::from(([10, 0, 0, 1], 47_700))
}

/// Runs a QR pairing from slot `slot`, optionally proving `old`. Returns the
/// iPad's pairing side and its new key, and what the host answered.
fn pair(
    rig: &mut Rig,
    from: SocketAddr,
    slot: u8,
    old: Option<u8>,
) -> (PairingClient, ClientKey, Option<PairResult>) {
    let at = rig.at();
    let qr = rig
        .host
        .open_pairing(at, host_addr(), "PC")
        .expect("the window opens");
    let mut ipad = PairingClient::start(
        &qr,
        &id(slot),
        "0.4.0",
        "Paired iPad",
        from.port(),
        &mut SeededRandom::new(u64::from(slot) + 40),
    )
    .unwrap();
    let msg2 = pair_msg1(rig, from, &mut ipad);
    assert_eq!(msg2.len(), 1, "message 2");
    let accept = ipad.read_msg2(&msg2[0].1).unwrap();
    let key = ClientKey {
        dev_id: accept.dev_id,
        k_dev: *accept.k_new,
    };
    let proof = old.map(|n| ClientKey {
        dev_id: id(n),
        k_dev: self::key(n),
    });
    let confirm = ipad.confirm(proof.as_ref()).unwrap();
    let answer = rig
        .receive(from, &confirm)
        .iter()
        .find_map(|(_, d)| ipad.handle_record(d));
    (ipad, key, answer)
}

fn connect_with(rig: &mut Rig, pad: &mut Pad, key: &ClientKey) {
    let clock = Pad::clock(rig);
    let outputs = pad.client.connect(rig.now, key.clone(), clock);
    settle(rig, pad, outputs);
}

#[test]
fn an_ipad_pairs_by_qr_then_connects_and_finalizes() {
    let mut rig = Rig::new(&world(&[]));
    let mut pad = Pad::new(50_000);
    let (_, key, answer) = pair(&mut rig, pad.addr, 7, None);
    let Some(PairResult::Done(status)) = answer else {
        panic!("PairDone, after the write: {answer:?}");
    };
    assert_eq!(status.held, vec![(key.dev_id, RecordState::Pending)]);
    let stored = rig
        .store()
        .records()
        .into_iter()
        .find(|r| r.dev_id == key.dev_id)
        .expect("written");
    assert_eq!(
        (
            stored.slot_id,
            stored.state,
            stored.replaces,
            stored.device_name.as_str()
        ),
        (id(7), RecordState::Pending, None, "Paired iPad")
    );
    assert!(rig
        .store()
        .snapshot()
        .key(&key.dev_id)
        .is_some_and(|k| *k == key.k_dev));

    // The new key connects, and that commit finalizes it.
    connect_with(&mut rig, &mut pad, &key);
    assert!(pad.live().is_some());
    assert_eq!(
        rig.store()
            .records()
            .into_iter()
            .find(|r| r.dev_id == key.dev_id)
            .map(|r| r.state),
        Some(RecordState::Active)
    );

    // The QR is used up: pairing again needs a new one.
    let at = rig.at();
    assert!(rig.host.open_pairing(at, host_addr(), "PC").is_ok());
}

#[test]
fn a_re_pairing_with_a_replace_proof_takes_over_the_slot() {
    let mut rig = Rig::new(&world(&[(0x11, 1, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    let (_, key, answer) = pair(&mut rig, pad.addr, 1, Some(0x11));
    assert!(matches!(answer, Some(PairResult::Done(_))));
    let stored = rig
        .store()
        .records()
        .into_iter()
        .find(|r| r.dev_id == key.dev_id)
        .unwrap();
    assert_eq!(stored.replaces, Some(id(0x11)), "the proof checked out");
    connect_with(&mut rig, &mut pad, &key);
    assert!(pad.live().is_some());
    assert!(
        rig.tombstoned(0x11),
        "the old record is retired at Finalize"
    );

    // Without a proof, the same slot's new record conflicts at its commit.
    let mut rig = Rig::new(&world(&[(0x11, 1, None, true, 0)]));
    let mut pad = Pad::new(50_000);
    let (_, key, answer) = pair(&mut rig, pad.addr, 1, None);
    assert!(matches!(answer, Some(PairResult::Done(_))));
    connect_with(&mut rig, &mut pad, &key);
    assert!(pad.saw(|o| matches!(o, ClientOutput::Refused { dev_id, .. } if *dev_id == key.dev_id)));
    assert_eq!(rig.state(0x11), Some(RecordState::Active));
}

#[test]
fn caps_refuse_a_pairing_and_a_full_host_opens_no_window() {
    // Slot 1 holds 1 active and 3 pending: the write is refused.
    let mut rig = Rig::new(&world(&[
        (0x11, 1, None, true, 0),
        (0x12, 1, Some(0x11), false, 0),
        (0x13, 1, Some(0x11), false, 0),
        (0x14, 1, Some(0x11), false, 0),
    ]));
    let pad = Pad::new(50_000);
    let (_, key, answer) = pair(&mut rig, pad.addr, 1, None);
    assert_eq!(
        answer,
        Some(PairResult::Refused(
            eternal_wire::v3::wire::PAIR_REFUSED_TOO_MANY
        ))
    );
    assert!(rig.store().snapshot().key(&key.dev_id).is_none());

    // 64 records: no QR at all.
    let records: Vec<(u8, u8, Option<u8>, bool, u64)> =
        (0..64u8).map(|n| (0x40 + n, n, None, true, 0)).collect();
    let mut rig = Rig::new(&world(&records));
    let at = rig.at();
    assert_eq!(
        rig.host.open_pairing(at, host_addr(), "PC").err(),
        Some(PairingClosed::Full)
    );
}

#[test]
fn pairings_that_are_unavailable_open_no_window() {
    let host = &mut V3Host::new(
        HOST_ID,
        None,
        Pairings::Unavailable("test".into()),
        Box::new(SeededRandom::new(1)),
        Instant::now(),
        WALL_S * 1_000_000,
    );
    assert_eq!(
        host.open_pairing(Instant::now(), host_addr(), "PC").err(),
        Some(PairingClosed::Unavailable)
    );
}

/// Fails the store at every step of the pairing write, with the store
/// reopened the way the writer does: the record is there or not, PairDone
/// went out only if it is, and the iPad's next confirm finishes the job.
#[test]
fn a_crash_in_the_pairing_write_never_answers_what_isnt_stored() {
    for cookies in [CookieMode::UnderLoad, CookieMode::Always] {
        crash_in_the_pairing_write(cookies);
    }
}

fn crash_in_the_pairing_write(cookies: CookieMode) {
    for k in 0.. {
        let world = world(&[]);
        let (store, _, live) = world.open();
        let mut rig = Rig::with_cookies(store, live, cookies);
        let pad = Pad::new(50_000);
        let at = rig.at();
        let qr = rig.host.open_pairing(at, host_addr(), "PC").unwrap();
        let mut ipad = PairingClient::start(
            &qr,
            &id(7),
            "0.4.0",
            "iPad",
            pad.addr.port(),
            &mut SeededRandom::new(3),
        )
        .unwrap();
        let msg2 = pair_msg1(&mut rig, pad.addr, &mut ipad);
        let accept = ipad.read_msg2(&msg2[0].1).unwrap();
        let dev_id = accept.dev_id;
        assert_eq!(
            rig.host.pairing.cookie_replies() > 0,
            cookies == CookieMode::Always
        );
        let base = rig.live.steps.count.get();
        rig.live.steps.crash_at.set(Some(base + k));
        let confirm = ipad.confirm(None).unwrap();
        let answer = rig
            .receive(pad.addr, &confirm)
            .iter()
            .find_map(|(_, d)| ipad.handle_record(d));
        let crashed = rig.live.steps.count.get() > base + k;
        rig.live.steps.crash_at.set(None);
        let held = rig.store().snapshot().key(&dev_id).is_some();
        assert_eq!(matches!(answer, Some(PairResult::Done(_))), held, "k={k}");
        assert_eq!(rig.host.held_invalid(), 0);
        // A restart finds the same.
        let (reopened, _, _) = rig.live.same_boot().open();
        check_invariants(&reopened);
        assert_eq!(reopened.snapshot().key(&dev_id).is_some(), held);
        if !held {
            // The iPad retries its confirm: written now, and answered.
            rig.now += Duration::from_millis(100);
            let retry = ipad.confirm(None).unwrap();
            let answer = rig
                .receive(pad.addr, &retry)
                .iter()
                .find_map(|(_, d)| ipad.handle_record(d));
            assert!(matches!(answer, Some(PairResult::Done(_))), "k={k}");
            assert!(rig.store().snapshot().key(&dev_id).is_some());
        }
        if !crashed {
            break;
        }
        assert!(k < 100, "a write of bounded steps");
    }
}

/// The store closes while a pairing write is in flight, or while a QR is
/// open: the window closes, the pairing ends unanswered, and no message 1
/// gets a message 2 any more.
#[test]
fn a_store_that_closes_ends_pairing() {
    for writing in [true, false] {
        let mut rig = Rig::new(&world(&[(0x11, 1, None, true, 0)]));
        rig.hold_jobs = true;
        let pad = Pad::new(50_000);
        let at = rig.at();
        let qr = rig.host.open_pairing(at, host_addr(), "PC").unwrap();
        let mut ipad = PairingClient::start(
            &qr,
            &id(7),
            "0.4.0",
            "iPad",
            pad.addr.port(),
            &mut SeededRandom::new(5),
        )
        .unwrap();
        if writing {
            let msg2 = rig.receive(pad.addr, ipad.msg1());
            ipad.read_msg2(&msg2[0].1).unwrap();
            let confirm = ipad.confirm(None).unwrap();
            rig.receive(pad.addr, &confirm);
            assert!(
                rig.host.pairing.in_flight(Duration::ZERO),
                "the write is in flight"
            );
        }
        // Every store step fails from here, the reopen's too.
        rig.transient = false;
        let base = rig.live.steps.count.get();
        rig.live.steps.crash_at.set(Some(base));
        let sends = if writing {
            rig.pump()
        } else {
            let at = rig.at();
            let actions = rig
                .host
                .remove(&mut rig.session, &[id(0x11)], &TestConfig, at);
            let mut sends = rig.take(actions);
            sends.extend(rig.pump());
            sends
        };
        assert!(matches!(rig.host.pairings(), Pairings::Unavailable(_)));
        assert!(
            sends.iter().all(|(_, d)| ipad.handle_record(d).is_none()),
            "no answer for a write that may or may not have landed"
        );
        assert!(!rig.host.pairing.is_open());
        assert_eq!(rig.host.pairing.session_id(), None);
        // A message 1 now gets nothing.
        let mut fresh = PairingClient::start(
            &qr,
            &id(8),
            "0.4.0",
            "iPad",
            pad.addr.port(),
            &mut SeededRandom::new(6),
        )
        .unwrap();
        assert!(rig.receive(pad.addr, fresh.msg1()).is_empty());
        assert!(fresh.read_msg2(&[]).is_err());
    }
}
