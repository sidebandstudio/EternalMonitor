//! When the system has no buffer space for a datagram (WSAENOBUFS 10055 on
//! Windows, ENOBUFS elsewhere), the real transport loop backs off, steps the
//! bitrate down once per episode, gives up a frame it can't send rather than
//! wait on it, retries a control reply once, and logs one line per episode
//! plus its end. The sends are refused through the test-hooks seam.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use eternal_host::control::SharedControl;
use eternal_host::encoder::NALUnit;
use eternal_host::transport::congestion::{BACKOFF, EPISODE_QUIET};
use eternal_host::transport::probe::{self, Kind};
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, Nack, Ping, CAP_DECODE_H264,
    FEATURE_SUPPORTS_NACK,
};
use eternal_wire::v2::media::{media_payload_size, MediaHeader};
use parking_lot::Mutex;

static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct Sink;

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOG.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn log_lines(containing: &str) -> Vec<String> {
    String::from_utf8_lossy(&LOG.lock())
        .lines()
        .filter(|line| line.contains(containing))
        .map(|line| line.trim().to_owned())
        .collect()
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap()
}

struct Ipad {
    socket: UdpSocket,
    host: String,
    session_id: u32,
    msg_seq: u32,
    pinged: Instant,
}

impl Ipad {
    fn addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    fn send(&mut self, message: &ControlMessage) {
        self.msg_seq += 1;
        self.socket
            .send_to(
                &encode_control(self.session_id, self.msg_seq, message),
                &self.host,
            )
            .unwrap();
    }

    /// Keeps the session alive while the test waits.
    fn keep_alive(&mut self) {
        if self.pinged.elapsed() >= Duration::from_millis(250) {
            self.pinged = Instant::now();
            self.send(&ControlMessage::Ping(Ping { t1_us: 1 }));
        }
    }

    /// Every datagram for `wait`, with when it came.
    fn receive(&mut self, wait: Duration) -> Vec<(Instant, Vec<u8>)> {
        let mut buffer = [0; 2048];
        let mut received = Vec::new();
        let until = Instant::now() + wait;
        while Instant::now() < until {
            self.keep_alive();
            if let Ok(len) = self.socket.recv(&mut buffer) {
                received.push((Instant::now(), buffer[..len].to_vec()));
            }
        }
        received
    }

    /// Frame `frame_seq`'s fragments among `received`, with when each came.
    fn fragments(received: &[(Instant, Vec<u8>)], frame_seq: u32) -> Vec<(Instant, u16)> {
        received
            .iter()
            .filter_map(|(at, bytes)| {
                let (header, _) = MediaHeader::decode(bytes).ok()?;
                (header.frame_seq == frame_seq && !header.is_retransmit)
                    .then_some((*at, header.frag_index))
            })
            .collect()
    }

    /// Waits for a heartbeat, so the next second has none.
    fn after_heartbeat(&mut self) {
        let mut buffer = [0; 2048];
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            self.keep_alive();
            if let Ok(len) = self.socket.recv(&mut buffer) {
                if let Ok((_, ControlMessage::Heartbeat(_))) = parse_control(&buffer[..len]) {
                    return;
                }
            }
        }
        panic!("no heartbeat");
    }
}

/// A keyframe of exactly `fragments` datagrams.
fn keyframe(shared: &SharedControl, fragments: usize) -> NALUnit {
    let payload = media_payload_size(shared.max_dgram.load(Ordering::SeqCst) as usize).unwrap();
    let mut data = vec![0x41; fragments * payload];
    data[..5].copy_from_slice(&[0, 0, 0, 1, 0x65]);
    NALUnit {
        data,
        sequence: 0,
        timestamp: Instant::now(),
        encode_duration_us: 0,
        is_keyframe: true,
    }
}

#[test]
fn refused_sends_back_off_step_down_once_and_log_one_episode() {
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(|| Sink)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let port = free_udp_port();
    let shared = SharedControl::new(port, 15_000_000);
    shared.pairing.lock().required = false;
    shared.running.store(true, Ordering::SeqCst);
    let (nal_tx, nal_rx) = tokio::sync::mpsc::channel(16);
    let (_audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
    let (supervisor_tx, _supervisor_rx) = std_mpsc::channel();
    let sender_shared = shared.clone();
    let sender = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(eternal_host::transport::start_sender(
                nal_rx,
                audio_rx,
                port,
                sender_shared,
                supervisor_tx,
            ))
            .unwrap();
    });

    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    let mut ipad = Ipad {
        host: format!("127.0.0.1:{port}"),
        session_id: 0,
        msg_seq: 0,
        pinged: Instant::now(),
        socket,
    };
    let hello = ControlMessage::Hello2(Hello2 {
        proto_min: 2,
        proto_max: 2,
        client_nonce: 9,
        listen_port: ipad.addr().port(),
        decoder_caps: CAP_DECODE_H264,
        feature_caps: FEATURE_SUPPORTS_NACK,
        screen_px_w: 640,
        screen_px_h: 360,
        screen_pt_w: 640,
        screen_pt_h: 360,
        refresh_hz: 60,
        device_name: "Congested iPad".into(),
        device_id: 0,
        preferred_fps: 0,
        auth_token: [0; 16],
        pairing_code: 0,
    });
    let mut buffer = [0; 2048];
    let deadline = Instant::now() + Duration::from_secs(10);
    ipad.session_id = loop {
        assert!(Instant::now() < deadline, "no HELLO_ACK");
        ipad.send(&hello);
        if let Ok(len) = ipad.socket.recv(&mut buffer) {
            if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) {
                break ack.session_id;
            }
        }
    };
    let addr = ipad.addr();
    assert_eq!(shared.abr_current_bps.load(Ordering::SeqCst), 15_000_000);

    // Frame 1: fragment 10 is refused once. The frame backs off and sends it
    // again, and the bitrate steps down without waiting for a loss report.
    probe::refuse_sends(addr, Kind::Media, 10, 1);
    nal_tx.blocking_send(keyframe(&shared, 40)).unwrap();
    let received = ipad.receive(Duration::from_millis(300));
    let frame = Ipad::fragments(&received, 1);
    let indexes: Vec<u16> = frame.iter().map(|&(_, index)| index).collect();
    assert_eq!(
        indexes,
        (0..40).collect::<Vec<u16>>(),
        "the refused fragment is sent again, not lost"
    );
    let paused = frame[10].0.duration_since(frame[9].0);
    assert!(
        paused >= BACKOFF - Duration::from_millis(1),
        "it waited {paused:?} before sending again"
    );
    assert_eq!(
        shared.abr_current_bps.load(Ordering::SeqCst),
        12_000_000,
        "one rung down at once"
    );

    // Frame 2, in the same episode: fragment 5 is refused, and so is its
    // retry. The frame gives up its rest, which repair recovers, and the
    // bitrate stays: one step per episode.
    probe::refuse_sends(addr, Kind::Media, 5, 2);
    nal_tx.blocking_send(keyframe(&shared, 40)).unwrap();
    let received = ipad.receive(Duration::from_millis(300));
    let indexes: Vec<u16> = Ipad::fragments(&received, 2)
        .iter()
        .map(|&(_, index)| index)
        .collect();
    assert_eq!(
        indexes,
        (0..5).collect::<Vec<u16>>(),
        "the rest is given up"
    );
    assert_eq!(probe::refusals_left(addr), 0);
    assert_eq!(shared.abr_current_bps.load(Ordering::SeqCst), 12_000_000);
    let epoch = received
        .iter()
        .find_map(|(_, bytes)| MediaHeader::decode(bytes).ok())
        .unwrap()
        .0
        .stream_epoch;
    ipad.send(&ControlMessage::Nack(Nack {
        stream_epoch: epoch,
        frame_seq: 2,
        frag_count: 40,
        missing: (5..40).collect(),
    }));
    let repaired = ipad
        .receive(Duration::from_millis(300))
        .iter()
        .filter(|(_, bytes)| {
            MediaHeader::decode(bytes)
                .is_ok_and(|(header, _)| header.frame_seq == 2 && header.is_retransmit)
        })
        .count();
    assert_eq!(repaired, 35, "repair recovers what was given up");

    // Five frames of 100 fragments while every send is refused: each frame
    // backs off once and gives up, so none holds the loop for more than one
    // back-off.
    probe::refuse_sends(addr, Kind::Media, 0, 1_000_000);
    let pushed = Instant::now();
    for _ in 0..5 {
        nal_tx.blocking_send(keyframe(&shared, 100)).unwrap();
    }
    while 1_000_000 - probe::refusals_left(addr) < 10 {
        assert!(
            pushed.elapsed() < Duration::from_secs(2),
            "refused {} sends",
            1_000_000 - probe::refusals_left(addr)
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let took = pushed.elapsed();
    ipad.receive(Duration::from_millis(100));
    assert_eq!(
        1_000_000 - probe::refusals_left(addr),
        10,
        "two refusals per frame, the first and its retry"
    );
    assert!(
        took < 5 * (BACKOFF + Duration::from_millis(30)),
        "five frames took {took:?}"
    );
    probe::refuse_sends(addr, Kind::Media, 0, 0);
    assert_eq!(shared.abr_current_bps.load(Ordering::SeqCst), 12_000_000);

    // The episode ends after a quiet second, with one summary line.
    let until = Instant::now() + EPISODE_QUIET + Duration::from_secs(2);
    while log_lines("buffer space again").is_empty() && Instant::now() < until {
        ipad.receive(Duration::from_millis(50));
    }
    let started = log_lines("no buffer space");
    assert_eq!(started.len(), 1, "{started:#?}");
    assert!(
        started[0].starts_with("WARN UDP send failed: no buffer space, backing off link=wifi"),
        "{started:#?}"
    );
    let ended = log_lines("buffer space again");
    assert_eq!(ended.len(), 1, "{ended:#?}");
    assert!(
        ended[0]
            .starts_with("INFO UDP sends have buffer space again link=wifi refused=13 took_ms="),
        "{ended:#?}"
    );
    let steps = log_lines("ABR stepping bitrate down");
    assert_eq!(steps.len(), 1, "{steps:#?}");
    assert!(
        steps[0].contains("reason=\"no_buffer_space\""),
        "{steps:#?}"
    );

    // A refused control reply is sent again once. This is a new episode,
    // with its own step down.
    ipad.after_heartbeat();
    // No keep-alive Pong still on its way to take the refusal.
    ipad.pinged = Instant::now();
    ipad.receive(Duration::from_millis(30));
    probe::refuse_sends(addr, Kind::Control, 0, 1);
    ipad.send(&ControlMessage::Ping(Ping { t1_us: 0xC0FFEE }));
    let ponged = ipad
        .receive(Duration::from_millis(200))
        .iter()
        .any(|(_, bytes)| {
            matches!(
                parse_control(bytes),
                Ok((_, ControlMessage::Pong(pong))) if pong.t1_us == 0xC0FFEE
            )
        });
    assert!(ponged, "the reply went on its retry");
    assert_eq!(probe::refusals_left(addr), 0);
    // Stepped on the next liveness tick.
    let until = Instant::now() + Duration::from_secs(1);
    while shared.abr_current_bps.load(Ordering::SeqCst) != 10_000_000 && Instant::now() < until {
        ipad.receive(Duration::from_millis(20));
    }
    assert_eq!(shared.abr_current_bps.load(Ordering::SeqCst), 10_000_000);
    assert_eq!(log_lines("no buffer space").len(), 2);
    assert!(log_lines("Failed to send control reply").is_empty());
    assert!(
        log_lines("UDP send failed link=").is_empty(),
        "no line per send"
    );

    drop(nal_tx);
    shared.stop();
    sender.join().unwrap();
}
