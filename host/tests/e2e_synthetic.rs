//! End-to-end pipeline test over protocol v2: the REAL supervisor +
//! capture(synthetic) + encoder(libx264) + UDP transport, talked to by a fake
//! receiver speaking the same wire protocol as the iPad.
//!
//! Proves, on any dev machine or CI runner with FFmpeg:
//! - HELLO2/HELLO_ACK session establishment (nonzero session id, host timing),
//! - media flows as v2 datagrams stamped with that session id,
//! - the first delivered frame is a parameter-set-bearing keyframe,
//! - decoded pictures carry advancing frame counters (real video end to end),
//! - host heartbeats arrive while streaming,
//! - a client KEYFRAME_REQUEST forces an IDR ahead of the natural GOP,
//! - BYE stops the media stream promptly,
//! - shutdown is bounded.

use std::net::UdpSocket;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eternal_host::capture::synthetic::decode_frame_counter_from_luma;
use eternal_host::control::{SharedControl, SupervisorCommand};
use eternal_host::gpu::GpuInfo;
use eternal_host::pipeline;
use eternal_wire::h264::contains_nal_type;
use eternal_wire::reassembly::{AddOutcome, Reassembler};
use eternal_wire::v2::control::{
    encode_control, parse_control, ByeReason, ControlMessage, Hello2, HelloAck, HelloStatus,
    KeyframeReason, KeyframeRequest, ReceiverReport, CAP_DECODE_H264,
};
use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified};

/// Tests share process-global env (ETERNAL_*); run them one at a time.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Surface the host's tracing in test output (best effort, once per process).
fn init_test_tracing() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, priority: i32) -> i32;
        }
        // Match UDPReceiver's userInteractive queue. Capture and encode already
        // use this class; a default-priority fake receiver can miss a repair
        // deadline while those producer threads keep filling its socket.
        assert_eq!(unsafe { pthread_set_qos_class_self_np(0x21, 0) }, 0);
    }
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}

const SYNTH_W: u32 = 640;
const SYNTH_H: u32 = 360;
const WANT_DECODED_FRAMES: usize = 30;
const DEADLINE: Duration = Duration::from_secs(30);

#[test]
fn usb_framed_stream_end_to_end() {
    use eternal_host::stats::PIPELINE_STATS;
    use eternal_host::transport::link::{read_datagram, read_preamble, write_datagram};
    use eternal_wire::v2::control::HOSTCAP_USB;
    use std::sync::atomic::Ordering;

    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    ffmpeg_next::init().unwrap();
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    for name in ["ETERNAL_DROP", "ETERNAL_REORDER", "ETERNAL_JITTER_MS"] {
        std::env::remove_var(name);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Leave the port closed initially to exercise the direct-link retry path.
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    std::env::set_var("ETERNAL_USB_DIRECT", address.to_string());
    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".into());
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_sender = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            GpuInfo::software_fallback(),
            supervisor_sender,
            supervisor_rx,
        )
    });

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
        stream.set_nodelay(true).unwrap();
        socket2::SockRef::from(&stream).set_recv_buffer_size(32 * 1024).unwrap();
        read_preamble(&mut stream).await.unwrap();
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2, proto_max: 2, client_nonce: 1, listen_port: 0,
            decoder_caps: CAP_DECODE_H264, feature_caps: 0,
            screen_px_w: SYNTH_W as u16, screen_px_h: SYNTH_H as u16,
            screen_pt_w: SYNTH_W as u16, screen_pt_h: SYNTH_H as u16,
            refresh_hz: 60, device_name: "USB E2E iPad".into(),
            device_id: 77, preferred_fps: 60, auth_token: [0; 16], pairing_code: 0,
        });
        write_datagram(&mut stream, &encode_control(0, 1, &hello)).await.unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(2), read_datagram(&mut stream)).await.unwrap().unwrap();
        let (_, ControlMessage::HelloAck(ack)) = parse_control(&ack).unwrap() else { panic!("expected USB HELLO_ACK"); };
        assert_eq!(ack.status, HelloStatus::Ok);
        assert_ne!(ack.session_id, 0);
        assert_ne!(ack.host_caps & HOSTCAP_USB, 0);
        assert!(!shared.session.lock().client_supports_nack());

        let mut reassembler = Reassembler::new();
        let mut decoder = H264TestDecoder::new();
        let started = Instant::now();
        let mut last_report = Instant::now();
        let mut msg_seq = 1;
        let mut decoded = 0;
        let mut after_recovery = 0;
        let mut heartbeats = 0;
        let mut resumed = None;
        let mut recovered = false;
        let mut recovery_ms = 0;
        let mut highest_seq = 0;
        let mut epoch = ack.stream_config.stream_epoch;
        while after_recovery < 30 || heartbeats == 0 {
            assert!(started.elapsed() < DEADLINE, "USB timed out: decoded={decoded}, recovered={recovered}, heartbeats={heartbeats}");
            if decoded >= 30 && resumed.is_none() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                resumed = Some(Instant::now());
            }
            if last_report.elapsed() >= Duration::from_millis(300) {
                last_report = Instant::now();
                msg_seq += 1;
                let report = ControlMessage::ReceiverReport(ReceiverReport {
                    stream_epoch: epoch, highest_seq, frames_complete: decoded,
                    decode_fps_x10: 600, ..ReceiverReport::default()
                });
                write_datagram(&mut stream, &encode_control(ack.session_id, msg_seq, &report)).await.unwrap();
            }
            let bytes = tokio::time::timeout(Duration::from_secs(2), read_datagram(&mut stream)).await.unwrap().unwrap();
            match classify(&bytes) {
                Classified::Control(_) => if matches!(parse_control(&bytes), Ok((_, ControlMessage::Heartbeat(_)))) { heartbeats += 1; },
                Classified::Media { .. } => {
                    let (header, payload) = MediaHeader::decode(&bytes).unwrap();
                    assert_eq!(header.session_id, ack.session_id);
                    assert!(!header.is_retransmit);
                    highest_seq = header.frame_seq;
                    epoch = header.stream_epoch;
                    if let AddOutcome::Completed(frame) = reassembler.add_fragment(header.frame_seq,
                        header.frag_index, header.frag_count, header.stream_epoch, payload, Instant::now()) {
                        let age_us = eternal_host::clock::host_now_us().saturating_sub(header.capture_ts_us);
                        if let Some(resumed) = resumed {
                            if !recovered && header.is_keyframe && age_us <= 100_000 {
                                recovered = true;
                                recovery_ms = resumed.elapsed().as_millis();
                                assert!(recovery_ms <= 100, "USB recovery took {recovery_ms} ms");
                            }
                        }
                        let frames = decoder.decode(&frame);
                        for counter in &frames {
                            assert_eq!(*counter, u64::from(header.frame_seq & 0xFF_FFFF));
                        }
                        decoded += frames.len() as u32;
                        if recovered && !frames.is_empty() {
                            assert!(age_us <= 100_000, "USB retained stale video after recovery: {age_us} us");
                            after_recovery += frames.len();
                        }
                    }
                }
                _ => panic!("unexpected USB packet"),
            }
        }
        let dropped = PIPELINE_STATS.lock().usb_frames_dropped;
        assert!(dropped > 0, "a 500 ms read stall must overflow the video queue");
        assert_eq!(PIPELINE_STATS.lock().transport_retransmits, 0);
        eprintln!("USB_E2E decoded={decoded} after_recovery={after_recovery} heartbeats={heartbeats} dropped={dropped} recovery_ms={recovery_ms}");
        drop(stream);
        let disconnected = Instant::now();
        while shared.client_connected() && disconnected.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!shared.client_connected(), "tunnel EOF must release the session");
        assert!(shared.running.load(Ordering::SeqCst));
    })
    }));
    shared.stop();
    let _ = supervisor_tx.send(SupervisorCommand::Shutdown);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("USB host shutdown");
    std::env::remove_var("ETERNAL_USB_DIRECT");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .expect("ephemeral port")
}

struct H264TestDecoder {
    decoder: ffmpeg_next::codec::decoder::Video,
    frame: ffmpeg_next::frame::Video,
}

impl H264TestDecoder {
    fn new() -> Self {
        Self::with_codec(ffmpeg_next::codec::Id::H264)
    }

    fn with_codec(id: ffmpeg_next::codec::Id) -> Self {
        let codec = ffmpeg_next::decoder::find(id).expect("test decoder");
        let context = ffmpeg_next::codec::Context::new_with_codec(codec);
        let decoder = context.decoder().video().expect("open test decoder");
        Self {
            decoder,
            frame: ffmpeg_next::frame::Video::empty(),
        }
    }

    /// Feed one Annex B access unit; returns the counter values of any frames
    /// that came out.
    fn decode(&mut self, annex_b: &[u8]) -> Vec<u64> {
        let packet = ffmpeg_next::Packet::copy(annex_b);
        if self.decoder.send_packet(&packet).is_err() {
            return Vec::new();
        }
        let mut counters = Vec::new();
        while self.decoder.receive_frame(&mut self.frame).is_ok() {
            let stride = self.frame.stride(0);
            let luma = self.frame.data(0);
            counters.push(decode_frame_counter_from_luma(luma, stride, SYNTH_W));
        }
        counters
    }
}

/// The fake iPad: one socket, HELLO2 handshake, media reassembly, reports.
struct FakeReceiver {
    socket: UdpSocket,
    host: String,
    session_id: u32,
    msg_seq: u32,
    last_report: Instant,
    host_caps: u16,
}

impl FakeReceiver {
    fn connect(host_port: u16) -> Self {
        Self::connect_full(host_port, CAP_DECODE_H264, 0)
    }

    #[cfg(not(windows))]
    fn connect_with_caps(host_port: u16, feature_caps: u16) -> Self {
        Self::connect_full(host_port, CAP_DECODE_H264, feature_caps)
    }

    fn connect_full(host_port: u16, decoder_caps: u16, feature_caps: u16) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("receiver socket");
        socket
            .set_read_timeout(Some(Duration::from_millis(150)))
            .expect("read timeout");
        let _ = socket2::SockRef::from(&socket).set_recv_buffer_size(4 * 1024 * 1024);
        let listen_port = socket.local_addr().unwrap().port();
        let host = format!("127.0.0.1:{host_port}");

        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 0xE2E0_0001,
            listen_port,
            decoder_caps,
            feature_caps,
            screen_px_w: 2420,
            screen_px_h: 1668,
            screen_pt_w: 1210,
            screen_pt_h: 834,
            refresh_hz: 120,
            device_name: "E2E fake iPad".to_string(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let hello_bytes = encode_control(0, 1, &hello);

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut buf = [0u8; 2048];
        let mut last_hello = Instant::now() - Duration::from_secs(1);
        let mut datagrams_seen = 0u32;
        let mut media_seen = 0u32;
        let ack: HelloAck = loop {
            assert!(
                Instant::now() < deadline,
                "no HELLO_ACK within 20s (saw {datagrams_seen} datagrams, {media_seen} media)"
            );
            if last_hello.elapsed() >= Duration::from_millis(250) {
                socket.send_to(&hello_bytes, &host).expect("send hello2");
                last_hello = Instant::now();
            }
            let Ok((len, _)) = socket.recv_from(&mut buf) else {
                continue;
            };
            datagrams_seen += 1;
            match classify(&buf[..len]) {
                Classified::Control(_) => {
                    if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buf[..len]) {
                        break ack;
                    }
                }
                Classified::Media { .. } => media_seen += 1,
                _ => {}
            }
        };

        assert_eq!(ack.status, HelloStatus::Ok);
        assert_ne!(ack.session_id, 0, "an OK ack must carry a session id");
        assert_eq!(ack.accepted_version, 2);
        assert_eq!(ack.liveness_timeout_ms, 3000);
        assert!(!ack.host_name.is_empty());

        Self {
            socket,
            host,
            session_id: ack.session_id,
            msg_seq: 1,
            last_report: Instant::now(),
            host_caps: ack.host_caps,
        }
    }

    fn send(&mut self, message: &ControlMessage) {
        self.msg_seq += 1;
        let bytes = encode_control(self.session_id, self.msg_seq, message);
        self.socket
            .send_to(&bytes, &self.host)
            .expect("send control");
    }

    /// Keep the host's liveness window open (reports double as keepalive).
    fn maybe_report(&mut self, highest_seq: u32, frames_complete: u32) {
        if self.last_report.elapsed() >= Duration::from_millis(400) {
            self.last_report = Instant::now();
            self.send(&ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 0,
                highest_seq,
                frames_complete,
                ..Default::default()
            }));
        }
    }
}

#[test]
fn audio_stream_end_to_end() {
    use eternal_wire::v2::audio::AudioHeader;
    use eternal_wire::v2::control::{FEATURE_WANTS_AUDIO, HOSTCAP_AUDIO};
    use std::sync::atomic::Ordering;
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    ffmpeg_next::init().unwrap();
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::set_var("ETERNAL_AUDIO", "synthetic");
    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    for name in [
        "ETERNAL_DROP",
        "ETERNAL_REORDER",
        "ETERNAL_JITTER_MS",
        "ETERNAL_USB_DIRECT",
    ] {
        std::env::remove_var(name);
    }
    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".into());
    shared.max_dgram.store(576, Ordering::SeqCst);
    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_sender = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            GpuInfo::software_fallback(),
            supervisor_sender,
            supervisor_rx,
        )
    });

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut buffer = [0; 2048];
        let assert_video_without_audio = |receiver: &mut FakeReceiver, duration: Duration| {
            let end = Instant::now() + duration;
            let mut buffer = [0; 2048];
            let mut video = 0;
            while Instant::now() < end {
                receiver.maybe_report(0, 0);
                if let Ok((len, _)) = receiver.socket.recv_from(&mut buffer) {
                    match classify(&buffer[..len]) {
                        Classified::Audio { .. } => {
                            panic!("audio was sent without both peers opting in")
                        }
                        Classified::Media { .. } => video += 1,
                        _ => {}
                    }
                }
            }
            assert!(video >= 30, "video stopped while audio was off");
        };

        let mut legacy = FakeReceiver::connect(listen_port);
        assert_ne!(legacy.host_caps & HOSTCAP_AUDIO, 0);
        assert_video_without_audio(&mut legacy, Duration::from_millis(600));
        legacy.send(&ControlMessage::Bye(ByeReason::UserDisconnect));
        drop(legacy);
        std::thread::sleep(Duration::from_millis(100));

        shared.audio_enabled.store(false, Ordering::SeqCst);
        let mut receiver =
            FakeReceiver::connect_full(listen_port, CAP_DECODE_H264, FEATURE_WANTS_AUDIO);
        assert_video_without_audio(&mut receiver, Duration::from_millis(600));
        shared.audio_enabled.store(true, Ordering::SeqCst);
        let codec = ffmpeg_next::decoder::find_by_name("opus").unwrap();
        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec);
        unsafe {
            ffmpeg_next::ffi::av_channel_layout_default(&mut (*context.as_mut_ptr()).ch_layout, 2);
            (*context.as_mut_ptr()).sample_rate = 48_000;
        }
        let mut decoder = context.decoder().audio().unwrap();
        let mut previous: Option<AudioHeader> = None;
        let mut audio_packets = 0;
        let mut quiet_packets = 0;
        let mut pcm = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while audio_packets < 130 {
            assert!(
                Instant::now() < deadline,
                "only {audio_packets} audio packets arrived"
            );
            receiver.maybe_report(0, 0);
            let Ok((len, _)) = receiver.socket.recv_from(&mut buffer) else {
                continue;
            };
            if !matches!(classify(&buffer[..len]), Classified::Audio { .. }) {
                continue;
            }
            assert!(len <= 576);
            let (header, payload) = AudioHeader::decode(&buffer[..len]).unwrap();
            assert_eq!(header.session_id, receiver.session_id);
            if let Some(previous) = previous {
                assert_eq!(header.audio_seq, previous.audio_seq.wrapping_add(1));
                assert!(header.capture_ts_us > previous.capture_ts_us);
                assert_eq!(header.stream_epoch, previous.stream_epoch);
            } else {
                assert!(
                    header.discontinuity,
                    "first audio packet lacks DISCONTINUITY"
                );
            }
            if header.discontinuity {
                decoder.flush();
            }
            decoder
                .send_packet(&ffmpeg_next::Packet::copy(payload))
                .unwrap();
            let mut frame = ffmpeg_next::frame::Audio::empty();
            decoder.receive_frame(&mut frame).unwrap();
            assert_eq!(frame.samples(), 960);
            pcm.extend_from_slice(frame.plane::<f32>(0));
            previous = Some(header);
            audio_packets += 1;
            quiet_packets += usize::from(payload.len() < 10);
        }
        let window = &pcm[4800..9600];
        let mut re = 0.0;
        let mut im = 0.0;
        for (i, &value) in window.iter().enumerate() {
            let phase = std::f64::consts::TAU * 1000.0 * i as f64 / 48000.0;
            re += f64::from(value) * phase.cos();
            im += f64::from(value) * phase.sin();
        }
        let tone_db = 20.0 * (2.0 * re.hypot(im) / window.len() as f64).log10();
        assert!(tone_db > -20.0, "1 kHz tone too quiet: {tone_db} dBFS");
        assert!(
            quiet_packets >= 5,
            "100 ms silence did not produce small Opus packets"
        );
        eprintln!(
            "Audio E2E decoded={audio_packets}, tone={tone_db:.2} dBFS, quiet={quiet_packets}"
        );

        shared.audio_enabled.store(false, Ordering::SeqCst);
        let drain_until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < drain_until {
            let _ = receiver.socket.recv_from(&mut buffer);
        }
        assert_video_without_audio(&mut receiver, Duration::from_millis(600));

        // Force an actual source-open error. It must disable audio for this
        // session, without opening a Windows endpoint or restarting video.
        let generation = eternal_host::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
        std::env::set_var("ETERNAL_AUDIO", "invalid-test-source");
        shared.audio_enabled.store(true, Ordering::SeqCst);
        assert_video_without_audio(&mut receiver, Duration::from_millis(600));
        assert!(eternal_host::stats::PIPELINE_STATS
            .lock()
            .audio
            .error
            .is_some());
        assert_eq!(
            eternal_host::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst),
            generation
        );
        receiver.send(&ControlMessage::Bye(ByeReason::UserDisconnect));
    }));
    let _ = supervisor_tx.send(SupervisorCommand::Shutdown);
    supervisor.join().unwrap();
    std::env::remove_var("ETERNAL_AUDIO");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn synthetic_stream_end_to_end_v2() {
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::remove_var("ETERNAL_DROP");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect(listen_port);

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut datagram = [0u8; 2048];

    let mut first_frame_checked = false;
    let mut decoded_counters: Vec<u64> = Vec::new();
    let mut heartbeats_seen = 0u32;
    let mut keyframe_requested_at: Option<u64> = None;
    let mut forced_keyframe_seen = false;
    let mut highest_seq = 0u32;

    while decoded_counters.len() < WANT_DECODED_FRAMES
        || !forced_keyframe_seen
        || heartbeats_seen == 0
    {
        assert!(
            Instant::now() < deadline,
            "timed out: {} decoded frames, forced_keyframe_seen={forced_keyframe_seen}, \
             heartbeats={heartbeats_seen} (reassembly: {:?})",
            decoded_counters.len(),
            reassembler.counters()
        );

        receiver.maybe_report(highest_seq, decoded_counters.len() as u32);

        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            continue;
        };
        let bytes = &datagram[..len];

        match classify(bytes) {
            Classified::Control(_) => {
                if let Ok((_, ControlMessage::Heartbeat(hb))) = parse_control(bytes) {
                    heartbeats_seen += 1;
                    assert_eq!(hb.stream_config.width, SYNTH_W as u16);
                    assert_eq!(hb.stream_config.height, SYNTH_H as u16);
                }
            }
            Classified::Media { .. } => {
                let (header, payload) = MediaHeader::decode(bytes).expect("valid media datagram");
                assert_eq!(
                    header.session_id, receiver.session_id,
                    "media must be stamped with the negotiated session id"
                );
                highest_seq = highest_seq.max(header.frame_seq);

                let outcome = reassembler.add_fragment(
                    header.frame_seq,
                    header.frag_index,
                    header.frag_count,
                    header.stream_epoch,
                    payload,
                    Instant::now(),
                );
                let AddOutcome::Completed(frame_bytes) = outcome else {
                    continue;
                };

                // v2 media payload is raw Annex B — no FlatBuffer wrapper.
                if !first_frame_checked {
                    first_frame_checked = true;
                    assert!(
                        header.is_keyframe,
                        "first delivered frame must be a keyframe"
                    );
                    assert!(
                        contains_nal_type(&frame_bytes, 7),
                        "startup keyframe must carry an SPS"
                    );
                    assert!(
                        contains_nal_type(&frame_bytes, 8),
                        "startup keyframe must carry a PPS"
                    );
                    assert!(
                        contains_nal_type(&frame_bytes, 5),
                        "startup keyframe must carry an IDR slice"
                    );
                }

                if keyframe_requested_at.is_none() && decoded_counters.len() >= 5 {
                    // Ask for an IDR mid-GOP. x264's natural GOP is 30, so a
                    // keyframe well before seq+25 proves the request worked.
                    keyframe_requested_at = Some(header.frame_seq as u64);
                    receiver.send(&ControlMessage::KeyframeRequest(KeyframeRequest {
                        stream_epoch: header.stream_epoch,
                        last_complete_seq: header.frame_seq,
                        reason: KeyframeReason::GapLoss,
                    }));
                }
                if let Some(at) = keyframe_requested_at {
                    if header.is_keyframe
                        && header.frame_seq as u64 > at
                        && (header.frame_seq as u64) < at + 25
                    {
                        forced_keyframe_seen = true;
                    }
                }

                for counter in decoder.decode(&frame_bytes) {
                    assert_eq!(
                        counter,
                        header.frame_seq as u64 & 0xFF_FFFF,
                        "decoded frame counter must match the wire sequence number"
                    );
                    decoded_counters.push(counter);
                }
            }
            other => panic!("unexpected datagram from host: {other:?}"),
        }
    }

    assert!(
        heartbeats_seen >= 1,
        "host heartbeats must arrive while streaming"
    );
    for pair in decoded_counters.windows(2) {
        assert!(
            pair[1] > pair[0],
            "decoded frame counters must strictly increase: {:?}",
            pair
        );
    }

    // ---- BYE stops the media stream promptly ----
    receiver.send(&ControlMessage::Bye(ByeReason::UserDisconnect));
    receiver.send(&ControlMessage::Bye(ByeReason::UserDisconnect));

    let quiet_deadline = Instant::now() + Duration::from_secs(3);
    let mut last_media = Instant::now();
    loop {
        match receiver.socket.recv_from(&mut datagram) {
            Ok((len, _)) => {
                if matches!(classify(&datagram[..len]), Classified::Media { .. }) {
                    last_media = Instant::now();
                }
            }
            Err(_) => {
                if last_media.elapsed() >= Duration::from_millis(800) {
                    break; // media stream went quiet after BYE
                }
            }
        }
        assert!(
            Instant::now() < quiet_deadline,
            "media kept flowing more than 3s after BYE"
        );
    }

    // ---- Clean shutdown within a bounded window ----
    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");

    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}

/// Repair is negotiated and exercised through the real encoder and transport.
#[test]
fn lossy_stream_recovers_and_adapts() {
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();
    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::set_var("ETERNAL_DROP", "0.05");
    std::env::set_var("ETERNAL_REORDER", "0.02");
    let port = free_udp_port();
    let shared = SharedControl::new(port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".into());
    let (tx, rx) = mpsc::channel();
    let pipeline_shared = shared.clone();
    let pipeline_tx = tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            port,
            pipeline_shared,
            GpuInfo::software_fallback(),
            pipeline_tx,
            rx,
        );
    });
    let mut receiver = FakeReceiver::connect_full(
        port,
        CAP_DECODE_H264,
        eternal_wire::v2::control::FEATURE_SUPPORTS_NACK,
    );
    #[cfg(not(target_os = "macos"))]
    receiver
        .socket
        .set_read_timeout(Some(Duration::from_millis(2)))
        .unwrap();
    #[cfg(target_os = "macos")]
    let repair_poll = {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        // SO_RCVTIMEO can coalesce a 2 ms wait beyond the frame's repair
        // deadline. Match the app's 1 ms repair timer while also waking as
        // soon as a packet arrives. No spin loop or relaxed deadline.
        receiver.socket.set_nonblocking(true).unwrap();
        let fd = unsafe { libc::kqueue() };
        assert!(
            fd >= 0,
            "repair kqueue: {}",
            std::io::Error::last_os_error()
        );
        let queue = unsafe { OwnedFd::from_raw_fd(fd) };
        let changes = [
            libc::kevent {
                ident: receiver.socket.as_raw_fd() as usize,
                filter: libc::EVFILT_READ,
                flags: libc::EV_ADD,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            },
            libc::kevent {
                ident: 1,
                filter: libc::EVFILT_TIMER,
                flags: libc::EV_ADD,
                fflags: libc::NOTE_USECONDS | libc::NOTE_CRITICAL,
                data: 1_000,
                udata: std::ptr::null_mut(),
            },
        ];
        let result = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(
            result,
            0,
            "repair poll: {}",
            std::io::Error::last_os_error()
        );
        queue
    };
    let mut assembler = Reassembler::new();
    assembler.configure_repair(
        true,
        Duration::from_micros(16_667),
        Duration::from_millis(1),
    );
    // Match the app's independent decode queue. Synchronous software decode
    // here can stall socket reads through a repair deadline on a busy runner,
    // even when the retransmit is already waiting in the kernel buffer.
    let (decode_tx, decode_rx) = mpsc::sync_channel::<Vec<u8>>(3);
    let decoding = std::thread::spawn(move || {
        let mut decoder = H264TestDecoder::new();
        let mut decoded = 0usize;
        for frame in decode_rx {
            decoded += decoder.decode(&frame).len();
        }
        decoded
    });
    let mut decode_queue_drops = 0;
    let mut requests = 0u32;
    let mut abr_down = false;
    let started = Instant::now();
    let mut media_trace = Vec::new();
    let mut nack_trace = Vec::new();
    let mut recovery_sequences = Vec::new();
    let mut bytes = [0; 2048];
    while started.elapsed() < Duration::from_secs(15) {
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
            let result = unsafe {
                libc::kevent(
                    repair_poll.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    event.as_mut_ptr(),
                    1,
                    std::ptr::null(),
                )
            };
            if result == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            assert_eq!(
                result,
                1,
                "repair wait: {}",
                std::io::Error::last_os_error()
            );
            assert_eq!(unsafe { event.assume_init() }.flags & libc::EV_ERROR, 0);
        }
        if let Ok((len, _)) = receiver.socket.recv_from(&mut bytes) {
            if let Ok((header, payload)) = MediaHeader::decode(&bytes[..len]) {
                if header.session_id == receiver.session_id {
                    media_trace.push((
                        started.elapsed().as_micros(),
                        header.frame_seq,
                        header.frag_index,
                        header.is_retransmit,
                    ));
                    assembler.add_media(header, payload, Instant::now());
                }
            } else if let Ok((_, ControlMessage::Heartbeat(hb))) = parse_control(&bytes[..len]) {
                abr_down |= hb.stream_config.bitrate_bps < 15_000_000;
            }
        }
        assembler.tick(Instant::now());
        for nack in assembler.take_nacks() {
            nack_trace.push((started.elapsed().as_micros(), nack.clone()));
            receiver.send(&ControlMessage::Nack(nack));
        }
        if let Some((epoch, seq)) = assembler.take_keyframe_request() {
            requests += 1;
            recovery_sequences.push(seq);
            eprintln!(
                "NACK_E2E_RECOVERY elapsed_ms={} last_complete={seq} counters={:?}",
                started.elapsed().as_millis(),
                assembler.counters()
            );
            receiver.send(&ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: epoch,
                last_complete_seq: seq,
                reason: KeyframeReason::GapLoss,
            }));
        }
        for frame in assembler.take_ready() {
            if decode_tx.try_send(frame.payload).is_err() {
                decode_queue_drops += 1;
            }
        }
        if receiver.last_report.elapsed() >= Duration::from_millis(400) {
            receiver.last_report = Instant::now();
            let c = assembler.counters();
            receiver.send(&ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: c.stream_epoch,
                highest_seq: c.highest_seq,
                frames_complete: c.frames_complete as u32,
                frames_dropped: c.frames_dropped as u32,
                frags_received: c.frags_received as u32,
                frags_lost: c.frags_lost as u32,
                frags_repaired: c.frags_repaired as u32,
                nacks_sent: c.nacks_sent as u32,
                assembler_depth: c.assembler_depth,
                jitter_us: c.jitter_us,
                ..Default::default()
            }));
        }
    }
    let counters = assembler.counters();
    let (sent, retransmits) = {
        let stats = eternal_host::stats::PIPELINE_STATS.lock();
        (stats.transport_packets_sent, stats.transport_retransmits)
    };
    std::env::remove_var("ETERNAL_DROP");
    std::env::remove_var("ETERNAL_REORDER");
    shared.stop();
    tx.send(SupervisorCommand::Shutdown).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("bounded shutdown");
    drop(decode_tx);
    let decoded = decoding.join().expect("decoder thread");
    if requests > 1 {
        for retired in recovery_sequences {
            for event in &media_trace {
                if (retired..=retired + 3).contains(&event.1) {
                    eprintln!(
                        "REPAIR_MEDIA us={} seq={} frag={} retransmit={}",
                        event.0, event.1, event.2, event.3
                    );
                }
            }
            for (at, nack) in &nack_trace {
                if (retired..=retired + 3).contains(&nack.frame_seq) {
                    eprintln!("REPAIR_NACK us={at} {nack:?}");
                }
            }
        }
    }
    eprintln!("NACK_E2E decoded={decoded} sent={sent} requests={requests} retransmits={retransmits} decode_queue_drops={decode_queue_drops} counters={counters:?}");
    assert!(
        sent >= 600,
        "fifteen-second stream must make sustained progress"
    );
    assert!(
        decoded as f64 / sent as f64 >= 0.97,
        "at least 97% of sent frames must decode"
    );
    assert!(
        requests <= 1,
        "no keyframe storm during the fifteen-second window"
    );
    assert!(
        counters.frags_repaired > 0 && retransmits > 0,
        "negotiated repair must actually run"
    );
    assert!(
        abr_down,
        "repairs above five percent must reduce the bitrate"
    );
}

/// Under injected fragment loss the system must keep delivering decodable
/// video (keyframe requests beat the GOP) and the ABR must step the bitrate
/// down from its 15 Mbps start.
#[test]
fn lossy_legacy_stream_uses_keyframe_recovery() {
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::set_var("ETERNAL_DROP", "0.05");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect(listen_port);

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut datagram = [0u8; 2048];

    let mut decoded = 0usize;
    let mut abr_stepped_down = false;
    let mut keyframe_requests = 0u32;
    let mut last_progress = Instant::now();
    let mut highest_seq = 0u32;
    let mut current_epoch = 0u32;

    while decoded < 40 || !abr_stepped_down {
        assert!(
            Instant::now() < deadline,
            "timed out: decoded={decoded}, abr_stepped_down={abr_stepped_down}, \
             keyframe_requests={keyframe_requests}, counters={:?}",
            reassembler.counters()
        );

        // Real loss accounting: the reassembler's counters feed the reports
        // that drive the host ABR.
        let counters = reassembler.counters();
        if receiver.last_report.elapsed() >= Duration::from_millis(400) {
            receiver.last_report = Instant::now();
            receiver.send(&ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: current_epoch,
                highest_seq,
                frames_complete: counters.frames_complete as u32,
                frames_dropped: counters.frames_dropped as u32,
                frags_received: counters.frags_received as u32,
                frags_lost: counters.frags_lost as u32,
                ..Default::default()
            }));
        }

        // Client-side recovery: stuck for 400ms -> ask for a keyframe.
        if last_progress.elapsed() >= Duration::from_millis(400) {
            last_progress = Instant::now();
            keyframe_requests += 1;
            receiver.send(&ControlMessage::KeyframeRequest(KeyframeRequest {
                stream_epoch: current_epoch,
                last_complete_seq: highest_seq,
                reason: KeyframeReason::GapLoss,
            }));
        }

        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            continue;
        };
        let bytes = &datagram[..len];

        match classify(bytes) {
            Classified::Control(_) => {
                if let Ok((_, ControlMessage::Heartbeat(hb))) = parse_control(bytes) {
                    if hb.stream_config.bitrate_bps < 15_000_000 {
                        abr_stepped_down = true;
                    }
                }
            }
            Classified::Media { .. } => {
                let Ok((header, payload)) = MediaHeader::decode(bytes) else {
                    continue;
                };
                highest_seq = highest_seq.max(header.frame_seq);
                current_epoch = header.stream_epoch;
                if let AddOutcome::Completed(frame_bytes) = reassembler.add_fragment(
                    header.frame_seq,
                    header.frag_index,
                    header.frag_count,
                    header.stream_epoch,
                    payload,
                    Instant::now(),
                ) {
                    let frames = decoder.decode(&frame_bytes);
                    if !frames.is_empty() {
                        decoded += frames.len();
                        last_progress = Instant::now();
                    }
                }
            }
            _ => {}
        }
    }

    let counters = reassembler.counters();
    assert!(
        counters.frags_lost > 0,
        "injected drop must surface as fragment loss"
    );

    std::env::remove_var("ETERNAL_DROP");

    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}

/// HEVC negotiation end to end: with the host preference on and a client
/// advertising HEVC decode, the encoder switches to H.265 mid-session —
/// keyframes carry VPS/SPS/PPS, pictures decode with advancing counters, and
/// heartbeats advertise codec=HEVC. The switch is a live reopen, so a stray
/// H.264 access unit from before the handoff is tolerated (the iPad's decoder
/// sniffs the codec per keyframe for exactly this reason).
#[test]
fn hevc_stream_negotiates_and_decodes() {
    use eternal_wire::v2::control::{CAP_DECODE_HEVC, CODEC_HEVC};

    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    if ffmpeg_next::encoder::find_by_name("libx265").is_none() {
        eprintln!("libx265 not present in this FFmpeg build — skipping HEVC E2E");
        return;
    }

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::remove_var("ETERNAL_DROP");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    shared
        .hevc_enabled
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver =
        FakeReceiver::connect_full(listen_port, CAP_DECODE_H264 | CAP_DECODE_HEVC, 0);

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::with_codec(ffmpeg_next::codec::Id::HEVC);
    let mut datagram = [0u8; 2048];

    let mut decoded_hevc: Vec<u64> = Vec::new();
    let mut hevc_parameter_sets_seen = false;
    let mut hevc_codec_advertised = false;
    let mut highest_seq = 0u32;

    while decoded_hevc.len() < WANT_DECODED_FRAMES
        || !hevc_parameter_sets_seen
        || !hevc_codec_advertised
    {
        assert!(
            Instant::now() < deadline,
            "timed out: decoded={}, vps_seen={hevc_parameter_sets_seen}, \
             advertised={hevc_codec_advertised} (reassembly: {:?})",
            decoded_hevc.len(),
            reassembler.counters()
        );

        receiver.maybe_report(highest_seq, decoded_hevc.len() as u32);

        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            continue;
        };
        let bytes = &datagram[..len];

        match classify(bytes) {
            Classified::Control(_) => {
                if let Ok((_, ControlMessage::Heartbeat(hb))) = parse_control(bytes) {
                    if hb.stream_config.codec == CODEC_HEVC {
                        hevc_codec_advertised = true;
                    }
                }
            }
            Classified::Media { .. } => {
                let (header, payload) = MediaHeader::decode(bytes).expect("valid media datagram");
                highest_seq = highest_seq.max(header.frame_seq);
                let AddOutcome::Completed(frame_bytes) = reassembler.add_fragment(
                    header.frame_seq,
                    header.frag_index,
                    header.frag_count,
                    header.stream_epoch,
                    payload,
                    Instant::now(),
                ) else {
                    continue;
                };

                // Ignore anything before the HEVC handoff (a leftover H.264
                // access unit from the pre-switch encoder session).
                if !hevc_parameter_sets_seen {
                    if eternal_wire::hevc::contains_parameter_sets(&frame_bytes) {
                        hevc_parameter_sets_seen = true;
                        assert!(
                            eternal_wire::hevc::contains_keyframe(&frame_bytes),
                            "the first HEVC frame must be an IRAP keyframe"
                        );
                        assert!(
                            header.is_keyframe,
                            "the wire keyframe flag must be set on the HEVC IRAP"
                        );
                    } else {
                        continue;
                    }
                }

                for counter in decoder.decode(&frame_bytes) {
                    assert_eq!(
                        counter,
                        header.frame_seq as u64 & 0xFF_FFFF,
                        "decoded HEVC frame counter must match the wire sequence"
                    );
                    decoded_hevc.push(counter);
                }
            }
            _ => {}
        }
    }

    for pair in decoded_hevc.windows(2) {
        assert!(pair[1] > pair[0], "HEVC counters must advance: {pair:?}");
    }

    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}

/// Input relay, wire to injection: a client that asked for input in HELLO2
/// sends touches/scrolls; the host maps them through the synthetic capture
/// geometry and "injects" them into the off-Windows recorder. Duplicate edges
/// (sent twice for loss tolerance) must inject exactly once.
#[cfg(not(windows))]
#[test]
fn input_relay_maps_touches_end_to_end() {
    use eternal_host::input::{recorder, Injection, KIND_SCROLL, KIND_TOUCH};
    use eternal_wire::v2::control::{InputEvent, FEATURE_WANTS_INPUT};

    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::remove_var("ETERNAL_DROP");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect_with_caps(listen_port, FEATURE_WANTS_INPUT);

    // Wait for one completed frame: capture is then certainly up, which means
    // the capture geometry the relay maps through has been published.
    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut datagram = [0u8; 2048];
    let mut highest_seq = 0u32;
    'media: loop {
        assert!(Instant::now() < deadline, "no media before input phase");
        receiver.maybe_report(highest_seq, 0);
        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            continue;
        };
        if let Classified::Media { .. } = classify(&datagram[..len]) {
            let (header, payload) = MediaHeader::decode(&datagram[..len]).unwrap();
            highest_seq = highest_seq.max(header.frame_seq);
            if let AddOutcome::Completed(_) = reassembler.add_fragment(
                header.frame_seq,
                header.frag_index,
                header.frag_count,
                header.stream_epoch,
                payload,
                Instant::now(),
            ) {
                break 'media;
            }
        }
    }

    let _ = recorder::take(); // start the assertion window clean

    let base = InputEvent {
        input_ver: 1,
        kind: KIND_TOUCH,
        phase: 0, // began
        buttons: 1,
        event_id: 1,
        x_norm: 0,
        y_norm: 0,
        pressure_x1000: 0,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
    };

    // Tap the top-left corner: began ×2 (edge redundancy), ended ×2.
    receiver.send(&ControlMessage::InputEvent(base));
    receiver.send(&ControlMessage::InputEvent(base));
    let ended = InputEvent {
        phase: 2,
        event_id: 2,
        ..base
    };
    receiver.send(&ControlMessage::InputEvent(ended));
    receiver.send(&ControlMessage::InputEvent(ended));
    // Drag to the bottom-right corner (a move), then a two-finger scroll up.
    receiver.send(&ControlMessage::InputEvent(InputEvent {
        phase: 1,
        event_id: 3,
        x_norm: 65_535,
        y_norm: 65_535,
        ..base
    }));
    receiver.send(&ControlMessage::InputEvent(InputEvent {
        kind: KIND_SCROLL,
        phase: 1,
        event_id: 4,
        x_norm: 32_768,
        y_norm: 32_768,
        buttons: 0,
        scroll_dy: -40,
        ..base
    }));

    // Give the loopback datagrams a moment to be routed and injected. The
    // scroll event was sent last, so once its two injections appear (7 total)
    // everything before it has been processed; a short settle then catches
    // any wrongly-injected duplicate.
    let inject_deadline = Instant::now() + Duration::from_secs(5);
    while recorder::peek().len() < 7 && Instant::now() < inject_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(150));
    let recorded = recorder::take();
    assert!(
        recorded.len() >= 7,
        "expected 7 injections within 5s, got {recorded:?}"
    );

    // Tap at (0,0): move + left down, then move + left up — exactly once
    // despite each edge arriving twice.
    assert_eq!(
        &recorded[..4],
        &[
            Injection::MoveAbs { x: 0, y: 0 },
            Injection::LeftDown { x: 0, y: 0 },
            Injection::MoveAbs { x: 0, y: 0 },
            Injection::LeftUp { x: 0, y: 0 },
        ],
        "tap must inject exactly one click (recorded: {recorded:?})"
    );
    // Drag to the far corner maps to the absolute-space maximum.
    assert_eq!(
        recorded[4],
        Injection::MoveAbs {
            x: 65_535,
            y: 65_535
        }
    );
    // Scroll: pointer placed at ~center, then a wheel tick down.
    let Injection::MoveAbs { x, y } = recorded[5] else {
        panic!("expected scroll pointer move, got {:?}", recorded[5]);
    };
    assert!((i32::from(x) - 32_768).unsigned_abs() < 300, "scroll x {x}");
    assert!((i32::from(y) - 32_768).unsigned_abs() < 300, "scroll y {y}");
    assert_eq!(recorded[6], Injection::Wheel { delta: -120 });
    assert_eq!(recorded.len(), 7, "no extra injections: {recorded:?}");

    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}

/// An encoder crash mid-stream must be contained by the supervisor: automatic
/// restart with backoff, a fresh stream epoch, and the SAME session resuming
/// (the client never re-handshakes; its assembler resets on the epoch bump).
#[test]
fn encoder_crash_auto_restarts_and_stream_recovers() {
    let _guard = ENV_LOCK.lock().unwrap();
    init_test_tracing();
    let _ = ffmpeg_next::init();

    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::remove_var("ETERNAL_DROP");
    std::env::set_var("ETERNAL_FAULT_ENCODER_AFTER", "45");

    let listen_port = free_udp_port();
    let shared = SharedControl::new(listen_port, pipeline::DEFAULT_BITRATE_BPS);
    *shared.encoder_override.lock() = Some("libx264".to_string());
    let gpu_info = GpuInfo::software_fallback();

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let supervisor_shared = shared.clone();
    let supervisor_tx_clone = supervisor_tx.clone();
    let supervisor = std::thread::spawn(move || {
        eternal_host::supervisor::run(
            listen_port,
            supervisor_shared,
            gpu_info,
            supervisor_tx_clone,
            supervisor_rx,
        );
    });

    let mut receiver = FakeReceiver::connect(listen_port);
    let original_session = receiver.session_id;

    let deadline = Instant::now() + DEADLINE;
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut datagram = [0u8; 2048];

    let mut epochs_seen: Vec<u32> = Vec::new();
    let mut decoded_first_epoch = 0usize;
    let mut decoded_second_epoch = 0usize;
    let mut gap_started: Option<Instant> = None;
    let mut recovery_gap = Duration::ZERO;
    let mut highest_seq = 0u32;

    while decoded_second_epoch < 30 {
        assert!(
            Instant::now() < deadline,
            "timed out: epochs={epochs_seen:?}, first={decoded_first_epoch}, \
             second={decoded_second_epoch}"
        );
        receiver.maybe_report(
            highest_seq,
            (decoded_first_epoch + decoded_second_epoch) as u32,
        );

        let Ok((len, _)) = receiver.socket.recv_from(&mut datagram) else {
            if gap_started.is_none() && decoded_first_epoch >= 30 {
                gap_started = Some(Instant::now());
            }
            continue;
        };
        let bytes = &datagram[..len];
        let Classified::Media { .. } = classify(bytes) else {
            continue;
        };
        let Ok((header, payload)) = MediaHeader::decode(bytes) else {
            continue;
        };
        assert_eq!(
            header.session_id, original_session,
            "the session must survive the pipeline restart — no re-handshake"
        );
        highest_seq = highest_seq.max(header.frame_seq);
        if !epochs_seen.contains(&header.stream_epoch) {
            epochs_seen.push(header.stream_epoch);
            if epochs_seen.len() == 2 {
                if let Some(started) = gap_started {
                    recovery_gap = started.elapsed();
                }
            }
        }

        if let AddOutcome::Completed(frame_bytes) = reassembler.add_fragment(
            header.frame_seq,
            header.frag_index,
            header.frag_count,
            header.stream_epoch,
            payload,
            Instant::now(),
        ) {
            let frames = decoder.decode(&frame_bytes).len();
            if header.stream_epoch == epochs_seen[0] {
                decoded_first_epoch += frames;
            } else {
                decoded_second_epoch += frames;
            }
        }
    }

    assert!(
        decoded_first_epoch >= 20,
        "should stream normally before the injected crash (got {decoded_first_epoch})"
    );
    assert_eq!(epochs_seen.len(), 2, "exactly one restart expected");
    assert!(
        epochs_seen[1] > epochs_seen[0],
        "the new generation must carry a higher stream epoch"
    );
    assert!(
        recovery_gap < Duration::from_secs(5),
        "recovery took {recovery_gap:?} — the backoff restart should be ~0.5-2s"
    );

    std::env::remove_var("ETERNAL_FAULT_ENCODER_AFTER");
    shared.stop();
    supervisor_tx
        .send(SupervisorCommand::Shutdown)
        .expect("send shutdown");
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = supervisor.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("supervisor must shut down within 5s");
}
