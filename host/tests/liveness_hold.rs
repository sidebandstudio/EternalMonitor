//! A Wi-Fi uplink that stalls while the downlink keeps working: the iPad
//! decodes on, but the PC hears nothing from it for seconds. The PC used to
//! end the session 3 s into the silence without telling the iPad, which
//! froze until its own check gave up and it reconnected.
//!
//! Paused-clock simulations of the real transport loop, with a fake encoder
//! and fake iPads on loopback whose packets to the PC can be held back and
//! released in order, as a congested Wi-Fi queue does. The tokio clock only
//! moves when every task waits, and nothing waits while a datagram is on
//! the loopback, so a busy machine cannot change the result.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use eternal_host::control::SharedControl;
use eternal_host::encoder::NALUnit;
use eternal_host::input::{
    recorder, CaptureGeometry, Injection, KIND_KEY, PHASE_BEGAN, PHASE_ENDED,
};
use eternal_host::transport;
use eternal_host::transport::probe;
use eternal_host::transport::session::{EndReason, HOLD_LIMIT, LIVENESS_TIMEOUT};
use eternal_wire::v2::control::{
    encode_control, parse_control, ByeReason, ControlMessage, Hello2, HelloStatus, InputEvent,
    Ping, ReceiverReport, CAP_DECODE_H264, FEATURE_SUPPORTS_NACK, FEATURE_WANTS_INPUT,
};
use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The tests share the input recorder and ETERNAL_USB_DIRECT; run them one
/// at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
/// The transport's liveness tick.
const TICK: Duration = Duration::from_millis(250);
const STEP: Duration = Duration::from_millis(10);
/// The 'a' key.
const KEY_A: u16 = 0x04;
/// A stall that outlasts every test: the iPad is gone.
const FOREVER: Duration = Duration::from_secs(3600);
/// Real time a datagram may take across the loopback before a test fails.
const LOOPBACK_LIMIT: Duration = Duration::from_secs(10);

/// Waits until `done`, without moving the paused clock: a yielding task
/// keeps the runtime busy, so tokio polls the sockets but advances no time.
/// macOS delivers loopback datagrams on a kernel thread, a little after the
/// send returns, and later still on a loaded machine.
async fn settle(what: &str, mut done: impl FnMut() -> bool) {
    let limit = std::time::Instant::now() + LOOPBACK_LIMIT;
    while !done() {
        assert!(
            std::time::Instant::now() < limit,
            "{what} still in flight after {LOOPBACK_LIMIT:?}"
        );
        tokio::task::yield_now().await;
    }
}

fn secs(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    Media(u32),
    Heartbeat(u32),
    Bye(u32, ByeReason),
    Ack(HelloStatus, u32),
    Other,
}

/// A fake iPad: it counts the frames it receives, reports every 500 ms and
/// pings every 2 s, as the app does, and stamps pings and input with its own
/// clock. Its packets to the PC go through a queue that a stall holds back
/// and then releases in order.
struct Ipad {
    socket: Arc<std::net::UdpSocket>,
    host: SocketAddr,
    device_id: u64,
    nonce: u32,
    msg_seq: u32,
    session_id: Option<u32>,
    received: Arc<Mutex<Vec<(Duration, Heard)>>>,
    /// Each video frame: its session, number, and when it was captured.
    frames: Vec<(u32, u32, Duration)>,
    /// When each packet reached the PC's socket.
    delivered: Arc<Mutex<Vec<Duration>>>,
    /// What the transport counted for this iPad's address before the test.
    sent_before: u64,
    taken_before: u64,
    uplink: mpsc::UnboundedSender<Vec<u8>>,
    stall: Arc<Mutex<Option<(Duration, Duration)>>>,
    next_report: Duration,
    next_ping: Duration,
    start: Instant,
}

/// The fake iPad's clock at the start: its own epoch, far from the host's.
const IPAD_EPOCH_US: u64 = 987_654_321_000;

impl Ipad {
    async fn new(host: SocketAddr, device_id: u64, start: Instant) -> Self {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = Arc::new(socket);
        let address = socket.local_addr().unwrap();
        // A port an earlier test used may come round again.
        let (sent_before, taken_before) = (probe::sent_to(address), probe::taken_from(address));
        let received = Arc::new(Mutex::new(Vec::new()));
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let stall = Arc::new(Mutex::new(None::<(Duration, Duration)>));
        let (uplink, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
        {
            let (socket, delivered, stall) = (socket.clone(), delivered.clone(), stall.clone());
            tokio::spawn(async move {
                while let Some(bytes) = queue.recv().await {
                    let held = *stall.lock();
                    if let Some((from, to)) = held {
                        let now = start.elapsed();
                        if from <= now && now < to {
                            tokio::time::sleep_until(start + to).await;
                        }
                    }
                    socket.send_to(&bytes, host).unwrap();
                    delivered.lock().push(start.elapsed());
                    // The PC hears it now, not whenever the kernel and the
                    // runtime get round to it.
                    let sent = taken_before + delivered.lock().len() as u64;
                    settle("a packet to the PC", || probe::taken_from(address) >= sent).await;
                }
            });
        }
        Self {
            socket,
            host,
            device_id,
            nonce: 0,
            msg_seq: 0,
            session_id: None,
            received,
            frames: Vec::new(),
            delivered,
            sent_before,
            taken_before,
            uplink,
            stall,
            next_report: Duration::MAX,
            next_ping: Duration::MAX,
            start,
        }
    }

    fn clock_us(&self) -> u64 {
        IPAD_EPOCH_US + self.start.elapsed().as_micros() as u64
    }

    fn encode(&mut self, session_id: u32, message: &ControlMessage) -> Vec<u8> {
        self.msg_seq += 1;
        encode_control(session_id, self.msg_seq, message)
    }

    fn send(&mut self, message: &ControlMessage) {
        let bytes = self.encode(self.session_id.unwrap_or(0), message);
        self.uplink.send(bytes).unwrap();
    }

    /// Sends past the queue, as an uplink that came back.
    async fn send_now(&mut self, session_id: u32, message: &ControlMessage) {
        let bytes = self.encode(session_id, message);
        self.socket.send_to(&bytes, self.host).unwrap();
        self.delivered.lock().push(self.start.elapsed());
        let (address, sent) = (
            self.address(),
            self.taken_before + self.delivered.lock().len() as u64,
        );
        settle("a packet to the PC", || probe::taken_from(address) >= sent).await;
    }

    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    fn hello_message(&mut self) -> ControlMessage {
        self.nonce += 1;
        ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: self.nonce,
            listen_port: self.socket.local_addr().unwrap().port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_WANTS_INPUT | FEATURE_SUPPORTS_NACK,
            screen_px_w: 1280,
            screen_px_h: 720,
            screen_pt_w: 1280,
            screen_pt_h: 720,
            refresh_hz: 60,
            device_name: "Stall test iPad".into(),
            device_id: self.device_id,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        })
    }

    fn hello(&mut self) {
        self.session_id = None;
        let hello = self.hello_message();
        self.send(&hello);
    }

    fn key(&mut self, event_id: u32, phase: u8) {
        self.send(&ControlMessage::InputEvent(InputEvent {
            input_ver: 1,
            kind: KIND_KEY,
            phase,
            buttons: 0,
            event_id,
            x_norm: 0,
            y_norm: 0,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: KEY_A,
            modifiers: 0,
            client_time_us: self.clock_us(),
            tilt_x: 0,
            tilt_y: 0,
        }));
    }

    /// Holds back every packet sent in `from..to`, then releases them in order.
    fn stall(&self, from: Duration, to: Duration) {
        *self.stall.lock() = Some((from, to));
    }

    fn received(&self) -> Vec<(Duration, Heard)> {
        self.received.lock().clone()
    }

    fn last_delivered_before(&self, at: Duration) -> Duration {
        *self
            .delivered
            .lock()
            .iter()
            .filter(|sent| **sent < at)
            .max()
            .unwrap()
    }

    /// What the app does every step: take the session from an ACK, report
    /// the frames it received and ping.
    /// Takes what reached the iPad's socket, as received `now`: a plain
    /// socket read on the iPad's own steps, not a task that tokio's
    /// readiness wakes.
    fn drain(&mut self, now: Duration) {
        let mut bytes = [0; 2048];
        while let Ok((len, _)) = self.socket.recv_from(&mut bytes) {
            let bytes = &bytes[..len];
            if let Classified::Media { .. } = classify(bytes) {
                let (header, _) = MediaHeader::decode(bytes).unwrap();
                let captured = header.capture_ts_us
                    - eternal_host::clock::instant_to_us(self.start.into_std());
                self.frames.push((
                    header.session_id,
                    header.frame_seq,
                    Duration::from_micros(captured),
                ));
            }
            self.received.lock().push((now, heard(bytes)));
        }
    }

    /// Takes everything the PC has sent so far, as received `now`.
    async fn drain_all(&mut self, now: Duration) {
        let address = self.address();
        let sent_before = self.sent_before;
        settle("a packet to the iPad", || {
            self.drain(now);
            self.received.lock().len() as u64 >= probe::sent_to(address) - sent_before
        })
        .await;
    }

    fn step(&mut self, now: Duration) {
        self.drain(now);
        let received = self.received();
        if self.session_id.is_none() {
            let accepted = received.iter().rev().find_map(|(_, heard)| match heard {
                Heard::Ack(HelloStatus::Ok, id) => Some(*id),
                _ => None,
            });
            if let Some(id) = accepted {
                self.session_id = Some(id);
                self.next_report = now + Duration::from_millis(500);
                self.next_ping = now + Duration::from_secs(2);
            }
        }
        let Some(id) = self.session_id else {
            return;
        };
        if now >= self.next_report {
            self.next_report += Duration::from_millis(500);
            let frames = received
                .iter()
                .filter(|(_, heard)| *heard == Heard::Media(id))
                .count() as u32;
            self.send(&ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 1,
                highest_seq: frames,
                frames_complete: frames,
                ..ReceiverReport::default()
            }));
        }
        if now >= self.next_ping {
            self.next_ping += Duration::from_secs(2);
            self.send(&ControlMessage::Ping(Ping {
                t1_us: self.clock_us(),
            }));
        }
    }
}

fn heard(bytes: &[u8]) -> Heard {
    match classify(bytes) {
        Classified::Media { .. } => Heard::Media(MediaHeader::decode(bytes).unwrap().0.session_id),
        Classified::Control(_) => match parse_control(bytes) {
            Ok((header, ControlMessage::Heartbeat(_))) => Heard::Heartbeat(header.session_id),
            Ok((header, ControlMessage::Bye(reason))) => Heard::Bye(header.session_id, reason),
            Ok((_, ControlMessage::HelloAck(ack))) => Heard::Ack(ack.status, ack.session_id),
            _ => Heard::Other,
        },
        _ => Heard::Other,
    }
}

/// The real transport loop, a fake encoder at 60 fps that makes a keyframe
/// whenever one is asked for, and the iPads.
struct Sim {
    start: Instant,
    shared: SharedControl,
    host: SocketAddr,
    transport: tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
    encoder: tokio::task::JoinHandle<()>,
    /// When the encoder found an IDR asked for.
    forced: Arc<Mutex<Vec<Duration>>>,
    injected: Vec<(Duration, Injection)>,
    ipads: Vec<Ipad>,
}

impl Sim {
    async fn start() -> Self {
        let start = Instant::now();
        // Another process can take the free port before the transport binds it.
        let (port, shared, nal_tx, transport) = loop {
            let port = std::net::UdpSocket::bind("127.0.0.1:0")
                .and_then(|s| s.local_addr())
                .map(|a| a.port())
                .unwrap();
            let shared = SharedControl::new(port, 10_000_000);
            shared.pairing.lock().required = false;
            shared.running.store(true, Ordering::SeqCst);
            *shared.capture_geometry.lock() = Some(CaptureGeometry {
                left: 0,
                top: 0,
                width: 1280,
                height: 720,
            });
            let (nal_tx, nal_rx) = mpsc::channel(4);
            let (audio_tx, audio_rx) = mpsc::channel(4);
            let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
            let control = shared.clone();
            let transport = tokio::spawn(async move {
                let _audio = audio_tx;
                transport::start_sender(nal_rx, audio_rx, port, control, supervisor_tx).await
            });
            // The transport binds before its first wait.
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            if !transport.is_finished() {
                break (port, shared, nal_tx, transport);
            }
            let error = transport.await.unwrap().unwrap_err();
            assert!(
                error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::AddrInUse),
                "{error}"
            );
        };
        let forced = Arc::new(Mutex::new(Vec::new()));
        let encoder = {
            let (force, forced) = (shared.force_next_idr.clone(), forced.clone());
            tokio::spawn(async move {
                let mut next = Instant::now();
                for sequence in 0.. {
                    tokio::time::sleep_until(next).await;
                    next += FRAME_INTERVAL;
                    let is_keyframe = force.swap(false, Ordering::SeqCst);
                    if is_keyframe {
                        forced.lock().push(start.elapsed());
                    }
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
        Self {
            start,
            shared,
            host: SocketAddr::from(([127, 0, 0, 1], port)),
            transport,
            encoder,
            forced,
            injected: Vec::new(),
            ipads: Vec::new(),
        }
    }

    async fn add_ipad(&mut self, device_id: u64) -> usize {
        self.ipads
            .push(Ipad::new(self.host, device_id, self.start).await);
        self.ipads.len() - 1
    }

    async fn run_until(&mut self, at: Duration) {
        while self.start.elapsed() < at {
            let now = self.start.elapsed();
            for ipad in &mut self.ipads {
                ipad.step(now);
            }
            tokio::time::sleep(STEP).await;
            let now = self.start.elapsed();
            for ipad in &mut self.ipads {
                ipad.drain_all(now).await;
            }
            self.injected.extend(
                recorder::take()
                    .into_iter()
                    .map(|injection| (now, injection)),
            );
        }
    }

    fn session(&self) -> (Option<u32>, Option<(u32, EndReason)>) {
        let session = self.shared.session.lock();
        (session.session_id(), session.last_end())
    }

    fn keys(&self) -> Vec<(Duration, &'static str)> {
        self.injected
            .iter()
            .filter_map(|(at, injection)| match injection {
                Injection::KeyDown { .. } => Some((*at, "down")),
                Injection::KeyUp { .. } => Some((*at, "up")),
                _ => None,
            })
            .collect()
    }

    fn forced_in(&self, from: Duration, to: Duration) -> usize {
        self.forced
            .lock()
            .iter()
            .filter(|at| from <= **at && **at < to)
            .count()
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
    // Never reach a real iPad: this unreachable endpoint stands in for USB.
    std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(test);
}

impl Ipad {
    /// The longest wait between two video frames of `session_id` captured
    /// in `from..to`, counting from `from` to the first and from the last to
    /// `to`, by when the host captured them: what it sent, whenever the
    /// loopback delivered it. Every frame of the session must have arrived.
    fn video_gap(&self, session_id: u32, from: Duration, to: Duration) -> Duration {
        let mut frames: Vec<(u32, Duration)> = self
            .frames
            .iter()
            .filter(|(id, _, _)| *id == session_id)
            .map(|(_, seq, captured)| (*seq, *captured))
            .collect();
        frames.sort();
        assert!(
            frames.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1),
            "frames missing"
        );
        let mut times: Vec<Duration> = frames
            .iter()
            .map(|(_, captured)| *captured)
            .filter(|captured| from <= *captured && *captured < to)
            .collect();
        times.insert(0, from);
        times.push(to);
        times
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .max()
            .unwrap()
    }

    /// Whether a frame of `session_id` captured after `at` arrived.
    fn video_after(&self, session_id: u32, at: Duration) -> bool {
        self.frames
            .iter()
            .any(|(id, _, captured)| *id == session_id && *captured > at)
    }
}

fn byes(received: &[(Duration, Heard)]) -> Vec<(Duration, u32, ByeReason)> {
    received
        .iter()
        .filter_map(|(at, heard)| match heard {
            Heard::Bye(id, reason) => Some((*at, *id, *reason)),
            _ => None,
        })
        .collect()
}

/// A 5.2 s uplink stall: nothing from the iPad reaches the PC, then
/// everything at once, in order. Video and heartbeats never
/// stop and the session is never torn down, so the iPad never reconnects.
/// The key it held is released 3 s after its last packet, as when the
/// session expired. What was typed during the stall arrives seconds late
/// and lands nothing; keys typed once it is heard work.
#[test]
fn a_5_2_s_uplink_stall_keeps_the_stream_and_the_session() {
    run(async {
        let mut sim = Sim::start().await;
        let ipad = sim.add_ipad(77).await;
        sim.ipads[ipad].hello();
        sim.run_until(secs(2.0)).await;
        let (Some(id), None) = sim.session() else {
            panic!("no session: {:?}", sim.session())
        };
        sim.ipads[ipad].key(1, PHASE_BEGAN);
        sim.run_until(secs(4.0)).await;
        let (stall_from, stall_to) = (secs(4.0), secs(9.2));
        sim.ipads[ipad].stall(stall_from, stall_to);
        sim.run_until(secs(5.0)).await;
        // The release, and a key typed after it, wait in the queue with
        // everything else.
        sim.ipads[ipad].key(2, PHASE_ENDED);
        sim.run_until(secs(6.0)).await;
        sim.ipads[ipad].key(3, PHASE_BEGAN);
        sim.run_until(secs(6.1)).await;
        sim.ipads[ipad].key(4, PHASE_ENDED);
        sim.run_until(secs(10.0)).await;
        sim.ipads[ipad].key(5, PHASE_BEGAN);
        sim.run_until(secs(10.1)).await;
        sim.ipads[ipad].key(6, PHASE_ENDED);
        sim.run_until(secs(12.0)).await;

        let last_heard = sim.ipads[ipad].last_delivered_before(stall_from);
        let received = sim.ipads[ipad].received();
        eprintln!(
            "STALL_SIM last_heard={last_heard:?} keys={:?} forced={:?}",
            sim.keys(),
            sim.forced.lock()
        );
        assert_eq!(
            sim.session(),
            (Some(id), None),
            "no teardown, no new session"
        );
        assert!(byes(&received).is_empty(), "{:?}", byes(&received));
        let gap = sim.ipads[ipad].video_gap(id, secs(1.0), secs(12.0));
        assert!(
            gap < Duration::from_millis(100),
            "video stopped for {gap:?}"
        );
        let heartbeats: Vec<Duration> = received
            .iter()
            .filter(|(_, heard)| *heard == Heard::Heartbeat(id))
            .map(|(at, _)| *at)
            .collect();
        let heartbeat_gap = heartbeats.windows(2).map(|p| p[1] - p[0]).max().unwrap();
        assert!(heartbeat_gap <= secs(1.1), "{heartbeat_gap:?}");
        assert!(*heartbeats.last().unwrap() >= secs(11.0));

        let keys = sim.keys();
        let kinds: Vec<_> = keys.iter().map(|(_, kind)| *kind).collect();
        // Pressed; released by the hold; typed again once heard.
        assert_eq!(kinds, ["down", "up", "down", "up"], "{keys:?}");
        let released = keys[1].0;
        let hold = last_heard + LIVENESS_TIMEOUT;
        assert!(
            hold <= released && released <= hold + TICK + STEP,
            "released at {released:?}, hold at {hold:?}"
        );
        assert!(keys[2].0 >= secs(10.0) && keys[3].0 >= secs(10.1));

        // One IDR for the session, and one when the iPad is heard again.
        assert_eq!(sim.forced_in(Duration::ZERO, secs(1.0)), 1);
        assert_eq!(sim.forced_in(secs(1.0), stall_to), 0);
        assert_eq!(sim.forced_in(stall_to, stall_to + secs(0.5)), 1);
        assert_eq!(sim.forced_in(stall_to + secs(0.5), secs(12.0)), 0);
        sim.stop().await;
    });
}

/// An iPad that is really gone: held from 3 s after its last packet, still
/// streamed to, and told goodbye at the hold limit, after which nothing
/// more is sent. A late packet of that session gets the goodbye again, and
/// a new HELLO gets a session at once.
#[test]
fn a_gone_ipad_is_held_then_told_goodbye_at_the_hold_limit() {
    run(async {
        let mut sim = Sim::start().await;
        let ipad = sim.add_ipad(77).await;
        sim.ipads[ipad].hello();
        sim.run_until(secs(2.0)).await;
        let (Some(id), None) = sim.session() else {
            panic!("no session: {:?}", sim.session())
        };
        sim.ipads[ipad].key(1, PHASE_BEGAN);
        sim.run_until(secs(4.0)).await;
        sim.ipads[ipad].stall(secs(4.0), FOREVER);
        sim.run_until(secs(16.0)).await;

        let last_heard = sim.ipads[ipad].last_delivered_before(secs(4.0));
        let received = sim.ipads[ipad].received();
        let keys = sim.keys();
        eprintln!(
            "GONE_SIM last_heard={last_heard:?} keys={keys:?} byes={:?}",
            byes(&received)
        );
        let kinds: Vec<_> = keys.iter().map(|(_, kind)| *kind).collect();
        assert_eq!(kinds, ["down", "up"]);
        let hold = last_heard + LIVENESS_TIMEOUT;
        assert!(
            hold <= keys[1].0 && keys[1].0 <= hold + TICK + STEP,
            "{keys:?}"
        );

        let goodbyes = byes(&received);
        assert_eq!(goodbyes.len(), 3, "{goodbyes:?}");
        let told = goodbyes[0].0;
        let expiry = last_heard + HOLD_LIMIT;
        assert!(
            expiry <= told && told <= expiry + TICK + STEP,
            "told at {told:?}"
        );
        assert!(goodbyes.iter().all(
            |(at, bye_id, reason)| *at - told < Duration::from_millis(20)
                && (*bye_id, *reason) == (id, ByeReason::Error)
        ));
        let gap = sim.ipads[ipad].video_gap(id, secs(1.0), told);
        assert!(
            gap < Duration::from_millis(100),
            "video stopped for {gap:?} before the goodbye"
        );
        assert!(
            !sim.ipads[ipad].video_after(id, told),
            "video after the goodbye"
        );
        assert_eq!(sim.session(), (None, Some((id, EndReason::Liveness))));

        // The uplink comes back: the queued packets are lost, a late one gets
        // the goodbye once, and a new HELLO a session.
        let report = ControlMessage::ReceiverReport(ReceiverReport::default());
        sim.ipads[ipad].send_now(id, &report).await;
        sim.run_until(secs(16.1)).await;
        sim.ipads[ipad].send_now(id, &report).await;
        sim.run_until(secs(16.2)).await;
        assert_eq!(byes(&sim.ipads[ipad].received()).len(), 4);
        let hello = sim.ipads[ipad].hello_message();
        let before = sim.ipads[ipad].received().len();
        sim.ipads[ipad].send_now(0, &hello).await;
        sim.run_until(secs(16.3)).await;
        let acks: Vec<_> = sim.ipads[ipad].received()[before..]
            .iter()
            .filter_map(|(_, heard)| match heard {
                Heard::Ack(status, new_id) => Some((*status, *new_id)),
                _ => None,
            })
            .collect();
        assert_eq!(acks.len(), 1, "{acks:?}");
        assert_eq!(acks[0].0, HelloStatus::Ok);
        assert_ne!(acks[0].1, id);
        sim.stop().await;
    });
}

/// Another iPad asking for the PC gets Busy while the streaming iPad is
/// heard, and the slot 3 s after its last packet, as when the session
/// expired then. The silent iPad is told goodbye.
#[test]
fn another_ipad_takes_the_slot_of_a_silent_one() {
    run(async {
        let mut sim = Sim::start().await;
        let silent = sim.add_ipad(77).await;
        let other = sim.add_ipad(88).await;
        sim.ipads[silent].hello();
        sim.run_until(secs(2.0)).await;
        let (Some(id), None) = sim.session() else {
            panic!("no session: {:?}", sim.session())
        };
        sim.ipads[silent].stall(secs(4.0), FOREVER);
        sim.run_until(secs(4.0)).await;
        let last_heard = sim.ipads[silent].last_delivered_before(secs(4.0));
        sim.run_until(last_heard + secs(2.5)).await;
        sim.ipads[other].hello();
        sim.run_until(last_heard + secs(3.5)).await;
        sim.ipads[other].hello();
        sim.run_until(last_heard + secs(5.0)).await;

        let acks: Vec<_> = sim.ipads[other]
            .received()
            .into_iter()
            .filter_map(|(at, heard)| match heard {
                Heard::Ack(status, new_id) => Some((at, status, new_id)),
                _ => None,
            })
            .collect();
        eprintln!("TAKEOVER_SIM last_heard={last_heard:?} acks={acks:?}");
        assert_eq!(acks.len(), 2, "{acks:?}");
        assert_eq!(acks[0].1, HelloStatus::Busy);
        let (taken, status, new_id) = acks[1];
        assert_eq!(status, HelloStatus::Ok);
        assert_eq!(
            sim.session(),
            (Some(new_id), Some((id, EndReason::Liveness)))
        );

        let goodbyes = byes(&sim.ipads[silent].received());
        assert_eq!(goodbyes.len(), 3, "{goodbyes:?}");
        assert!(goodbyes
            .iter()
            .all(|(at, bye_id, reason)| *at >= taken - STEP
                && (*bye_id, *reason) == (id, ByeReason::Error)));
        let told = goodbyes[0].0;
        assert!(!sim.ipads[silent].video_after(id, told));
        let gap = sim.ipads[other].video_gap(new_id, taken + secs(0.1), last_heard + secs(5.0));
        assert!(gap < Duration::from_millis(100), "{gap:?}");
        sim.stop().await;
    });
}
