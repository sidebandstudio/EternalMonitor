//! The PC answers an iPad's HELLO2 while it isn't streaming: in the pause
//! before an automatic restart, across a restart, after Stop, and after the
//! storm brake. Drives the REAL supervisor with synthetic capture and
//! libx264, talked to by fake iPads over loopback UDP on ephemeral ports.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eternal_host::control::{SharedControl, SupervisorCommand};
use eternal_host::gpu::GpuInfo;
use eternal_host::pipeline;
use eternal_host::stats::PIPELINE_STATS;
use eternal_host::transport::session::NotStreaming;
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, HelloAck, HelloStatus, CAP_DECODE_H264,
    FEATURE_STANDBY_STATUS,
};

/// Tests share process-global env (ETERNAL_*) and pipeline state; run them
/// one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An iPad on 0.3.5 or later, and one on 0.3.4 or older.
const NEW: u16 = FEATURE_STANDBY_STATUS;
const OLD: u16 = 0;

struct Host {
    shared: SharedControl,
    port: u16,
    commands: mpsc::Sender<SupervisorCommand>,
    supervisor: Option<std::thread::JoinHandle<()>>,
    _env: std::sync::MutexGuard<'static, ()>,
}

impl Host {
    fn start(gpu: GpuInfo, pairing: bool) -> Self {
        Self::start_with_backoff(gpu, pairing, 500)
    }

    /// With the first automatic restart `backoff_ms` after a failure.
    fn start_with_backoff(gpu: GpuInfo, pairing: bool, backoff_ms: u64) -> Self {
        Self::start_with(gpu, backoff_ms, |shared| {
            shared.pairing.lock().required = pairing
        })
    }

    /// With `configure` applied before the supervisor starts.
    fn start_with(gpu: GpuInfo, backoff_ms: u64, configure: impl FnOnce(&SharedControl)) -> Self {
        let env = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        eternal_host::supervisor::TEST_BACKOFF_BASE_MS.store(backoff_ms, Ordering::SeqCst);
        eternal_host::clock::init();
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
        let port = UdpSocket::bind("0.0.0.0:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let shared = SharedControl::new(port, pipeline::DEFAULT_BITRATE_BPS);
        configure(&shared);
        let (commands, command_rx) = mpsc::channel();
        let supervisor = {
            let (shared, commands) = (shared.clone(), commands.clone());
            std::thread::spawn(move || {
                eternal_host::supervisor::run(port, shared, gpu, commands, command_rx);
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

    fn streaming(&self) -> bool {
        PIPELINE_STATS.lock().pipeline_running
    }

    /// Waits for the supervisor to stop the stream after Stop.
    fn wait_until_stopped(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.shared.session.lock().not_streaming() != Some(NotStreaming::Stopped) {
            assert!(Instant::now() < deadline, "the stream never stopped");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        eternal_host::supervisor::TEST_BACKOFF_BASE_MS.store(500, Ordering::SeqCst);
        #[cfg(debug_assertions)]
        eternal_host::network::set_test_adapters(None);
        self.shared.stop();
        let _ = self.commands.send(SupervisorCommand::Shutdown);
        if let Some(supervisor) = self.supervisor.take() {
            let _ = supervisor.join();
        }
    }
}

/// A GPU whose encoder FFmpeg doesn't have: every generation fails as it
/// starts, as a broken driver does.
fn broken_gpu() -> GpuInfo {
    GpuInfo {
        encoder_name: "no_such_encoder".into(),
        ..GpuInfo::software_fallback()
    }
}

struct FakeIpad {
    socket: UdpSocket,
    host: String,
    device_id: u64,
    feature_caps: u16,
    auth_token: [u8; 16],
    /// The bytes of the last HELLO2 sent and of the last ACK received.
    sizes: std::cell::Cell<(usize, usize)>,
}

impl FakeIpad {
    fn new(host: &Host, device_id: u64, feature_caps: u16) -> Self {
        Self::at(std::net::Ipv4Addr::LOCALHOST, host, device_id, feature_caps)
    }

    /// An iPad that reaches the PC at `ip`.
    fn at(ip: std::net::Ipv4Addr, host: &Host, device_id: u64, feature_caps: u16) -> Self {
        let socket = UdpSocket::bind((ip, 0)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let _ = socket2::SockRef::from(&socket).set_recv_buffer_size(4 * 1024 * 1024);
        Self {
            socket,
            host: format!("{ip}:{}", host.port),
            device_id,
            feature_caps,
            auth_token: [0; 16],
            sizes: Default::default(),
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
            // A long name, so a 0.3.4 iPad's HELLO2 is larger than an
            // answer carrying any PC name a test machine has.
            device_name: "standby fake iPad named like a real one".to_string(),
            device_id: self.device_id,
            preferred_fps: 0,
            auth_token: self.auth_token,
            pairing_code: 0,
        });
        self.send(&encode_control(0, nonce, &hello));
    }

    fn send(&self, datagram: &[u8]) {
        self.socket.send_to(datagram, &self.host).unwrap();
        self.sizes.set((datagram.len(), 0));
    }

    /// The next HELLO_ACK for `nonce` within `wait`, skipping video,
    /// heartbeats and older answers.
    fn ack(&self, nonce: u32, wait: Duration) -> Option<HelloAck> {
        let deadline = Instant::now() + wait;
        let mut buffer = [0; 2048];
        while Instant::now() < deadline {
            if let Ok(len) = self.socket.recv(&mut buffer) {
                if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) {
                    if ack.client_nonce == nonce {
                        self.sizes.set((self.sizes.get().0, len));
                        return Some(ack);
                    }
                }
            }
        }
        None
    }

    /// Asks once, as an iPad's first HELLO2 does. The answer must be prompt.
    fn ask(&self, nonce: u32) -> Option<HelloAck> {
        self.hello(nonce);
        self.ack(nonce, Duration::from_secs(2))
    }

    /// Keeps asking, as a waiting iPad does, until the PC answers at all.
    fn ask_until_answered(&self, nonce: u32, within: Duration) -> Option<HelloAck> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            self.hello(nonce);
            if let Some(ack) = self.ack(nonce, Duration::from_millis(500)) {
                return Some(ack);
            }
        }
        None
    }

    /// Keeps asking, as an iPad does, until the PC streams to it.
    fn connect(&self, nonce: u32) -> HelloAck {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "never connected");
            self.hello(nonce);
            if let Some(ack) = self.ack(nonce, Duration::from_millis(250)) {
                if ack.status == HelloStatus::Ok {
                    return ack;
                }
            }
        }
    }
}

/// The smallest HELLO2 there is: no name, no device id or token, no
/// feature bits: 38 bytes.
fn minimal_hello(nonce: u32) -> Vec<u8> {
    encode_control(
        0,
        nonce,
        &ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: nonce,
            listen_port: 0,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: 0,
            screen_px_w: 1,
            screen_px_h: 1,
            screen_pt_w: 1,
            screen_pt_h: 1,
            refresh_hz: 60,
            device_name: String::new(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        }),
    )
}

/// While no stream runs, the PC never answers with more bytes than it was
/// sent. A small HELLO2 from an unpaired sender hears nothing; an iPad that
/// asks for standby answers pads its HELLO2 and hears every answer.
#[test]
fn a_stopped_pc_never_answers_with_more_than_it_was_sent() {
    let host = Host::start(GpuInfo::software_fallback(), true);
    host.send(SupervisorCommand::Stop);
    host.wait_until_stopped();

    let small_sender = FakeIpad::new(&host, 1, OLD);
    let small = minimal_hello(1);
    assert_eq!(small.len(), 38);
    small_sender.send(&small);
    assert!(
        small_sender.ack(1, Duration::from_secs(2)).is_none(),
        "an answer larger than a 38-byte HELLO2 went out"
    );

    let unpaired = FakeIpad::new(&host, 2, NEW);
    let ack = unpaired.ask(2).expect("a padded HELLO2 is answered");
    assert_eq!(ack.status, HelloStatus::Unauthorized);
    let (sent, received) = unpaired.sizes.get();
    assert!(received <= sent, "{received} > {sent}");

    let mut paired = FakeIpad::new(&host, 3, NEW);
    paired.auth_token = host.shared.pairing.lock().token();
    let ack = paired.ask(3).expect("a padded HELLO2 is answered");
    assert_eq!(ack.status, HelloStatus::Stopped);
    let (sent, received) = paired.sizes.get();
    assert!(received <= sent, "{received} > {sent}");

    // A paired 0.3.4 iPad: an answer that fits goes out, one that doesn't
    // is today's silence.
    let mut old = FakeIpad::new(&host, 4, OLD);
    old.auth_token = paired.auth_token;
    if let Some(ack) = old.ask(4) {
        assert_eq!(ack.status, HelloStatus::Error);
        let (sent, received) = old.sizes.get();
        assert!(received <= sent, "{received} > {sent}");
    }
}

/// After Stop, a new iPad hears that the PC isn't streaming, an old one is
/// turned down at once instead of timing out, and nothing starts by itself.
/// After Start, the waiting iPad connects.
#[test]
fn a_stopped_pc_says_so_and_streams_again_only_after_start() {
    let host = Host::start(GpuInfo::software_fallback(), false);
    let ipad = FakeIpad::new(&host, 1, NEW);
    assert_eq!(ipad.connect(1).status, HelloStatus::Ok);

    host.send(SupervisorCommand::Stop);
    host.wait_until_stopped();
    let ack = ipad.ask(2).expect("a stopped PC must answer");
    assert_eq!(ack.status, HelloStatus::Stopped);
    assert_eq!(ack.session_id, 0);
    let old = FakeIpad::new(&host, 2, OLD);
    let ack = old.ask(3).expect("a stopped PC must answer an old iPad");
    assert_eq!(ack.status, HelloStatus::Error);

    // Asking again and again never starts the stream: only Start does.
    for nonce in 4..10 {
        let ack = ipad.ask(nonce).expect("still answered");
        assert_eq!(ack.status, HelloStatus::Stopped);
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(!host.streaming(), "a HELLO2 started the stream after Stop");

    host.send(SupervisorCommand::Start);
    let ack = ipad.connect(10);
    assert_ne!(ack.session_id, 0);
}

/// An unpaired iPad learns nothing new from a stopped PC: it is asked for
/// the pairing code, as when it streams. A paired one hears it's stopped.
#[test]
fn a_stopped_pc_still_asks_an_unpaired_ipad_to_pair() {
    let host = Host::start(GpuInfo::software_fallback(), true);
    host.send(SupervisorCommand::Stop);
    host.wait_until_stopped();
    let unpaired = FakeIpad::new(&host, 1, NEW);
    let ack = unpaired.ask(1).expect("a stopped PC must answer");
    assert_eq!(ack.status, HelloStatus::Unauthorized);
    assert!(ack.reason.is_empty());
    let mut paired = FakeIpad::new(&host, 2, NEW);
    paired.auth_token = host.shared.pairing.lock().token();
    let ack = paired.ask(2).expect("a stopped PC must answer");
    assert_eq!(ack.status, HelloStatus::Stopped);
    assert_eq!(ack.auth_token, [0; 16], "no token while stopped");
}

/// While the supervisor waits out the pause before an automatic restart, a
/// new iPad hears "restarting", and an old iPad is never turned down: it
/// hears nothing and keeps retrying, as before.
#[test]
fn a_pc_restarting_after_a_failure_says_so() {
    let host = Host::start(broken_gpu(), false);
    let ipad = FakeIpad::new(&host, 1, NEW);
    let old = FakeIpad::new(&host, 2, OLD);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut restarting = 0;
    let mut old_answers = Vec::new();
    for nonce in 1.. {
        assert!(Instant::now() < deadline, "never heard Restarting");
        if let Some(ack) = ipad.ask(nonce) {
            restarting += usize::from(ack.status == HelloStatus::Restarting);
        }
        old.hello(nonce);
        if let Some(ack) = old.ack(nonce, Duration::from_millis(50)) {
            old_answers.push(ack.status);
        }
        if restarting >= 3 {
            break;
        }
    }
    assert!(
        old_answers.iter().all(|status| *status == HelloStatus::Ok),
        "an old iPad must not be turned down mid-restart: {old_answers:?}"
    );
}

/// Every HELLO2 sent across a Restart is answered: the port stays bound
/// while it changes hands, so none falls into the gap.
#[test]
fn no_hello_is_lost_across_a_restart() {
    let host = Host::start(GpuInfo::software_fallback(), false);
    let ipad = FakeIpad::new(&host, 1, NEW);
    assert_eq!(ipad.connect(1).status, HelloStatus::Ok);

    const SENT: u32 = 300;
    let mut answers = BTreeMap::new();
    let mut buffer = [0; 2048];
    let mut collect = |ipad: &FakeIpad, answers: &mut BTreeMap<u32, HelloStatus>| {
        while let Ok(len) = ipad.socket.recv(&mut buffer) {
            if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) {
                answers.insert(ack.client_nonce, ack.status);
            }
        }
    };
    for nonce in 100..100 + SENT {
        if nonce == 150 {
            host.send(SupervisorCommand::Restart);
        }
        ipad.hello(nonce);
        std::thread::sleep(Duration::from_millis(5));
        if nonce % 10 == 0 {
            collect(&ipad, &mut answers);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while answers.len() < SENT as usize && Instant::now() < deadline {
        collect(&ipad, &mut answers);
    }
    let missing: Vec<_> = (100..100 + SENT)
        .filter(|nonce| !answers.contains_key(nonce))
        .collect();
    assert!(missing.is_empty(), "unanswered HELLO2 nonces: {missing:?}");
    assert!(
        answers
            .values()
            .all(|status| matches!(status, HelloStatus::Ok | HelloStatus::Restarting)),
        "{answers:?}"
    );
}

/// After repeated failures the storm brake stops the stream until someone
/// clicks Start. A new iPad hears why, an old one is turned down, and the
/// stream doesn't start by itself.
#[test]
fn the_storm_brake_says_why_and_waits_for_start() {
    let host = Host::start_with_backoff(broken_gpu(), false, 1);
    let ipad = FakeIpad::new(&host, 1, NEW);
    let deadline = Instant::now() + Duration::from_secs(30);
    let failed = (1..)
        .find_map(|nonce| {
            assert!(Instant::now() < deadline, "never heard Failed");
            ipad.ask(nonce)
                .filter(|ack| ack.status == HelloStatus::Failed)
        })
        .unwrap();
    assert_eq!(failed.reason, "encoder");
    assert_eq!(failed.session_id, 0);
    let (sent, received) = ipad.sizes.get();
    assert!(received <= sent, "{received} > {sent}");
    let old = FakeIpad::new(&host, 2, OLD);
    assert_eq!(
        old.ask(1)
            .expect("a failed PC must answer an old iPad")
            .status,
        HelloStatus::Error
    );
    std::thread::sleep(Duration::from_secs(1));
    assert!(!host.streaming());
    let ack = ipad.ask(1000).expect("still answered");
    assert_eq!(ack.status, HelloStatus::Failed);
}

/// A stopped PC listens where iPads can reach it, as the stream does: on
/// this PC only while the chosen network is down, on the network's address
/// once it is back, on this PC again when it goes, and on every network
/// once Listen on says so. It never starts the stream to do so.
#[cfg(debug_assertions)]
#[test]
fn a_stopped_pc_follows_the_chosen_network() {
    use eternal_host::network::{default_route, set_test_adapters, Adapter, Kind, ListenOn};
    use std::net::IpAddr;
    // Binding the network's address needs a real one.
    let Some(IpAddr::V4(ip)) = default_route().filter(|ip| !ip.is_loopback()) else {
        eprintln!("SKIP: no IPv4 default route to stand in for the chosen network");
        return;
    };
    let wifi = move |up| {
        vec![Adapter {
            name: "Wi-Fi".into(),
            kind: Kind::Wifi,
            up,
            gateway: true,
            ipv4: vec![ip],
        }]
    };
    let host = Host::start_with(GpuInfo::software_fallback(), 500, |shared| {
        shared.pairing.lock().required = false;
        *shared.listen_on.lock() = ListenOn::Adapter("Wi-Fi".into());
        set_test_adapters(Some(wifi(false)));
    });
    host.send(SupervisorCommand::Stop);
    host.wait_until_stopped();
    let here = FakeIpad::new(&host, 1, NEW);
    let lan = FakeIpad::at(ip, &host, 2, NEW);
    let ack = here
        .ask(1)
        .expect("answered on this PC while the network is down");
    assert_eq!(ack.status, HelloStatus::Stopped);

    set_test_adapters(Some(wifi(true)));
    let ack = lan
        .ask_until_answered(2, Duration::from_secs(20))
        .expect("never answered on the network's address once it came back");
    assert_eq!(ack.status, HelloStatus::Stopped);

    set_test_adapters(Some(wifi(false)));
    let ack = here
        .ask_until_answered(3, Duration::from_secs(20))
        .expect("never answered on this PC once the network went");
    assert_eq!(ack.status, HelloStatus::Stopped);

    // Listen on goes back to every network while stopped: the network's
    // address answers again, though the adapter is still down.
    *host.shared.listen_on.lock() = ListenOn::All;
    let ack = lan
        .ask_until_answered(4, Duration::from_secs(20))
        .expect("never answered on every network after Listen on changed to All");
    assert_eq!(ack.status, HelloStatus::Stopped);
    assert!(
        !host.streaming(),
        "following the network started the stream"
    );
    assert_eq!(
        host.shared.session.lock().not_streaming(),
        Some(NotStreaming::Stopped)
    );
}
