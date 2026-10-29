//! When Stop and update ends a stream, the iPad must hear the host's goodbye
//! however the transport stops, over UDP and over the USB tunnel. Drives the
//! real transport loop with a fake iPad on loopback.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eternal_host::control::SharedControl;
use eternal_host::transport::link::{read_datagram, read_preamble, write_datagram, PeerId};
use eternal_host::transport::session::ConfigSource;
use eternal_host::transport::{self};
use eternal_wire::v2::control::{
    encode_control, parse_control, ByeReason, ControlMessage, Hello2, StreamConfig, CAP_DECODE_H264,
};
use tokio::sync::mpsc;

/// The tests share ETERNAL_USB_DIRECT; run them one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Config;

impl ConfigSource for Config {
    fn stream_config(&self) -> StreamConfig {
        StreamConfig {
            stream_epoch: 1,
            width: 1280,
            height: 720,
            fps: 60,
            codec: 0,
            flags: 0,
            bitrate_bps: 10_000_000,
        }
    }

    fn host_name(&self) -> String {
        "TEST-HOST".into()
    }
}

fn hello() -> ControlMessage {
    ControlMessage::Hello2(Hello2 {
        proto_min: 2,
        proto_max: 2,
        client_nonce: 1,
        listen_port: 0,
        decoder_caps: CAP_DECODE_H264,
        feature_caps: 0,
        screen_px_w: 1280,
        screen_px_h: 720,
        screen_pt_w: 1280,
        screen_pt_h: 720,
        refresh_hz: 60,
        device_name: "Goodbye test iPad".into(),
        device_id: 0,
        preferred_fps: 0,
        auth_token: [0; 16],
        pairing_code: 0,
    })
}

fn is_goodbye(bytes: &[u8]) -> bool {
    matches!(
        parse_control(bytes),
        Ok((_, ControlMessage::Bye(ByeReason::HostShuttingDown)))
    )
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap()
}

/// How the transport stops after Stop and update ended the stream.
enum Stop {
    /// The host is closing: `running` goes false.
    Shutdown,
    /// The encoder stopped first and closed its channel.
    EncoderEof,
}

/// Runs the transport with a UDP iPad, stops the stream for an update, stops
/// the transport the given way and counts the goodbyes the iPad received.
fn udp_goodbyes(stop: Stop) -> usize {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    eternal_host::clock::init();
    // Never reach a real iPad: this unreachable endpoint stands in for USB.
    std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let ipad = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = free_udp_port();
        let shared = SharedControl::new(port, 10_000_000);
        shared.pairing.lock().required = false;
        shared.running.store(true, Ordering::SeqCst);
        let peer = PeerId::udp(ipad.local_addr().unwrap());
        let admitted =
            shared
                .session
                .lock()
                .handle_control(peer, 0, hello(), &Config, Instant::now());
        assert_eq!(admitted.new_target, Some(peer));
        let (nal_tx, nal_rx) = mpsc::channel(4);
        let (_audio_tx, audio_rx) = mpsc::channel(4);
        let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
        let control = shared.clone();
        let transport = tokio::spawn(async move {
            transport::start_sender(nal_rx, audio_rx, port, control, supervisor_tx)
                .await
                .unwrap()
        });
        // Let the loop take its first ticks.
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(shared.session.lock().begin_update(true));
        match stop {
            Stop::Shutdown => shared.stop(),
            Stop::EncoderEof => drop(nal_tx),
        }
        // Only a backstop for a transport that never stops. It stops at its
        // next liveness tick, 250 ms away, but a loaded machine can leave
        // this thread unscheduled for longer than that.
        tokio::time::timeout(Duration::from_secs(30), transport)
            .await
            .expect("the transport stops")
            .unwrap();
        assert!(
            shared.session.lock().take_goodbye().is_none(),
            "the goodbye was not left behind"
        );
        let mut bytes = [0; 2048];
        let mut goodbyes = 0;
        while let Ok(Ok((len, _))) =
            tokio::time::timeout(Duration::from_millis(100), ipad.recv_from(&mut bytes)).await
        {
            goodbyes += usize::from(is_goodbye(&bytes[..len]));
        }
        goodbyes
    })
}

/// One lost datagram must not turn a planned update into a lost connection.
#[test]
fn udp_goodbye_is_a_short_burst() {
    assert_eq!(udp_goodbyes(Stop::Shutdown), 3);
}

/// The encoder can close its channel before the next liveness tick while the
/// host shuts down; the goodbye must still go out.
#[test]
fn encoder_eof_still_says_goodbye() {
    assert!(udp_goodbyes(Stop::EncoderEof) >= 1);
}

/// Over USB the goodbye is queued for a writer task; the transport must wait
/// for it to be written, and not wait long. The tunnel itself belongs to the
/// process-wide USB hub, which keeps it open across pipeline stops, so the
/// one goodbye arrives on a link that stays open.
#[test]
fn usb_goodbye_arrives_before_the_link_closes() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    eternal_host::clock::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        std::env::set_var(
            "ETERNAL_USB_DIRECT",
            listener.local_addr().unwrap().to_string(),
        );
        let port = free_udp_port();
        let shared = SharedControl::new(port, 10_000_000);
        shared.pairing.lock().required = false;
        shared.running.store(true, Ordering::SeqCst);
        let (_nal_tx, nal_rx) = mpsc::channel(4);
        let (_audio_tx, audio_rx) = mpsc::channel(4);
        let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
        let control = shared.clone();
        let transport = tokio::spawn(async move {
            transport::start_sender(nal_rx, audio_rx, port, control, supervisor_tx)
                .await
                .unwrap()
        });
        let (mut ipad, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        read_preamble(&mut ipad).await.unwrap();
        write_datagram(&mut ipad, &encode_control(0, 1, &hello()))
            .await
            .unwrap();
        loop {
            let bytes = tokio::time::timeout(Duration::from_secs(3), read_datagram(&mut ipad))
                .await
                .unwrap()
                .unwrap();
            if matches!(parse_control(&bytes), Ok((_, ControlMessage::HelloAck(_)))) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(shared.session.lock().begin_update(true));
        shared.stop();
        let stopping = Instant::now();
        tokio::time::timeout(Duration::from_secs(2), transport)
            .await
            .expect("the transport stops")
            .unwrap();
        let stopped_in = stopping.elapsed();
        // Everything the transport wrote before it stopped, until the link
        // goes quiet.
        let mut goodbyes = 0;
        loop {
            match tokio::time::timeout(Duration::from_millis(500), read_datagram(&mut ipad)).await {
                Ok(Ok(bytes)) => goodbyes += usize::from(is_goodbye(&bytes)),
                Ok(Err(error)) => panic!("the USB hub closed the tunnel: {error}"),
                Err(_) => break,
            }
        }
        std::env::set_var("ETERNAL_USB_DIRECT", "127.0.0.1:0");
        assert_eq!(goodbyes, 1, "goodbyes written before the transport stopped");
        // One liveness tick (250 ms) plus a short flush, not a long hang.
        assert!(stopped_in < Duration::from_millis(800), "{stopped_in:?}");
    });
}
