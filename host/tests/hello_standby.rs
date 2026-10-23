//! The PC answers an iPad's HELLO2 even while it isn't streaming: after
//! Stop, while a pipeline restarts, and across the hand-over in between.
//! Drives the REAL supervisor with synthetic capture and libx264, talked to
//! by fake iPads over UDP, as in `e2e_synthetic.rs`.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eternal_host::control::{SharedControl, StreamPhase, SupervisorCommand};
use eternal_host::gpu::GpuInfo;
use eternal_host::pipeline;
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, HelloAck, HelloStatus, ReceiverReport,
    CAP_DECODE_H264, FEATURE_STANDBY_STATUS,
};

/// Tests share process-global env (ETERNAL_*); run them one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Host {
    shared: SharedControl,
    port: u16,
    commands: mpsc::Sender<SupervisorCommand>,
    supervisor: Option<std::thread::JoinHandle<()>>,
    _env: std::sync::MutexGuard<'static, ()>,
}

impl Host {
    fn start() -> Self {
        let env = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Never claim a physically connected iPad.
        std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
        std::env::set_var("ETERNAL_CAPTURE", "synthetic");
        std::env::set_var("ETERNAL_SYNTH_SIZE", "640x360");
        for name in [
            "ETERNAL_DROP",
            "ETERNAL_REORDER",
            "ETERNAL_JITTER_MS",
            "ETERNAL_FAULT_ENCODER_AFTER",
        ] {
            std::env::remove_var(name);
        }
        let _ = ffmpeg_next::init();
        let port = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let shared = SharedControl::new(port, pipeline::DEFAULT_BITRATE_BPS);
        shared.pairing.lock().required = false;
        *shared.encoder_override.lock() = Some("libx264".to_string());
        let (commands, command_rx) = mpsc::channel();
        let supervisor = {
            let (shared, commands) = (shared.clone(), commands.clone());
            std::thread::spawn(move || {
                eternal_host::supervisor::run(
                    port,
                    shared,
                    GpuInfo::software_fallback(),
                    commands,
                    command_rx,
                );
            })
        };
        Self {
            shared,
            port,
            commands,
            supervisor: Some(supervisor),
            _env: env,
        }
    }

    fn send(&self, command: SupervisorCommand) {
        self.commands.send(command).unwrap();
    }

    fn wait_for(&self, phase: StreamPhase) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while *self.shared.stream_phase.lock() != phase {
            assert!(Instant::now() < deadline, "never reached {phase:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        std::env::remove_var("ETERNAL_FAULT_ENCODER_AFTER");
        self.shared.stop();
        let _ = self.commands.send(SupervisorCommand::Shutdown);
        if let Some(supervisor) = self.supervisor.take() {
            let _ = supervisor.join();
        }
    }
}

struct FakeIpad {
    socket: UdpSocket,
    host: String,
    device_id: u64,
    feature_caps: u16,
}

impl FakeIpad {
    fn new(host: &Host, device_id: u64, feature_caps: u16) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let _ = socket2::SockRef::from(&socket).set_recv_buffer_size(4 * 1024 * 1024);
        Self {
            socket,
            host: format!("127.0.0.1:{}", host.port),
            device_id,
            feature_caps,
        }
    }

    fn hello(&self, nonce: u32) {
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: nonce,
            listen_port: self.socket.local_addr().unwrap().port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: self.feature_caps,
            screen_px_w: 1280,
            screen_px_h: 720,
            screen_pt_w: 640,
            screen_pt_h: 360,
            refresh_hz: 60,
            device_name: "standby fake iPad".to_string(),
            device_id: self.device_id,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        self.socket
            .send_to(&encode_control(0, nonce, &hello), &self.host)
            .unwrap();
    }

    /// The next HELLO_ACK within `wait`, skipping video and heartbeats.
    fn ack(&self, wait: Duration) -> Option<HelloAck> {
        let deadline = Instant::now() + wait;
        let mut buffer = [0; 2048];
        while Instant::now() < deadline {
            if let Ok(len) = self.socket.recv(&mut buffer) {
                if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) {
                    return Some(ack);
                }
            }
        }
        None
    }

    /// Asks once, as an iPad's first HELLO2 does, and waits for the answer.
    fn ask(&self, nonce: u32) -> Option<HelloAck> {
        self.hello(nonce);
        self.ack(Duration::from_secs(5))
    }

    /// Keeps asking, as an iPad retrying, until one is answered.
    fn connect(&self, nonce: u32) -> HelloAck {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "no answer within 20 s");
            self.hello(nonce);
            if let Some(ack) = self.ack(Duration::from_millis(250)) {
                return ack;
            }
        }
    }
}

/// After Stop, a new iPad hears that the PC isn't streaming and a 0.3.1
/// iPad is turned away at once instead of timing out. After Start, the very
/// first HELLO2 connects.
#[test]
fn a_stopped_pc_answers_and_the_first_hello_after_start_connects() {
    let host = Host::start();
    let ipad = FakeIpad::new(&host, 1, FEATURE_STANDBY_STATUS);
    assert_eq!(ipad.connect(1).status, HelloStatus::Ok);

    host.send(SupervisorCommand::Stop);
    host.wait_for(StreamPhase::Stopped);
    let ack = ipad.ask(2).expect("a stopped PC must answer");
    assert_eq!(ack.status, HelloStatus::Stopped);
    assert_eq!(ack.session_id, 0);
    let old = FakeIpad::new(&host, 2, 0);
    assert_eq!(
        old.ask(3).expect("a stopped PC must answer 0.3.1").status,
        HelloStatus::Error
    );

    host.send(SupervisorCommand::Start);
    host.wait_for(StreamPhase::Running);
    let ack = ipad
        .ask(4)
        .expect("the first HELLO2 after Start is answered");
    assert_eq!(ack.status, HelloStatus::Ok);
    assert_ne!(ack.session_id, 0);
}

/// A HELLO2 sent while the pipeline restarts used to fall into the gap
/// between one generation closing the port and the next binding it.
#[test]
fn no_hello_is_lost_while_the_pipeline_restarts() {
    let host = Host::start();
    let ipad = FakeIpad::new(&host, 1, FEATURE_STANDBY_STATUS);
    assert_eq!(ipad.connect(1).status, HelloStatus::Ok);

    const SENT: u32 = 400;
    let mut answers = BTreeMap::new();
    let mut buffer = [0; 2048];
    for nonce in 100..100 + SENT {
        if nonce == 150 {
            host.send(SupervisorCommand::Restart);
        }
        ipad.hello(nonce);
        let until = Instant::now() + Duration::from_millis(5);
        while Instant::now() < until {
            if let Ok(len) = ipad.socket.recv(&mut buffer) {
                if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) {
                    answers.insert(ack.client_nonce, ack.status);
                }
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while answers.len() < SENT as usize && Instant::now() < deadline {
        if let Some(ack) = ipad.ack(Duration::from_millis(100)) {
            answers.insert(ack.client_nonce, ack.status);
        }
    }
    let missing: Vec<_> = (100..100 + SENT)
        .filter(|nonce| !answers.contains_key(nonce))
        .collect();
    assert!(missing.is_empty(), "unanswered HELLO2 nonces: {missing:?}");
}

/// While the supervisor waits out the pause before an automatic restart, a
/// new iPad hears "restarting" and a 0.3.1 iPad hears nothing, so it keeps
/// retrying as before.
#[test]
fn a_restarting_pc_says_so() {
    let host = Host::start();
    std::env::set_var("ETERNAL_FAULT_ENCODER_AFTER", "45");
    let viewer = FakeIpad::new(&host, 1, FEATURE_STANDBY_STATUS);
    let viewer_ack = viewer.connect(1);
    assert_eq!(viewer_ack.status, HelloStatus::Ok);
    let probe = FakeIpad::new(&host, 2, FEATURE_STANDBY_STATUS);
    let old = FakeIpad::new(&host, 3, 0);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut nonce = 10;
    let mut restarting = 0;
    let mut old_answers = Vec::new();
    let mut seq = 1;
    let mut buffer = [0; 2048];
    while restarting == 0 {
        assert!(Instant::now() < deadline, "never heard Restarting");
        seq += 1;
        let report = ControlMessage::ReceiverReport(ReceiverReport::default());
        let _ = viewer.socket.send_to(
            &encode_control(viewer_ack.session_id, seq, &report),
            &viewer.host,
        );
        while viewer.socket.recv(&mut buffer).is_ok() {}
        if *host.shared.stream_phase.lock() != StreamPhase::Restarting {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        nonce += 1;
        probe.hello(nonce);
        old.hello(nonce);
        if let Some(ack) = probe.ack(Duration::from_millis(50)) {
            restarting += usize::from(ack.status == HelloStatus::Restarting);
        }
        if let Some(ack) = old.ack(Duration::from_millis(10)) {
            old_answers.push(ack.status);
        }
    }
    assert!(
        !old_answers.contains(&HelloStatus::Error),
        "0.3.1 must not be turned away mid-restart: {old_answers:?}"
    );
}
