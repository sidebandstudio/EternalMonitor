//! A stream restarts while Windows holds the old generation's input. The
//! iPad keeps its session and presses a key again on the new generation.
//! Each generation used to inject on a thread of its own: the new one's
//! press landed at once, and when Windows let the old thread go, its
//! cleanup released that key, which then read as up while the iPad held
//! it. Now the old generation's release lands first.
//!
//! The real transport loop, twice on one port, with the off-Windows input
//! recorder. Only the old generation's stuck injection is held.
#![cfg(not(windows))]

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eternal_host::control::SharedControl;
use eternal_host::input::{hold, recorder, CaptureGeometry, Injection, KIND_KEY, PHASE_BEGAN};
use eternal_host::transport;
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, HelloStatus, InputEvent,
    CAP_DECODE_H264, FEATURE_WANTS_INPUT,
};
use tokio::sync::mpsc;

const KEY_A: u16 = 0x04;
const KEY_B: u16 = 0x05;
/// The scan code Windows gets for A.
const SCAN_A: u16 = 0x1e;

/// One stream generation: the real transport loop on its own thread.
struct Generation {
    nal_tx: mpsc::Sender<eternal_host::encoder::NALUnit>,
    thread: std::thread::JoinHandle<()>,
}

fn start(shared: &SharedControl, port: u16) -> Generation {
    shared.running.store(true, Ordering::SeqCst);
    let (nal_tx, nal_rx) = mpsc::channel(4);
    let control = shared.clone();
    let thread = std::thread::spawn(move || {
        let (_audio_tx, audio_rx) = mpsc::channel(4);
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
    Generation { nal_tx, thread }
}

fn stop(shared: &SharedControl, generation: Generation) {
    shared.stop();
    drop(generation.nal_tx);
    let stopping = Instant::now();
    while !generation.thread.is_finished() {
        assert!(
            stopping.elapsed() < Duration::from_secs(3),
            "the stopped loop waits for Windows"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    generation.thread.join().unwrap();
}

struct Ipad {
    socket: UdpSocket,
    host: SocketAddr,
    msg_seq: u32,
    event_id: u32,
    session_id: u32,
}

impl Ipad {
    fn send(&mut self, message: &ControlMessage) {
        self.msg_seq += 1;
        self.socket
            .send_to(
                &encode_control(self.session_id, self.msg_seq, message),
                self.host,
            )
            .unwrap();
    }

    fn connect(&mut self) {
        let started = Instant::now();
        let mut bytes = [0u8; 2048];
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no HELLO answer"
            );
            self.send(&ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: self.socket.local_addr().unwrap().port(),
                decoder_caps: CAP_DECODE_H264,
                feature_caps: FEATURE_WANTS_INPUT,
                screen_px_w: 1280,
                screen_px_h: 720,
                screen_pt_w: 1280,
                screen_pt_h: 720,
                refresh_hz: 60,
                device_name: "Input restart iPad".into(),
                device_id: 1,
                preferred_fps: 0,
                auth_token: [0; 16],
                pairing_code: 0,
            }));
            let until = Instant::now() + Duration::from_millis(300);
            while Instant::now() < until {
                if let Ok((len, _)) = self.socket.recv_from(&mut bytes) {
                    if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&bytes[..len]) {
                        if ack.status == HelloStatus::Ok {
                            self.session_id = ack.session_id;
                            return;
                        }
                    }
                }
            }
        }
    }

    fn press(&mut self, keycode: u16) {
        self.event_id += 1;
        let event = InputEvent {
            input_ver: 1,
            kind: KIND_KEY,
            phase: PHASE_BEGAN,
            buttons: 0,
            event_id: self.event_id,
            x_norm: 0,
            y_norm: 0,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode,
            modifiers: 0,
            client_time_us: 0,
            tilt_x: 0,
            tilt_y: 0,
        };
        self.send(&ControlMessage::InputEvent(event));
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(5), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether Windows ends up holding A: its last press or release.
fn a_is_down(injected: &[Injection]) -> Option<bool> {
    injected.iter().rev().find_map(|injection| match injection {
        Injection::KeyDown { scan: SCAN_A, .. } => Some(true),
        Injection::KeyUp { scan: SCAN_A, .. } => Some(false),
        _ => None,
    })
}

#[test]
fn a_restart_while_windows_holds_input_never_releases_the_new_generations_keys() {
    eternal_host::clock::init();
    std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
    // Bound first, so its port is never the one the PC takes.
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let port = UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap();
    let shared = SharedControl::new(port, 10_000_000);
    shared.pairing.lock().required = false;
    *shared.capture_geometry.lock() = Some(CaptureGeometry {
        left: 0,
        top: 0,
        width: 1280,
        height: 720,
    });
    recorder::take();
    let old = start(&shared, port);
    let mut ipad = Ipad {
        socket,
        host: SocketAddr::from(([127, 0, 0, 1], port)),
        msg_seq: 0,
        event_id: 0,
        session_id: 0,
    };
    ipad.connect();
    ipad.press(KEY_A);
    wait_until("A goes down", || a_is_down(&recorder::peek()) == Some(true));

    // Windows stops taking the old generation's input, and the stream
    // restarts on the same port.
    hold::hold_next();
    ipad.press(KEY_B);
    wait_until("B reaches Windows", || hold::waiting() == 1);
    stop(&shared, old);
    let new = start(&shared, port);

    // The iPad, still in its session, presses A again on the new stream,
    // and the press is queued while Windows still holds the old one's.
    wait_until("the new loop starts", || {
        shared.transport_beat.read().is_some()
    });
    let queued = shared.input.relays_queued();
    ipad.press(KEY_A);
    wait_until("the new press is queued", || {
        shared.input.relays_queued() > queued
    });
    assert_eq!(hold::waiting(), 1, "Windows still holds the old input");

    // Windows answers again. Once everything queued has landed, A is down.
    hold::release();
    assert!(shared.input.finish(Duration::from_secs(5)));
    let injected = recorder::take();
    assert_eq!(a_is_down(&injected), Some(true), "{injected:?}");

    stop(&shared, new);
}
