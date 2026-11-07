//! A fake iPad that streams input at a host while it connects and leaves
//! over and over, so a host set to Second screen turns the virtual display
//! on and off under the input. It logs every gap in what the host sends and
//! how long each HELLO waits for its answer: a stuck transport loop shows up
//! as both. It also logs its own send errors and late wake-ups, so a stall
//! of its own is not mistaken for the host's.
//!
//! The input is harmless on a real desktop: pointer and Pencil hover moves
//! (no clicks, no ink) and taps of Right Ctrl alone.
//!
//! `input_soak <host:port> <seconds> <log.jsonl>`
//!
//! Keep the log, and stdout if redirected, on a disk nothing else is busy
//! writing: a log write that waits seconds stops the input with it, and the
//! host then rightly ends the session for silence.
use std::io::Write;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eternal_wire::v2::control::{
    encode_control, parse_control, ByeReason, ControlMessage, Hello2, HelloStatus, InputEvent,
    Ping, ReceiverReport, CAP_DECODE_H264, CAP_DECODE_HEVC, FEATURE_SUPPORTS_NACK,
    FEATURE_WANTS_INPUT,
};
use eternal_wire::v2::{classify, Classified};

const KIND_PENCIL: u8 = 1;
const KIND_KEY: u8 = 4;
const KIND_HOVER: u8 = 6;
const PHASE_BEGAN: u8 = 0;
const PHASE_MOVED: u8 = 1;
const PHASE_ENDED: u8 = 2;
const RIGHT_CTRL: u16 = 0xe4;
/// Connected and away times, cycled. The short ones leave while the
/// display is still coming up.
const CYCLE: [(u64, u64); 4] = [(25, 15), (4, 14), (25, 15), (8, 3)];
const GAP_LOGGED: Duration = Duration::from_millis(1500);
const INPUT_EVERY: Duration = Duration::from_millis(8);
/// A wake-up this late is the soak's own stall.
const LATE_WAKE: Duration = Duration::from_millis(100);

struct Log {
    start: Instant,
    file: Mutex<std::fs::File>,
}

impl Log {
    fn note(&self, line: String) {
        let line = format!("{{\"t_ms\":{},{line}}}", self.start.elapsed().as_millis());
        println!("{line}");
        let _ = writeln!(self.file.lock().unwrap(), "{line}");
    }
}

/// What the receiving thread heard.
#[derive(Default)]
struct Heard {
    session: AtomicU32,
    frames: AtomicU64,
    connected: AtomicBool,
    cycle: AtomicU32,
    last: Mutex<Option<Instant>>,
    worst_gap_ms: AtomicU64,
    recv_errors: AtomicU64,
}

fn receive(socket: &UdpSocket, heard: &Heard, log: &Log) {
    let mut buffer = [0u8; 2048];
    loop {
        let (len, _) = match socket.recv_from(&mut buffer) {
            Ok(received) => received,
            Err(error) => {
                if !matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) {
                    heard.recv_errors.fetch_add(1, Ordering::Relaxed);
                }
                continue;
            }
        };
        let bytes = &buffer[..len];
        let now = Instant::now();
        if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(bytes) {
            if ack.status == HelloStatus::Ok
                && ack.client_nonce == heard.cycle.load(Ordering::SeqCst)
                && heard.session.load(Ordering::SeqCst) == 0
            {
                *heard.last.lock().unwrap() = Some(now);
                heard.session.store(ack.session_id, Ordering::SeqCst);
            }
            continue;
        }
        if !heard.connected.load(Ordering::SeqCst) {
            continue;
        }
        if let Classified::Media { .. } = classify(bytes) {
            heard.frames.fetch_add(1, Ordering::Relaxed);
        }
        let last = heard.last.lock().unwrap().replace(now);
        if let Some(gap) = last.map(|last| now - last) {
            heard
                .worst_gap_ms
                .fetch_max(gap.as_millis() as u64, Ordering::Relaxed);
            if gap >= GAP_LOGGED {
                log.note(format!(
                    "\"event\":\"gap\",\"cycle\":{},\"gap_ms\":{}",
                    heard.cycle.load(Ordering::SeqCst),
                    gap.as_millis()
                ));
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let host: SocketAddr = args[1].parse().unwrap();
    let seconds: u64 = args[2].parse().unwrap();
    let start = Instant::now();
    let log = Arc::new(Log {
        start,
        file: Mutex::new(std::fs::File::create(&args[3]).unwrap()),
    });
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let heard = Arc::new(Heard::default());
    {
        let (socket, heard, log) = (socket.clone(), heard.clone(), log.clone());
        std::thread::spawn(move || receive(&socket, &heard, &log));
    }
    let mut msg_seq = 0u32;
    let send_errors = AtomicU64::new(0);
    let send = |msg_seq: &mut u32, session_id: u32, message: &ControlMessage| {
        *msg_seq += 1;
        if socket
            .send_to(&encode_control(session_id, *msg_seq, message), host)
            .is_err()
        {
            send_errors.fetch_add(1, Ordering::Relaxed);
        }
    };
    let mut event_id = 0u32;
    let (mut worst_ack, mut worst_wake, mut cycles) = (Duration::ZERO, Duration::ZERO, 0u32);
    'run: for (on, off) in CYCLE.iter().cycle().copied() {
        if start.elapsed() >= Duration::from_secs(seconds) {
            break;
        }
        cycles += 1;
        heard.cycle.store(cycles, Ordering::SeqCst);
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: cycles,
            listen_port: socket.local_addr().unwrap().port(),
            decoder_caps: CAP_DECODE_H264 | CAP_DECODE_HEVC,
            feature_caps: FEATURE_WANTS_INPUT | FEATURE_SUPPORTS_NACK,
            screen_px_w: 2732,
            screen_px_h: 2048,
            screen_pt_w: 1366,
            screen_pt_h: 1024,
            refresh_hz: 60,
            device_name: "Input soak".into(),
            device_id: 0x50a6,
            preferred_fps: 60,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        heard.session.store(0, Ordering::SeqCst);
        let asked = Instant::now();
        let mut next_hello = asked;
        let session_id = loop {
            if start.elapsed() >= Duration::from_secs(seconds) {
                break 'run;
            }
            let session_id = heard.session.load(Ordering::SeqCst);
            if session_id != 0 {
                break session_id;
            }
            if Instant::now() >= next_hello {
                send(&mut msg_seq, 0, &hello);
                next_hello += Duration::from_millis(500);
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let waited = asked.elapsed();
        worst_ack = worst_ack.max(waited);
        heard.connected.store(true, Ordering::SeqCst);
        log.note(format!(
            "\"event\":\"ack\",\"cycle\":{cycles},\"waited_ms\":{}",
            waited.as_millis()
        ));
        let connected_until = Instant::now() + Duration::from_secs(on);
        let (mut next_report, mut next_ping) = (Instant::now(), Instant::now());
        let mut step = 0u32;
        let mut key_down = false;
        let mut due = Instant::now();
        while Instant::now() < connected_until {
            let late = Instant::now().saturating_duration_since(due);
            if late >= LATE_WAKE {
                worst_wake = worst_wake.max(late);
                log.note(format!(
                    "\"event\":\"late_wake\",\"cycle\":{cycles},\"late_ms\":{}",
                    late.as_millis()
                ));
            }
            due = Instant::now() + INPUT_EVERY;
            step += 1;
            event_id += 1;
            let angle = f64::from(step) / 60.0;
            let mut event = InputEvent {
                input_ver: 1,
                kind: KIND_HOVER,
                phase: PHASE_MOVED,
                buttons: 0,
                event_id,
                x_norm: (32768.0 + 20000.0 * angle.cos()) as u16,
                y_norm: (32768.0 + 20000.0 * angle.sin()) as u16,
                pressure_x1000: 0,
                scroll_dx: 0,
                scroll_dy: 0,
                keycode: 0,
                modifiers: 0,
                client_time_us: start.elapsed().as_micros() as u64,
                tilt_x: 0,
                tilt_y: 0,
            };
            match step % 4 {
                // Pencil hover: native pen frames without contact.
                1 | 3 => {
                    event.input_ver = 2;
                    event.kind = KIND_PENCIL;
                    event.tilt_x = (step % 90) as i16 - 45;
                    event.tilt_y = 30;
                    if step % 400 == 399 {
                        event.phase = PHASE_ENDED; // leave
                    }
                }
                2 if step % 32 == 2 => {
                    event.kind = KIND_KEY;
                    event.keycode = RIGHT_CTRL;
                    event.phase = if key_down { PHASE_ENDED } else { PHASE_BEGAN };
                    key_down = !key_down;
                }
                _ => {}
            }
            send(&mut msg_seq, session_id, &ControlMessage::InputEvent(event));
            let now = Instant::now();
            if now >= next_report {
                next_report += Duration::from_millis(500);
                let complete = heard.frames.load(Ordering::Relaxed) as u32;
                send(
                    &mut msg_seq,
                    session_id,
                    &ControlMessage::ReceiverReport(ReceiverReport {
                        stream_epoch: 1,
                        highest_seq: complete,
                        frames_complete: complete,
                        ..ReceiverReport::default()
                    }),
                );
            }
            if now >= next_ping {
                next_ping += Duration::from_secs(2);
                send(
                    &mut msg_seq,
                    session_id,
                    &ControlMessage::Ping(Ping {
                        t1_us: start.elapsed().as_micros() as u64,
                    }),
                );
            }
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
        }
        if key_down {
            event_id += 1;
            send(
                &mut msg_seq,
                session_id,
                &ControlMessage::InputEvent(InputEvent {
                    input_ver: 1,
                    kind: KIND_KEY,
                    phase: PHASE_ENDED,
                    buttons: 0,
                    event_id,
                    x_norm: 0,
                    y_norm: 0,
                    pressure_x1000: 0,
                    scroll_dx: 0,
                    scroll_dy: 0,
                    keycode: RIGHT_CTRL,
                    modifiers: 0,
                    client_time_us: start.elapsed().as_micros() as u64,
                    tilt_x: 0,
                    tilt_y: 0,
                }),
            );
        }
        for _ in 0..3 {
            send(
                &mut msg_seq,
                session_id,
                &ControlMessage::Bye(ByeReason::UserDisconnect),
            );
        }
        heard.connected.store(false, Ordering::SeqCst);
        *heard.last.lock().unwrap() = None;
        log.note(format!(
            "\"event\":\"bye\",\"cycle\":{cycles},\"frames\":{},\"send_errors\":{},\"recv_errors\":{}",
            heard.frames.swap(0, Ordering::Relaxed),
            send_errors.swap(0, Ordering::Relaxed),
            heard.recv_errors.swap(0, Ordering::Relaxed)
        ));
        std::thread::sleep(Duration::from_secs(off));
    }
    log.note(format!(
        "\"event\":\"summary\",\"cycles\":{cycles},\"worst_gap_ms\":{},\"worst_ack_ms\":{},\"worst_late_wake_ms\":{}",
        heard.worst_gap_ms.load(Ordering::Relaxed),
        worst_ack.as_millis(),
        worst_wake.as_millis()
    ));
}
