//! The v3 transport end to end: the real transport loop with v3 on, its
//! credential store on a temporary folder with the in-memory keyring, a
//! fake encoder, and a fake iPad built from the reference iPad state
//! machine (`eternal_wire::v3::client`) on loopback.
//!
//! Paused-clock simulations, as in `liveness_hold.rs`: time only moves when
//! every task waits, and the simulation waits for each datagram to be taken
//! and for every store job to be answered before it moves time, so a busy
//! machine can't change the result.
//!
//! Covered: connect and stream, the ack after the write, an idle session
//! kept alive by the sealed heartbeat, a reconnect, a roam to a new address,
//! a refusal with a tombstone, a Remove mid-session, and that cleartext
//! control naming the live session does nothing.
//!
//! The fake input sink is the off-Windows recorder; Windows injects for
//! real, so these run everywhere else.
#![cfg(not(windows))]

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eternal_credstore::keyring::MemKeyring;
use eternal_credstore::{CredentialStore, NewRecord, StdDisk};
use eternal_host::control::SharedControl;
use eternal_host::encoder::NALUnit;
use eternal_host::input::{recorder, CaptureGeometry, Injection, KIND_KEY, PHASE_BEGAN};
use eternal_host::transport;
use eternal_host::transport::probe;
use eternal_wire::v2::control::{
    encode_control, ControlMessage, InputEvent, ReceiverReport, CAP_DECODE_H264,
    CONTROL_HEADER_SIZE, FEATURE_WANTS_INPUT,
};
use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified, PacketType};
use eternal_wire::v3::client::{Client, ClientClose, ClientConfig, ClientKey, ClientOutput};
use eternal_wire::v3::wire::{HelloPayload, RecordState};
use eternal_wire::v3::{Id16, SeededRandom, Zeroizing};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The tests share the input recorder and ETERNAL_USB_DIRECT; run them one
/// at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const HOST_ID: Id16 = [0xB0; 16];
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const STEP: Duration = Duration::from_millis(10);
/// Real time a datagram or a store job may take before a test fails.
const REAL_LIMIT: Duration = Duration::from_secs(10);

fn id(n: u8) -> Id16 {
    [n; 16]
}

/// Waits until `done`, without moving the paused clock.
async fn settle(what: &str, mut done: impl FnMut() -> bool) {
    let limit = std::time::Instant::now() + REAL_LIMIT;
    while !done() {
        assert!(
            std::time::Instant::now() < limit,
            "{what} still pending after {REAL_LIMIT:?}"
        );
        tokio::task::yield_now().await;
    }
}

/// A store folder of its own, removed afterwards.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new() -> Self {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        let path =
            std::env::temp_dir().join(format!("em-v3-session-{}", u64::from_le_bytes(bytes)));
        Self(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `(dev, slot, replaces, active)`.
fn seed(
    folder: &Folder,
    keyring: &MemKeyring,
    records: &[(u8, u8, Option<u8>, bool)],
) -> MemKeyring {
    let (mut store, _) = CredentialStore::open(
        StdDisk::new(&folder.0).unwrap(),
        keyring.clone(),
        Box::new(SeededRandom::new(3)),
    )
    .unwrap();
    for &(dev, slot, replaces, active) in records {
        store
            .add_pending(
                NewRecord {
                    dev_id: id(dev),
                    slot_id: id(slot),
                    key: Box::new(Zeroizing::new([dev; 32])),
                    replaces: replaces.map(id),
                    device_name: format!("iPad {dev}"),
                },
                eternal_host::transport::v3::wall_us() / 1_000_000,
            )
            .unwrap();
        if active {
            store.finalize(&id(dev)).unwrap();
        }
    }
    store.into_parts().1
}

/// A fake v3 iPad: the reference state machine on a loopback socket. It
/// reports every 500 ms once live, as the app does, and counts the video it
/// gets per session.
struct Ipad {
    socket: std::net::UdpSocket,
    host: SocketAddr,
    client: Client,
    start: Instant,
    /// What the transport counted for this socket before the test.
    sent_before: u64,
    taken_before: u64,
    received: u64,
    delivered: u64,
    seen: Vec<(Duration, ClientOutput)>,
    /// Session and keyframe flag of each video datagram.
    video: Vec<(u32, bool)>,
    /// Cleartext control that reached the iPad.
    cleartext: usize,
    next_report: Duration,
}

impl Ipad {
    fn new(host: SocketAddr, start: Instant) -> Self {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let address = socket.local_addr().unwrap();
        let hello = HelloPayload {
            listen_port: address.port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_WANTS_INPUT,
            screen_px_w: 1280,
            screen_px_h: 720,
            screen_pt_w: 1280,
            screen_pt_h: 720,
            refresh_hz: 60,
            device_id: 77,
            device_name: "v3 test iPad".into(),
            ..Default::default()
        };
        Self {
            client: Client::new(
                ClientConfig::new(HOST_ID),
                hello,
                Box::new(SeededRandom::new(u64::from(address.port()))),
            ),
            sent_before: probe::sent_to(address),
            taken_before: probe::taken_from(address),
            socket,
            host,
            start,
            received: 0,
            delivered: 0,
            seen: Vec::new(),
            video: Vec::new(),
            cleartext: 0,
            next_report: Duration::MAX,
        }
    }

    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn clock_us(&self) -> u64 {
        eternal_host::transport::v3::wall_us()
    }

    /// Moves to a new socket, as a roaming iPad gets a new address.
    fn roam(&mut self) {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let address = socket.local_addr().unwrap();
        self.socket = socket;
        self.sent_before = probe::sent_to(address);
        self.taken_before = probe::taken_from(address);
        self.received = 0;
        self.delivered = 0;
    }

    async fn send(&mut self, datagram: &[u8]) {
        self.socket.send_to(datagram, self.host).unwrap();
        self.delivered += 1;
        let (address, want) = (self.address(), self.taken_before + self.delivered);
        settle("a packet to the PC", || probe::taken_from(address) >= want).await;
    }

    async fn act(&mut self, outputs: Vec<ClientOutput>) {
        for output in outputs {
            match output {
                ClientOutput::Send(datagram) => self.send(&datagram).await,
                other => {
                    if let ClientOutput::Live { .. } = other {
                        self.next_report = self.now() + Duration::from_millis(500);
                    }
                    self.seen.push((self.now(), other));
                }
            }
        }
    }

    async fn connect(&mut self, dev: u8) {
        let (now, clock) = (self.now(), self.clock_us());
        let outputs = self.client.connect(
            now,
            ClientKey {
                dev_id: id(dev),
                k_dev: [dev; 32],
            },
            clock,
        );
        self.act(outputs).await;
    }

    /// Takes everything the PC has sent this socket so far, and answers it.
    async fn drain(&mut self) {
        let mut bytes = [0; 2048];
        let limit = std::time::Instant::now() + REAL_LIMIT;
        loop {
            let Ok((len, _)) = self.socket.recv_from(&mut bytes) else {
                if self.received >= probe::sent_to(self.address()) - self.sent_before {
                    return;
                }
                assert!(
                    std::time::Instant::now() < limit,
                    "a packet to the iPad still in flight"
                );
                tokio::task::yield_now().await;
                continue;
            };
            self.received += 1;
            let datagram = bytes[..len].to_vec();
            match classify(&datagram) {
                Classified::Media { .. } => {
                    let (header, _) = MediaHeader::decode(&datagram).unwrap();
                    self.video.push((header.session_id, header.is_keyframe));
                }
                Classified::Control(_) => self.cleartext += 1,
                _ => {
                    let (now, clock) = (self.now(), self.clock_us());
                    let outputs = self.client.handle_datagram(now, &datagram, clock);
                    self.act(outputs).await;
                }
            }
        }
    }

    async fn step(&mut self) {
        self.drain().await;
        let (now, clock) = (self.now(), self.clock_us());
        let outputs = self.client.poll(now, clock);
        self.act(outputs).await;
        if self.client.live_session().is_some() && now >= self.next_report {
            self.next_report = now + Duration::from_millis(500);
            let frames = self.video.len() as u32;
            let report = ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 1,
                highest_seq: frames,
                frames_complete: frames,
                ..ReceiverReport::default()
            });
            self.seal_and_send(PacketType::ReceiverReport, &report)
                .await;
        }
    }

    async fn seal_and_send(&mut self, packet_type: PacketType, message: &ControlMessage) {
        let body = encode_control(0, 0, message)[CONTROL_HEADER_SIZE..].to_vec();
        if let Some(record) = self.client.seal(self.now(), packet_type as u8, &body) {
            self.send(&record).await;
        }
    }

    fn live(&self) -> Option<u32> {
        self.client.live_session()
    }

    fn saw(&self, f: impl Fn(&ClientOutput) -> bool) -> bool {
        self.seen.iter().any(|(_, o)| f(o))
    }

    fn heartbeats(&self) -> usize {
        self.seen
            .iter()
            .filter(|(_, o)| {
                matches!(o, ClientOutput::Deliver { inner_type, .. } if *inner_type == PacketType::Heartbeat as u8)
            })
            .count()
    }

    fn video_of(&self, session_id: u32) -> usize {
        self.video.iter().filter(|(s, _)| *s == session_id).count()
    }
}

fn key_event(event_id: u32) -> ControlMessage {
    ControlMessage::InputEvent(InputEvent {
        input_ver: 1,
        kind: KIND_KEY,
        phase: PHASE_BEGAN,
        buttons: 0,
        event_id,
        x_norm: 0,
        y_norm: 0,
        pressure_x1000: 0,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0x04,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: 0,
        tilt_y: 0,
    })
}

/// The real transport loop with v3 on, a fake encoder at 60 fps, and an
/// iPad.
struct Sim {
    shared: SharedControl,
    transport: tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
    encoder: tokio::task::JoinHandle<()>,
    ipad: Ipad,
    injected: Vec<Injection>,
    _folder: Folder,
}

impl Sim {
    async fn start(records: &[(u8, u8, Option<u8>, bool)]) -> Self {
        let start = Instant::now();
        let folder = Folder::new();
        let keyring = seed(&folder, &MemKeyring::flushing(), records);
        let (port, shared, nal_tx, transport) = loop {
            let port = std::net::UdpSocket::bind("127.0.0.1:0")
                .and_then(|s| s.local_addr())
                .map(|a| a.port())
                .unwrap();
            let shared = SharedControl::new(port, 10_000_000);
            shared.running.store(true, Ordering::SeqCst);
            *shared.capture_geometry.lock() = Some(CaptureGeometry {
                left: 0,
                top: 0,
                width: 1280,
                height: 720,
            });
            transport::v3::enable_with(
                &shared,
                HOST_ID,
                StdDisk::new(&folder.0).unwrap(),
                keyring.clone(),
                || Box::new(SeededRandom::new(5)),
            );
            let (nal_tx, nal_rx) = mpsc::channel(4);
            let (audio_tx, audio_rx) = mpsc::channel(4);
            let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
            let control = shared.clone();
            let transport = tokio::spawn(async move {
                let _audio = audio_tx;
                transport::start_sender(nal_rx, audio_rx, port, control, supervisor_tx).await
            });
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            if !transport.is_finished() {
                break (port, shared, nal_tx, transport);
            }
            assert!(
                transport.await.unwrap().is_err(),
                "only a taken port stops it early"
            );
        };
        let encoder = {
            let force = shared.force_next_idr.clone();
            tokio::spawn(async move {
                let mut next = Instant::now();
                for sequence in 0.. {
                    tokio::time::sleep_until(next).await;
                    next += FRAME_INTERVAL;
                    let is_keyframe = force.swap(false, Ordering::SeqCst);
                    let nal = NALUnit {
                        data: vec![0, 0, 0, 1, if is_keyframe { 0x65 } else { 0x41 }, 0xAA],
                        sequence,
                        timestamp: Instant::now().into_std(),
                        encode_duration_us: 0,
                        is_keyframe,
                    };
                    if nal_tx.send(nal).await.is_err() {
                        break;
                    }
                }
            })
        };
        recorder::take();
        let host = SocketAddr::from(([127, 0, 0, 1], port));
        Self {
            ipad: Ipad::new(host, start),
            shared,
            transport,
            encoder,
            injected: Vec::new(),
            _folder: folder,
        }
    }

    /// Store jobs not yet answered.
    fn jobs(&self) -> usize {
        self.shared
            .v3
            .lock()
            .as_ref()
            .map_or(0, |v3| v3.jobs_in_flight())
    }

    async fn run_for(&mut self, duration: Duration) {
        let end = self.ipad.now() + duration;
        while self.ipad.now() < end {
            self.ipad.step().await;
            let shared = self.shared.clone();
            settle("a store job", || {
                shared
                    .v3
                    .lock()
                    .as_ref()
                    .map_or(0, |v3| v3.jobs_in_flight())
                    == 0
            })
            .await;
            tokio::time::sleep(STEP).await;
            self.ipad.drain().await;
            self.injected.extend(recorder::take());
        }
        assert_eq!(self.jobs(), 0);
    }

    fn state(&self, dev: u8) -> Option<RecordState> {
        let v3 = self.shared.v3.lock();
        let snapshot = v3.as_ref().unwrap().host.snapshot();
        snapshot
            .slot_status(&id(dev))
            .held
            .iter()
            .find(|(d, _)| *d == id(dev))
            .map(|(_, s)| *s)
    }

    async fn stop(self) {
        self.shared.stop();
        tokio::time::timeout(Duration::from_secs(5), self.transport)
            .await
            .expect("the transport stops")
            .unwrap()
            .unwrap();
        self.encoder.abort();
    }
}

fn run(test: impl std::future::Future<Output = ()>) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    eternal_host::clock::init();
    std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(test);
}

/// Connect, stream, stay idle on the heartbeat, reconnect, roam, then a
/// Remove: one iPad's whole life on a v3 host.
#[test]
fn a_v3_ipad_connects_streams_reconnects_roams_and_is_removed() {
    run(async {
        let mut sim = Sim::start(&[(0x21, 2, None, false)]).await;
        sim.ipad.connect(0x21).await;
        sim.run_for(Duration::from_secs(1)).await;
        let first = sim.ipad.live().expect("live");
        assert_eq!(sim.state(0x21), Some(RecordState::Active), "finalized");
        assert!(
            sim.ipad
                .saw(|o| matches!(o, ClientOutput::Status { status, .. }
            if status.held == vec![(id(0x21), RecordState::Active)])),
            "the ack, after the write"
        );
        let video = sim.ipad.video_of(first);
        assert!(video > 30, "video under the session: {video}");
        assert!(
            sim.ipad
                .video
                .iter()
                .find(|(s, _)| *s == first)
                .is_some_and(|(_, key)| *key),
            "it starts with a keyframe"
        );

        // Sealed input lands; the same event in the clear doesn't.
        sim.ipad
            .seal_and_send(PacketType::InputEvent, &key_event(1))
            .await;
        let forged = encode_control(first, 99, &key_event(2));
        sim.ipad.send(&forged).await;
        sim.run_for(Duration::from_millis(100)).await;
        let downs = sim
            .injected
            .iter()
            .filter(|i| matches!(i, Injection::KeyDown { .. }))
            .count();
        assert_eq!(downs, 1, "only the sealed key: {:?}", sim.injected);

        // Idle: no input, only reports. The iPad's own 3 s liveness check
        // would end the session without the host's sealed heartbeat.
        let heartbeats = sim.ipad.heartbeats();
        sim.run_for(Duration::from_secs(10)).await;
        assert_eq!(sim.ipad.live(), Some(first));
        assert!(sim.ipad.heartbeats() >= heartbeats + 9);
        assert!(!sim.ipad.saw(|o| matches!(o, ClientOutput::Closed { .. })));
        assert_eq!(sim.ipad.cleartext, 0, "nothing in the clear");

        // A reconnect: a new session, video moves to it.
        sim.ipad.connect(0x21).await;
        sim.run_for(Duration::from_secs(1)).await;
        let second = sim.ipad.live().expect("live again");
        assert_ne!(first, second);
        let before = sim.ipad.video_of(second);
        sim.run_for(Duration::from_millis(500)).await;
        assert!(sim.ipad.video_of(second) > before);

        // A roam: the iPad's next record comes from a new address, and video
        // follows it there.
        sim.ipad.roam();
        sim.run_for(Duration::from_secs(1)).await;
        let roamed = sim.ipad.video.len();
        sim.run_for(Duration::from_millis(500)).await;
        assert!(sim.ipad.video.len() > roamed, "video at the new address");
        assert_eq!(sim.ipad.live(), Some(second), "the same session");

        // Remove, mid-session: a sealed goodbye, and the key never connects
        // again.
        sim.shared
            .v3
            .lock()
            .as_mut()
            .unwrap()
            .request_remove(vec![id(0x21)]);
        sim.run_for(Duration::from_millis(500)).await;
        assert!(sim.ipad.saw(
            |o| matches!(o, ClientOutput::Closed { session_id, reason: ClientClose::HostBye(_) }
            if *session_id == second)
        ));
        assert_eq!(sim.state(0x21), None);
        assert!(!sim.shared.session.lock().is_active());
        sim.ipad.connect(0x21).await;
        sim.run_for(Duration::from_secs(3)).await;
        assert!(sim.ipad.live().is_none());
        assert!(sim
            .ipad
            .saw(|o| matches!(o, ClientOutput::AttemptFailed { .. })));
        sim.stop().await;
    });
}

/// A pending record whose Finalize conflicts: the iPad gets a sealed
/// refusal whose status tombstones its key, and the active record of the
/// slot still connects.
#[test]
fn a_conflicting_record_is_refused_with_its_tombstone() {
    run(async {
        let mut sim = Sim::start(&[(0x11, 1, None, true), (0x13, 1, None, false)]).await;
        sim.ipad.connect(0x13).await;
        sim.run_for(Duration::from_secs(1)).await;
        assert!(sim.ipad.live().is_none());
        assert!(sim
            .ipad
            .saw(|o| matches!(o, ClientOutput::Refused { dev_id, status }
            if *dev_id == id(0x13) && status.tombstones.contains(&id(0x13)))));
        assert!(!sim.shared.session.lock().is_active());
        sim.ipad.connect(0x11).await;
        sim.run_for(Duration::from_secs(1)).await;
        assert!(sim.ipad.live().is_some());
        sim.stop().await;
    });
}

/// v3 off, as shipped: a v3 message 1 gets exactly today's answer, a
/// cleartext `VersionUnsupported` naming version 2, and nothing starts.
#[test]
fn with_v3_off_a_v3_message_1_gets_todays_answer() {
    run(async {
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|s| s.local_addr())
            .map(|a| a.port())
            .unwrap();
        let shared = SharedControl::new(port, 10_000_000);
        shared.running.store(true, Ordering::SeqCst);
        let (_nal_tx, nal_rx) = mpsc::channel::<NALUnit>(4);
        let (_audio_tx, audio_rx) = mpsc::channel(4);
        let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
        let control = shared.clone();
        let transport = tokio::spawn(async move {
            transport::start_sender(nal_rx, audio_rx, port, control, supervisor_tx).await
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        let mut ipad = Ipad::new(SocketAddr::from(([127, 0, 0, 1], port)), Instant::now());
        ipad.connect(0x21).await;
        let address = ipad.address();
        let sent_before = ipad.sent_before;
        settle("the answer", || probe::sent_to(address) > sent_before).await;
        let mut bytes = [0; 2048];
        let len = loop {
            if let Ok((len, _)) = ipad.socket.recv_from(&mut bytes) {
                break len;
            }
            tokio::task::yield_now().await;
        };
        let (header, ControlMessage::HelloAck(ack)) =
            eternal_wire::v2::control::parse_control(&bytes[..len]).unwrap()
        else {
            panic!("a HelloAck");
        };
        assert_eq!(header.session_id, 0);
        assert_eq!(
            ack.status,
            eternal_wire::v2::control::HelloStatus::VersionUnsupported
        );
        assert_eq!(ack.accepted_version, 2);
        assert!(!shared.session.lock().is_active());
        assert!(shared.v3.lock().is_none());
        shared.stop();
        tokio::time::timeout(Duration::from_secs(5), transport)
            .await
            .expect("the transport stops")
            .unwrap()
            .unwrap();
    });
}
