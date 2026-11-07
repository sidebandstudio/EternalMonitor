//! Windows' input calls can wait for seconds while it reconfigures displays.
//! In the field the transport loop sat in one for about 53 s during a
//! virtual display change: its iPad heard nothing, new HELLOs went
//! unanswered, and the stopped stream kept its port from the next one.
//!
//! The real transport loop on loopback, with every injection held where
//! Windows' calls are. The input it takes is a key with no Windows scan
//! code, so nothing reaches the desktop on Windows either.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eternal_host::control::SharedControl;
use eternal_host::input::{hold, CaptureGeometry, KIND_KEY, PHASE_BEGAN};
use eternal_host::transport;
use eternal_host::transport::beat::Watch;
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, HelloStatus, InputEvent, Ping,
    CAP_DECODE_H264, FEATURE_WANTS_INPUT,
};
use tokio::sync::mpsc;

/// A HID usage Windows has no scan code for: the relay injects nothing.
const NO_KEY: u16 = 0x01;

struct Ipad {
    socket: UdpSocket,
    host: SocketAddr,
    device_id: u64,
    msg_seq: u32,
}

impl Ipad {
    fn new(host: SocketAddr, device_id: u64) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        Self {
            socket,
            host,
            device_id,
            msg_seq: 0,
        }
    }

    fn send(&mut self, session_id: u32, message: &ControlMessage) {
        self.msg_seq += 1;
        self.socket
            .send_to(
                &encode_control(session_id, self.msg_seq, message),
                self.host,
            )
            .unwrap();
    }

    fn hello(&mut self) {
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: self.device_id as u32,
            listen_port: self.socket.local_addr().unwrap().port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_WANTS_INPUT,
            screen_px_w: 1280,
            screen_px_h: 720,
            screen_pt_w: 1280,
            screen_pt_h: 720,
            refresh_hz: 60,
            device_name: "Input stall iPad".into(),
            device_id: self.device_id,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        self.send(0, &hello);
    }

    /// Control messages heard until `until`.
    fn listen(&mut self, until: Instant) -> Vec<ControlMessage> {
        let mut heard = Vec::new();
        let mut bytes = [0u8; 2048];
        while Instant::now() < until {
            if let Ok((len, _)) = self.socket.recv_from(&mut bytes) {
                if let Ok((_, message)) = parse_control(&bytes[..len]) {
                    heard.push(message);
                }
            }
        }
        heard
    }

    fn accepted(&mut self) -> u32 {
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no HELLO answer"
            );
            self.hello();
            let heard = self.listen(Instant::now() + Duration::from_millis(500));
            if let Some(id) = heard.iter().find_map(|message| match message {
                ControlMessage::HelloAck(ack) if ack.status == HelloStatus::Ok => {
                    Some(ack.session_id)
                }
                _ => None,
            }) {
                return id;
            }
        }
    }
}

fn now_ms() -> u64 {
    eternal_host::clock::host_now_us() / 1000
}

#[test]
fn input_windows_holds_never_stops_the_transport_loop() {
    eternal_host::clock::init();
    // Never reach a real iPad over USB.
    std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
    let port = UdpSocket::bind("127.0.0.1:0")
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
    let (_audio_tx, audio_rx) = mpsc::channel(4);
    let control = shared.clone();
    let transport = std::thread::spawn(move || {
        let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(transport::start_sender(
                nal_rx,
                audio_rx,
                port,
                control,
                supervisor_tx,
            ))
            .unwrap();
    });
    let host = SocketAddr::from(([127, 0, 0, 1], port));
    let mut ipad = Ipad::new(host, 1);
    let session_id = ipad.accepted();

    // Windows stops taking input, and the iPad presses a key.
    hold::hold();
    ipad.send(
        session_id,
        &ControlMessage::InputEvent(InputEvent {
            input_ver: 1,
            kind: KIND_KEY,
            phase: PHASE_BEGAN,
            buttons: 0,
            event_id: 1,
            x_norm: 0,
            y_norm: 0,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: NO_KEY,
            modifiers: 0,
            client_time_us: 0,
            tilt_x: 0,
            tilt_y: 0,
        }),
    );
    let started = Instant::now();
    while hold::waiting() == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the key reaches Windows"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // While Windows holds it, the iPad still hears its heartbeats, another
    // iPad's HELLO is answered, and the watch sees the loop moving.
    let mut watch = Watch::default();
    let mut verdicts = Vec::new();
    let mut other = Ipad::new(host, 2);
    other.hello();
    let mut heartbeats = 0;
    let mut answered = false;
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        ipad.send(session_id, &ControlMessage::Ping(Ping { t1_us: 1 }));
        let step = Instant::now() + Duration::from_millis(100);
        heartbeats += ipad
            .listen(step)
            .iter()
            .filter(|message| matches!(message, ControlMessage::Heartbeat(_)))
            .count();
        answered |= other
            .listen(Instant::now() + Duration::from_millis(5))
            .iter()
            .any(|message| matches!(message, ControlMessage::HelloAck(_)));
        verdicts.extend(watch.check(shared.transport_beat.read(), now_ms()));
    }
    assert_eq!(hold::waiting(), 1, "Windows still holds the key");
    assert!(
        heartbeats >= 2,
        "heartbeats while Windows held input: {heartbeats}"
    );
    assert!(answered, "the other iPad's HELLO went unanswered");
    assert_eq!(verdicts, []);

    // A stream stopped meanwhile ends at once, and frees its port.
    shared.stop();
    drop(nal_tx);
    let stopping = Instant::now();
    while !transport.is_finished() {
        assert!(
            stopping.elapsed() < Duration::from_secs(3),
            "the stopped loop waits for Windows"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    transport.join().unwrap();
    assert!(UdpSocket::bind(("127.0.0.1", port)).is_ok());
    hold::release();
}
