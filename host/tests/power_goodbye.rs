//! When Windows puts the PC to sleep, shuts it down or signs the user out
//! while an iPad streams, the iPad that understands it hears why, an older
//! one hears nothing as before, and the session's summary is written first.
//! Drives the real transport loop with a fake iPad on loopback and gives it
//! the messages Windows would; nothing here sleeps or shuts down a PC.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eternal_host::control::SharedControl;
use eternal_host::power::{self, Message, SLEEP_WAIT, SUMMARY_WAIT};
use eternal_host::transport;
use eternal_wire::v2::control::{
    encode_control, parse_control, ByeReason, ControlMessage, Hello2, HelloStatus, CAP_DECODE_H264,
    FEATURE_POWER_BYE, FEATURE_SUPPORTS_NACK,
};
use tokio::sync::mpsc;

/// The tests share the environment and the session log; one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Keeps the session log, which a summary written before a shutdown goes
/// to directly, out of the real app data folder.
fn private_app_data() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("em-power-goodbye-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("APPDATA", &dir);
        dir
    })
    .clone()
}

fn free_udp_port() -> u16 {
    // The transport binds every address.
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap()
}

fn hello(nonce: u32, listen_port: u16, feature_caps: u16) -> Vec<u8> {
    encode_control(
        0,
        nonce,
        &ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: nonce,
            listen_port,
            decoder_caps: CAP_DECODE_H264,
            feature_caps,
            screen_px_w: 1280,
            screen_px_h: 720,
            screen_pt_w: 1280,
            screen_pt_h: 720,
            refresh_hz: 60,
            device_name: "Power test iPad".into(),
            device_id: 7,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        }),
    )
}

/// A fake iPad streaming from a real transport on loopback.
struct Rig {
    shared: SharedControl,
    ipad: UdpSocket,
    host: SocketAddr,
    caps: u16,
    nal_tx: Option<mpsc::Sender<eternal_host::encoder::NALUnit>>,
    transport: Option<std::thread::JoinHandle<()>>,
}

impl Rig {
    fn start(caps: u16) -> Self {
        eternal_host::clock::init();
        private_app_data();
        // Never reach a real iPad over USB.
        std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
        let port = free_udp_port();
        let shared = SharedControl::new(port, 10_000_000);
        shared.pairing.lock().required = false;
        shared.running.store(true, Ordering::SeqCst);
        let (nal_tx, nal_rx) = mpsc::channel(4);
        let control = shared.clone();
        let transport = std::thread::spawn(move || {
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
                    1,
                ))
                .unwrap();
        });
        let ipad = UdpSocket::bind("127.0.0.1:0").unwrap();
        ipad.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut rig = Self {
            shared,
            ipad,
            host: SocketAddr::from(([127, 0, 0, 1], port)),
            caps,
            nal_tx: Some(nal_tx),
            transport: Some(transport),
        };
        assert_eq!(rig.connect(1), Some(HelloStatus::Ok), "the iPad streams");
        rig
    }

    /// Sends HELLO2 `nonce` until an answer comes, for at most `patience`.
    fn ask(&self, nonce: u32, patience: Duration) -> Option<HelloStatus> {
        let port = self.ipad.local_addr().unwrap().port();
        let until = Instant::now() + patience;
        let mut bytes = [0; 2048];
        while Instant::now() < until {
            assert!(
                !self.transport.as_ref().unwrap().is_finished(),
                "the transport ended early"
            );
            self.ipad
                .send_to(&hello(nonce, port, self.caps), self.host)
                .unwrap();
            let asked = Instant::now();
            while asked.elapsed() < Duration::from_millis(200) {
                if let Ok(len) = self.ipad.recv(&mut bytes) {
                    if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&bytes[..len]) {
                        if ack.client_nonce == nonce {
                            return Some(ack.status);
                        }
                    }
                }
            }
        }
        None
    }

    fn connect(&mut self, nonce: u32) -> Option<HelloStatus> {
        // The transport binds its socket first.
        self.ask(nonce, Duration::from_secs(30))
    }

    /// The goodbyes that arrive within `window`.
    fn goodbyes(&self, window: Duration) -> Vec<ByeReason> {
        let until = Instant::now() + window;
        let mut bytes = [0; 2048];
        let mut said = Vec::new();
        while Instant::now() < until {
            if let Ok(len) = self.ipad.recv(&mut bytes) {
                if let Ok((_, ControlMessage::Bye(reason))) = parse_control(&bytes[..len]) {
                    said.push(reason);
                }
            }
        }
        said
    }

    /// Gives the host `message` as its window would, and how long that took.
    fn tell(&self, message: Message) -> (isize, Duration) {
        let started = Instant::now();
        let answer = power::handle(&self.shared, message);
        (answer, started.elapsed())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.shared.stop();
        drop(self.nal_tx.take());
        if let Some(transport) = self.transport.take() {
            let _ = transport.join();
        }
    }
}

fn session_log() -> String {
    std::fs::read_to_string(eternal_host::logging::session_log_path()).unwrap_or_default()
}

/// Sleep: the iPad that understands it hears that the PC went to sleep, in
/// the usual burst of three, before the message returns; then the PC answers
/// no HELLO2 until it wakes, and streams again after.
#[test]
fn sleep_tells_the_ipad_and_waking_lets_it_back() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut rig = Rig::start(FEATURE_POWER_BYE | FEATURE_SUPPORTS_NACK);
    let (answer, took) = rig.tell(Message::Suspend);
    assert_eq!(answer, 1, "never asks Windows to wait");
    assert!(took < SLEEP_WAIT + Duration::from_millis(100), "{took:?}");
    assert_eq!(
        rig.goodbyes(Duration::from_millis(500)),
        [ByeReason::HostSleeping; 3]
    );
    assert!(!rig.shared.client_connected());

    assert_eq!(
        rig.ask(2, Duration::from_millis(600)),
        None,
        "no answer while it sleeps"
    );
    assert!(!rig.shared.client_connected());
    assert_eq!(rig.tell(Message::Resume).0, 1);
    assert_eq!(
        rig.connect(3),
        Some(HelloStatus::Ok),
        "streams again after waking"
    );
}

/// An iPad from before these goodbyes gets none, as before: it notices the
/// silence. Nothing waits for a goodbye that isn't coming.
#[test]
fn sleep_says_nothing_to_an_older_ipad() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rig = Rig::start(FEATURE_SUPPORTS_NACK);
    let (answer, took) = rig.tell(Message::Suspend);
    assert_eq!(answer, 1);
    assert!(took < SLEEP_WAIT, "{took:?}");
    assert!(
        !rig.shared.client_connected(),
        "the session ended all the same"
    );
    let heard = rig.goodbyes(Duration::from_millis(500));
    assert!(heard.is_empty(), "{heard:?}");
}

/// Shutdown and sign-out: the iPad hears that the PC is powering off, and
/// the session's summary is in the session log on disk before the message
/// returns, ready for the next launch to upload.
#[test]
fn shutting_down_or_signing_out_says_so_and_saves_the_summary() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for (nonce, message) in [
        (
            11,
            Message::EndSession {
                ending: true,
                logoff: false,
                close_app: false,
            },
        ),
        (
            12,
            Message::EndSession {
                ending: true,
                logoff: true,
                close_app: false,
            },
        ),
        (13, Message::SessionLogoff),
    ] {
        let mut rig = Rig::start(FEATURE_POWER_BYE);
        // Asking is not going: another app may still call it off.
        assert_eq!(rig.tell(Message::QueryEndSession).0, 1);
        assert!(rig.shared.client_connected());
        assert_eq!(
            rig.tell(Message::EndSession {
                ending: false,
                logoff: false,
                close_app: false,
            })
            .0,
            0
        );
        assert!(
            rig.shared.client_connected(),
            "a cancelled shutdown changes nothing"
        );
        // The session to end, under its own nonce.
        assert_eq!(rig.connect(nonce), Some(HelloStatus::Ok));

        let (answer, took) = rig.tell(message);
        assert_eq!(answer, 0, "{message:?}");
        assert!(took < SUMMARY_WAIT + Duration::from_millis(100), "{took:?}");
        // A sign-out's session change follows its end of session: the
        // repeat ends nothing, tells nothing and writes nothing again.
        let (_, repeat_took) = rig.tell(Message::SessionLogoff);
        assert!(repeat_took < Duration::from_millis(100), "{repeat_took:?}");
        let summary = format!(" nonce={nonce:016x} ");
        let summaries = session_log()
            .lines()
            .filter(|line| {
                line.contains("session_summary v=1 role=pc")
                    && line.contains(&summary)
                    && line.contains(" end=shutdown ")
            })
            .count();
        assert_eq!(summaries, 1, "{message:?}: one summary in the log");
        assert_eq!(
            rig.goodbyes(Duration::from_millis(500)),
            [ByeReason::HostPoweringOff; 3],
            "{message:?}"
        );
    }
}

/// Restart Manager closes the app for an installer, as Setup does on an
/// update, while the PC stays up: the stream ends as on a quit, with the
/// goodbye every iPad reads, not as the PC powering off. An update that
/// hands over to its new copy says connect again.
#[test]
fn an_installer_closing_the_app_ends_the_stream_as_a_quit() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for (nonce, handoff, bye) in [
        (21, false, ByeReason::UserDisconnect),
        (22, true, ByeReason::Error),
    ] {
        let mut rig = Rig::start(FEATURE_POWER_BYE);
        assert_eq!(rig.connect(nonce), Some(HelloStatus::Ok));
        if handoff {
            rig.shared
                .handoff
                .claim(eternal_host::handoff::Handoff::Update)
                .unwrap();
        }
        let (answer, took) = rig.tell(Message::EndSession {
            ending: true,
            logoff: false,
            close_app: true,
        });
        assert_eq!(answer, 0);
        assert!(took < SUMMARY_WAIT + Duration::from_millis(100), "{took:?}");
        assert_eq!(rig.goodbyes(Duration::from_millis(500)), [bye; 3]);
        assert!(
            !rig.shared.running.load(Ordering::SeqCst),
            "the stream stopped"
        );
        let summary = format!(" nonce={nonce:016x} ");
        assert!(
            session_log()
                .lines()
                .any(|line| line.contains("session_summary v=1 role=pc") && line.contains(&summary)),
            "no summary in the log"
        );
    }
}

/// The real window, given the message Windows sends before sleep. Windows
/// only: it checks the window gets Windows' messages and answers them.
#[cfg(windows)]
#[test]
fn the_hidden_window_turns_windows_messages_into_goodbyes() {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageW, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, WM_POWERBROADCAST,
    };

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rig = Rig::start(FEATURE_POWER_BYE);
    power::watch(rig.shared.clone());
    let started = Instant::now();
    let hwnd = loop {
        if let Some(hwnd) = power::window() {
            break HWND(hwnd as _);
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the window opens"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let answer = unsafe {
        SendMessageW(
            hwnd,
            WM_POWERBROADCAST,
            WPARAM(PBT_APMSUSPEND as usize),
            LPARAM(0),
        )
    };
    assert_eq!(answer.0, 1);
    assert_eq!(
        rig.goodbyes(Duration::from_millis(500)),
        [ByeReason::HostSleeping; 3]
    );
    let answer = unsafe {
        SendMessageW(
            hwnd,
            WM_POWERBROADCAST,
            WPARAM(PBT_APMRESUMEAUTOMATIC as usize),
            LPARAM(0),
        )
    };
    assert_eq!(answer.0, 1);
    assert_eq!(rig.ask(2, Duration::from_secs(10)), Some(HelloStatus::Ok));
}
