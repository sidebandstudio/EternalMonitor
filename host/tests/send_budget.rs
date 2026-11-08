//! The PC sends the first picture, then waits to hear back from the iPad
//! before it streams more.
//!
//! Paused-clock simulations of the real transport loop, with a fake encoder
//! and fake clients on loopback. The tokio clock only moves when every task
//! waits, and nothing waits while a datagram is on the loopback, so a busy
//! machine cannot change the result. Frames stay at 16 fragments or fewer,
//! which go out unpaced: pacing waits on a real-time thread.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use eternal_host::control::SharedControl;
use eternal_host::encoder::NALUnit;
use eternal_host::transport;
use eternal_host::transport::probe;
use eternal_host::transport::session::{EndReason, UNANSWERED_MEDIA_BUDGET};
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, HelloStatus, Ping, ReceiverReport,
    CAP_DECODE_H264, FEATURE_SUPPORTS_NACK,
};
use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The tests share ETERNAL_USB_DIRECT and the transport probe; run them one
/// at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const STEP: Duration = Duration::from_millis(10);
/// 16 fragments: the largest frame that goes out unpaced.
const KEYFRAME_BYTES: usize = 21_000;
const DELTA_BYTES: usize = 8_000;
/// Real time a datagram may take across the loopback before a test fails.
const LOOPBACK_LIMIT: Duration = Duration::from_secs(10);

/// Waits until `done`, without moving the paused clock: a yielding task
/// keeps the runtime busy, so tokio polls the sockets but advances no time.
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
    /// Session, frame number, keyframe.
    Media(u32, u32, bool),
    Ack(HelloStatus, u32),
    Heartbeat,
    Bye,
    Other,
}

/// How a fake client answers its session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answers {
    /// As the app does since 0.3.0: a ping the moment the HELLO_ACK lands,
    /// four more 200 ms apart, then one every 2 s, and a report every
    /// 500 ms.
    AtOnce,
    /// Its first report, this long after the HELLO_ACK.
    After(Duration),
    Never,
}

struct Client {
    socket: std::net::UdpSocket,
    host: SocketAddr,
    answers: Answers,
    nonce: u32,
    msg_seq: u32,
    session: Option<(u32, Duration)>,
    /// What arrived, when, and its size.
    received: Vec<(Duration, Heard, usize)>,
    /// When each packet reached the PC's socket.
    delivered: Vec<Duration>,
    sent_before: u64,
    taken_before: u64,
    next_ping: Option<(Duration, u32)>,
    next_report: Option<Duration>,
    start: Instant,
}

impl Client {
    fn new(host: SocketAddr, answers: Answers, start: Instant) -> Self {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let address = socket.local_addr().unwrap();
        // A port an earlier test used may come round again.
        let (sent_before, taken_before) = (probe::sent_to(address), probe::taken_from(address));
        Self {
            socket,
            host,
            answers,
            nonce: 0,
            msg_seq: 0,
            session: None,
            received: Vec::new(),
            delivered: Vec::new(),
            sent_before,
            taken_before,
            next_ping: None,
            next_report: None,
            start,
        }
    }

    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    /// Sends, and waits until the PC took it.
    async fn send(&mut self, bytes: Vec<u8>) {
        self.socket.send_to(&bytes, self.host).unwrap();
        self.delivered.push(self.start.elapsed());
        let (address, sent) = (
            self.address(),
            self.taken_before + self.delivered.len() as u64,
        );
        settle("a packet to the PC", || probe::taken_from(address) >= sent).await;
    }

    async fn send_message(&mut self, message: &ControlMessage) {
        self.msg_seq += 1;
        let session_id = self.session.map_or(0, |(id, _)| id);
        let bytes = encode_control(session_id, self.msg_seq, message);
        self.send(bytes).await;
    }

    /// The shipped iPad's HELLO2: the generic name, a device id, and
    /// `listen_port` its own.
    async fn hello(&mut self) {
        self.nonce += 1;
        self.session = None;
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: self.nonce,
            listen_port: self.address().port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_SUPPORTS_NACK,
            screen_px_w: 1280,
            screen_px_h: 720,
            screen_pt_w: 1280,
            screen_pt_h: 720,
            refresh_hz: 60,
            device_name: "iPad".into(),
            device_id: 77,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        self.send_message(&hello).await;
    }

    /// The smallest HELLO2 there is: no name and no device id. 38 bytes.
    async fn bare_hello(&mut self) {
        self.nonce += 1;
        let bytes = encode_control(
            0,
            1,
            &ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: self.nonce,
                listen_port: self.address().port(),
                decoder_caps: CAP_DECODE_H264,
                feature_caps: 0,
                screen_px_w: 1280,
                screen_px_h: 720,
                screen_pt_w: 1280,
                screen_pt_h: 720,
                refresh_hz: 60,
                device_name: String::new(),
                device_id: 0,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
        );
        assert_eq!(bytes.len(), 38);
        self.send(bytes).await;
    }

    /// Takes everything the PC has sent so far, as received `now`.
    async fn drain(&mut self, now: Duration) {
        let address = self.address();
        let sent_before = self.sent_before;
        let mut bytes = [0; 2048];
        loop {
            while let Ok((len, _)) = self.socket.recv_from(&mut bytes) {
                let datagram = &bytes[..len];
                self.received.push((now, heard(datagram), len));
            }
            if self.received.len() as u64 >= probe::sent_to(address) - sent_before {
                break;
            }
            tokio::task::yield_now().await;
        }
    }

    /// What the client does every step: take the session from an ACK, then
    /// answer as it does.
    async fn step(&mut self, now: Duration) {
        self.drain(now).await;
        if self.session.is_none() {
            let accepted = self
                .received
                .iter()
                .rev()
                .find_map(|(_, heard, _)| match heard {
                    Heard::Ack(HelloStatus::Ok, id) => Some(*id),
                    _ => None,
                });
            if let Some(id) = accepted {
                self.session = Some((id, now));
                match self.answers {
                    Answers::AtOnce => {
                        self.next_ping = Some((now, 5));
                        self.next_report = Some(now + Duration::from_millis(500));
                    }
                    Answers::After(delay) => self.next_report = Some(now + delay),
                    Answers::Never => {}
                }
            }
        }
        if let Some((at, burst)) = self.next_ping.filter(|(at, _)| now >= *at) {
            self.next_ping = Some(if burst > 1 {
                (at + Duration::from_millis(200), burst - 1)
            } else {
                (at + Duration::from_secs(2), 1)
            });
            self.send_message(&ControlMessage::Ping(Ping { t1_us: 1 }))
                .await;
        }
        if let Some(at) = self.next_report.filter(|at| now >= *at) {
            self.next_report = Some(at + Duration::from_millis(500));
            let frames = self.frames().len() as u32;
            self.send_message(&ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 1,
                highest_seq: frames,
                frames_complete: frames,
                ..ReceiverReport::default()
            }))
            .await;
        }
    }

    /// Each video frame's first fragment: when, its number, keyframe.
    fn frames(&self) -> Vec<(Duration, u32, bool)> {
        let mut seen = std::collections::BTreeSet::new();
        self.received
            .iter()
            .filter_map(|(at, heard, _)| match heard {
                Heard::Media(_, seq, key) if seen.insert(*seq) => Some((*at, *seq, *key)),
                _ => None,
            })
            .collect()
    }

    fn media_bytes(&self) -> usize {
        self.received
            .iter()
            .filter(|(_, heard, _)| matches!(heard, Heard::Media(..)))
            .map(|(_, _, len)| len)
            .sum()
    }

    fn count(&self, kind: fn(&Heard) -> bool) -> usize {
        self.received
            .iter()
            .filter(|(_, heard, _)| kind(heard))
            .count()
    }
}

fn heard(bytes: &[u8]) -> Heard {
    match classify(bytes) {
        Classified::Media { .. } => {
            let header = MediaHeader::decode(bytes).unwrap().0;
            Heard::Media(header.session_id, header.frame_seq, header.is_keyframe)
        }
        Classified::Control(_) => match parse_control(bytes) {
            Ok((_, ControlMessage::HelloAck(ack))) => Heard::Ack(ack.status, ack.session_id),
            Ok((_, ControlMessage::Heartbeat(_))) => Heard::Heartbeat,
            Ok((_, ControlMessage::Bye(_))) => Heard::Bye,
            _ => Heard::Other,
        },
        _ => Heard::Other,
    }
}

/// The real transport loop and a fake encoder at 60 fps that makes a
/// keyframe whenever one is asked for.
struct Sim {
    start: Instant,
    shared: SharedControl,
    host: SocketAddr,
    transport: tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
    encoder: tokio::task::JoinHandle<()>,
    /// When the encoder found an IDR asked for.
    forced: Arc<Mutex<Vec<Duration>>>,
    clients: Vec<Client>,
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
                    let mut data = vec![
                        0xAA;
                        if is_keyframe {
                            KEYFRAME_BYTES
                        } else {
                            DELTA_BYTES
                        }
                    ];
                    data[..5].copy_from_slice(&[0, 0, 0, 1, if is_keyframe { 0x65 } else { 0x41 }]);
                    let nal = NALUnit {
                        data,
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
        Self {
            start,
            shared,
            host: SocketAddr::from(([127, 0, 0, 1], port)),
            transport,
            encoder,
            forced,
            clients: Vec::new(),
        }
    }

    fn add_client(&mut self, answers: Answers) -> usize {
        self.clients
            .push(Client::new(self.host, answers, self.start));
        self.clients.len() - 1
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    async fn run_until(&mut self, at: Duration) {
        while self.start.elapsed() < at {
            let now = self.start.elapsed();
            for client in &mut self.clients {
                client.step(now).await;
            }
            tokio::time::sleep(STEP).await;
            let now = self.start.elapsed();
            for client in &mut self.clients {
                client.drain(now).await;
            }
        }
    }

    fn last_end(&self) -> Option<(u32, EndReason)> {
        self.shared.session.lock().last_end()
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

/// A HELLO2 from an address that never answers gets the first picture, at
/// most [`UNANSWERED_MEDIA_BUDGET`] of media, and nothing more however
/// often it asks again: a new HELLO2 there brings no new budget for a
/// minute. Its session ends at the liveness timeout, as before.
#[test]
fn an_address_that_never_answers_gets_only_the_first_picture() {
    run(async {
        let mut sim = Sim::start().await;
        let silent = sim.add_client(Answers::Never);
        sim.clients[silent].hello().await;
        sim.run_until(secs(2.0)).await;
        // It asks again and again, from the same address.
        for _ in 0..10 {
            sim.clients[silent].hello().await;
            let next = sim.now() + secs(0.1);
            sim.run_until(next).await;
        }
        sim.run_until(secs(8.0)).await;
        let (ended, reason) = sim.last_end().expect("the session ended");
        assert_eq!(reason, EndReason::Liveness, "session {ended}");
        sim.clients[silent].bare_hello().await;
        sim.run_until(secs(12.0)).await;

        let client = &sim.clients[silent];
        let frames = client.frames();
        eprintln!(
            "UNANSWERED_SIM media_bytes={} frames={} first={:?} last={:?} acks={} heartbeats={}",
            client.media_bytes(),
            frames.len(),
            frames.first(),
            frames.last(),
            client.count(|h| matches!(h, Heard::Ack(..))),
            client.count(|h| *h == Heard::Heartbeat),
        );
        assert!(
            client.media_bytes() <= UNANSWERED_MEDIA_BUDGET,
            "{} bytes of media",
            client.media_bytes()
        );
        let (_, first_seq, first_key) = frames[0];
        assert!(
            first_key && first_seq == 1,
            "the first picture: {:?}",
            frames[0]
        );
        let (last_at, _, _) = *frames.last().unwrap();
        assert!(last_at < secs(1.2), "media at {last_at:?}");
        sim.stop().await;
    });
}

/// An iPad that answers as the app does streams exactly as before: one
/// keyframe for its session, and every frame from the first on, with no
/// pause while the PC waits to hear from it.
#[test]
fn an_ipad_that_answers_at_once_streams_as_before() {
    run(async {
        let mut sim = Sim::start().await;
        let ipad = sim.add_client(Answers::AtOnce);
        sim.clients[ipad].hello().await;
        sim.run_until(secs(4.0)).await;

        let client = &sim.clients[ipad];
        let (id, accepted) = client.session.expect("a session");
        let frames = client.frames();
        eprintln!(
            "ANSWERED_SIM accepted={accepted:?} media_bytes={} frames={} first={:?} forced={:?}",
            client.media_bytes(),
            frames.len(),
            frames.first(),
            sim.forced.lock()
        );
        assert_eq!(sim.shared.session.lock().session_id(), Some(id));
        assert_eq!(sim.forced.lock().len(), 1, "one keyframe");
        assert!(frames[0].2 && frames[0].1 == 1);
        assert!(
            frames.windows(2).all(|pair| pair[1].1 == pair[0].1 + 1),
            "frames missing"
        );
        let gap = frames
            .windows(2)
            .map(|pair| pair[1].0 - pair[0].0)
            .max()
            .unwrap();
        assert!(gap <= FRAME_INTERVAL + STEP, "video stopped for {gap:?}");
        assert!(frames.last().unwrap().0 >= secs(3.9));
        assert!(client.media_bytes() > 3 * UNANSWERED_MEDIA_BUDGET);
        sim.stop().await;
    });
}

/// An iPad first heard after its budget ran out gets nothing more until
/// then, and then a keyframe and the stream, numbered on from the last
/// frame it got.
#[test]
fn an_ipad_heard_late_gets_a_keyframe_then_the_stream() {
    run(async {
        let mut sim = Sim::start().await;
        let ipad = sim.add_client(Answers::After(secs(2.0)));
        sim.clients[ipad].hello().await;
        sim.run_until(secs(4.0)).await;

        let client = &sim.clients[ipad];
        let (id, accepted) = client.session.expect("a session");
        let answered = accepted + secs(2.0);
        let frames = client.frames();
        let before: Vec<_> = frames.iter().filter(|(at, _, _)| *at < answered).collect();
        let after: Vec<_> = frames.iter().filter(|(at, _, _)| *at >= answered).collect();
        let held_bytes: usize = client
            .received
            .iter()
            .filter(|(at, heard, _)| *at < answered && matches!(heard, Heard::Media(..)))
            .map(|(_, _, len)| len)
            .sum();
        eprintln!(
            "LATE_SIM accepted={accepted:?} before={} ({held_bytes} bytes, last {:?}) \
             after_first={:?} forced={:?}",
            before.len(),
            before.last(),
            after.first(),
            sim.forced.lock()
        );
        assert_eq!(sim.shared.session.lock().session_id(), Some(id));
        assert!(held_bytes <= UNANSWERED_MEDIA_BUDGET, "{held_bytes}");
        assert!(before.last().unwrap().0 < secs(1.2), "{:?}", before.last());
        let (resumed, seq, key) = *after[0];
        assert!(key, "the stream starts again with a keyframe");
        assert_eq!(seq, before.last().unwrap().1 + 1, "numbered on");
        assert!(
            resumed - answered <= FRAME_INTERVAL + 2 * STEP,
            "{resumed:?}"
        );
        assert_eq!(
            sim.forced
                .lock()
                .iter()
                .filter(|at| **at >= answered)
                .count(),
            1
        );
        assert!(
            frames.windows(2).all(|pair| pair[1].1 == pair[0].1 + 1),
            "frames missing"
        );
        sim.stop().await;
    });
}
