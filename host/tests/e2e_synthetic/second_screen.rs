//! Second screen end to end: the real supervisor, synthetic capture, x264 and
//! transport, with the real second screen worker turning a fake virtual
//! display on and off (`second_screen::fake`, debug builds only). In the
//! reported cases the iPad lost the picture around Second screen: it
//! redialled while the display attached, or the display attached slowly or
//! never. Each test guards one of the fixes:
//! - mirror first, then switch once the display attaches (#97, #102),
//! - a failed start remembered with a wait, across app runs (#108),
//! - the switch as a new stream epoch on the same session and USB tunnel.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use eternal_host::capture::second_screen::{self, fake::FakeDisplay};
use eternal_host::control::{CaptureTarget, VddStatus};
use eternal_host::supervisor::CURRENT_GENERATION;

use super::*;

/// The iPad gives up 10 s after its HELLO2 when no picture has decoded.
const IPAD_TIMEOUT: Duration = Duration::from_secs(10);

/// Each decoded picture's stream epoch, and when it decoded.
type Pictures = Vec<(u32, Instant)>;

/// One app run of the host in Second screen mode, with `display` as its
/// virtual display driver. Dropping it quits, as the app does.
struct Host {
    shared: SharedControl,
    port: u16,
    supervisor_tx: mpsc::Sender<SupervisorCommand>,
    supervisor: Option<std::thread::JoinHandle<()>>,
    /// The pipeline generation running before any iPad connected.
    generation: u64,
}

impl Host {
    fn start(display: &FakeDisplay, memory: &Path, wall: SystemTime) -> Self {
        let port = free_udp_port();
        let shared = SharedControl::new(port, pipeline::DEFAULT_BITRATE_BPS);
        shared.pairing.lock().required = false;
        *shared.encoder_override.lock() = Some("libx264".into());
        *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
        *shared.vdd_status.lock() = VddStatus::WaitingForClient;
        display.install(&shared, memory, wall);
        let (supervisor_tx, supervisor_rx) = mpsc::channel();
        let before = CURRENT_GENERATION.load(Ordering::SeqCst);
        let supervisor_shared = shared.clone();
        let supervisor_sender = supervisor_tx.clone();
        let supervisor = std::thread::spawn(move || {
            eternal_host::supervisor::run(
                port,
                supervisor_shared,
                GpuInfo::software_fallback(),
                supervisor_sender,
                supervisor_rx,
            )
        });
        let started = Instant::now();
        while CURRENT_GENERATION.load(Ordering::SeqCst) == before {
            assert!(started.elapsed() < Duration::from_secs(10), "no pipeline");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            shared,
            port,
            supervisor_tx,
            supervisor: Some(supervisor),
            generation: CURRENT_GENERATION.load(Ordering::SeqCst),
        }
    }

    fn connect(&self, nonce: u32) -> FakeReceiver {
        FakeReceiver::connect_as(self.port, CAP_DECODE_H264, 0, 0, nonce)
    }

    fn status(&self) -> VddStatus {
        *self.shared.vdd_status.lock()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shared.stop();
        let _ = self.supervisor_tx.send(SupervisorCommand::Shutdown);
        if let Some(supervisor) = self.supervisor.take() {
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = supervisor.join();
                let _ = done_tx.send(());
            });
            let stopped = done_rx.recv_timeout(Duration::from_secs(30)).is_ok();
            assert!(stopped || std::thread::panicking(), "host shutdown");
        }
        second_screen::shut_down(&self.shared);
        second_screen::finish_saving(&self.shared);
    }
}

/// Synthetic capture at the harness's size, without injected loss.
fn synthetic_env() {
    init_test_tracing();
    ffmpeg_next::init().unwrap();
    std::env::set_var("ETERNAL_CAPTURE", "synthetic");
    std::env::set_var("ETERNAL_SYNTH_SIZE", format!("{SYNTH_W}x{SYNTH_H}"));
    for name in ["ETERNAL_DROP", "ETERNAL_REORDER", "ETERNAL_JITTER_MS"] {
        std::env::remove_var(name);
    }
}

/// An empty folder standing for the app data folder.
fn memory_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("em-e2e-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The failure saved in `memory`, as the next app run reads it.
fn saved_failure(memory: &Path) -> Option<serde_json::Value> {
    let data = std::fs::read(memory.join("second-screen-state.json")).ok()?;
    let file: serde_json::Value = serde_json::from_slice(&data).unwrap();
    Some(file["failure"].clone()).filter(|failure| !failure.is_null())
}

/// [`saved_failure`] once it is there, within 10 s: the host may write the
/// file a moment after it decides to save.
fn saved_failure_soon(memory: &Path) -> Option<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let saved = saved_failure(memory);
        if saved.is_some() || Instant::now() >= deadline {
            return saved;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Plays the iPad on `receiver`: reassembles and decodes the stream and
/// sends reports, until `done` holds for the pictures so far. Panics after
/// `backstop`.
fn watch(
    receiver: &mut FakeReceiver,
    backstop: Duration,
    mut done: impl FnMut(&Pictures) -> bool,
) -> Pictures {
    let started = Instant::now();
    let origin = Instant::now();
    let mut reassembler = Reassembler::new();
    let mut decoder = H264TestDecoder::new();
    let mut pictures = Vec::new();
    let mut highest_seq = 0;
    let mut buffer = [0u8; 2048];
    while !done(&pictures) {
        assert!(
            started.elapsed() < backstop,
            "{} pictures in {backstop:?}",
            pictures.len()
        );
        receiver.maybe_report(highest_seq, pictures.len() as u32);
        let Ok((len, _)) = receiver.socket.recv_from(&mut buffer) else {
            continue;
        };
        if !matches!(classify(&buffer[..len]), Classified::Media { .. }) {
            continue;
        }
        let (header, payload) = MediaHeader::decode(&buffer[..len]).unwrap();
        assert_eq!(header.session_id, receiver.session_id);
        highest_seq = header.frame_seq;
        if let AddOutcome::Completed(frame) = reassembler.add_fragment(
            header.frame_seq,
            header.frag_index,
            header.frag_count,
            header.stream_epoch,
            payload,
            stream_time(origin, &header),
        ) {
            for _ in decoder.decode(&frame) {
                pictures.push((header.stream_epoch, Instant::now()));
            }
        }
    }
    pictures
}

/// The time from `hello` to the first picture, which must be within the
/// iPad's timeout.
fn first_picture_ms(pictures: &Pictures, hello: Instant) -> u128 {
    let first = pictures.first().expect("a picture").1.duration_since(hello);
    assert!(
        first < IPAD_TIMEOUT,
        "the first picture took {first:?} after HELLO2: the iPad gives up at {IPAD_TIMEOUT:?}"
    );
    first.as_millis()
}

/// The stream epochs in the order the pictures showed them.
fn epochs(pictures: &Pictures) -> Vec<u32> {
    let mut epochs: Vec<u32> = pictures.iter().map(|(epoch, _)| *epoch).collect();
    epochs.dedup();
    epochs
}

/// How many pictures came after the first epoch.
fn after_first_epoch(pictures: &Pictures) -> usize {
    pictures
        .iter()
        .filter(|(epoch, _)| *epoch != pictures[0].0)
        .count()
}

/// The most display changes within any `window`.
fn most_toggles_within(toggles: &[(Duration, bool)], window: Duration) -> usize {
    toggles
        .iter()
        .map(|(start, _)| {
            toggles
                .iter()
                .filter(|(at, _)| *at >= *start && *at < *start + window)
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Waits, briefly, for the summary of the session the iPad with `nonce`
/// just ended.
fn summary(nonce: u32) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(summary) = summaries_with_nonce(nonce).pop() {
            return summary;
        }
        assert!(Instant::now() < deadline, "no summary for {nonce:x}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A 0.3.2 iPad gave up 10 s after its HELLO2 without a picture, and over
/// USB it was dialled again a second later; 0.3.2 restarted the stream for
/// each connection and held video back until the display attached. Here the
/// iPad leaves 10 s into each session and comes back 1 s later, three times,
/// while the display is still attaching. Every session gets the mirror from
/// the running pipeline within the iPad's timeout, nothing restarts, and the
/// display is turned on once.
#[test]
fn a_reconnect_storm_while_the_display_attaches_mirrors_each_time_and_attaches_once() {
    let _env = test_env();
    synthetic_env();
    let memory = memory_dir("storm");
    // Still attaching after the storm: the worker waits as long as the
    // update manifest allows.
    let display = FakeDisplay::new(None, Duration::from_secs(60));
    let host = Host::start(&display, &memory, SystemTime::now());
    for session in 0..4 {
        if session > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        let nonce = 0x5707_0000 + session;
        let hello = Instant::now();
        let mut receiver = host.connect(nonce);
        // The last redial only needs its picture.
        let pictures = watch(&mut receiver, IPAD_TIMEOUT * 2, |pictures| {
            if session == 3 {
                pictures.len() >= 10
            } else {
                hello.elapsed() >= IPAD_TIMEOUT
            }
        });
        let first_ms = first_picture_ms(&pictures, hello);
        assert_eq!(
            CURRENT_GENERATION.load(Ordering::SeqCst),
            host.generation,
            "session {session}: the mirror comes from the pipeline that ran before"
        );
        assert_eq!(epochs(&pictures).len(), 1, "session {session}");
        assert_eq!(host.status(), VddStatus::Attaching, "session {session}");
        let toggles = display.toggles();
        assert_eq!(
            toggles.iter().filter(|(_, on)| *on).count(),
            1,
            "session {session}: one Enable for the whole storm: {toggles:?}"
        );
        assert!(
            most_toggles_within(&toggles, Duration::from_secs(60)) <= 2,
            "{toggles:?}"
        );
        eprintln!(
            "SECOND_SCREEN_STORM session={session} first_picture_ms={first_ms} pictures={}",
            pictures.len()
        );
        if session < 3 {
            // As the 0.3.2 iPad did at its connect timeout.
            receiver.send(&ControlMessage::Bye(ByeReason::Error));
        }
    }
    drop(host);
    let _ = std::fs::remove_dir_all(&memory);
}

/// A display that never attaches within the worker's wait: the iPad keeps
/// the mirror it had all along, and the failure is saved. The next app run
/// mirrors without touching the driver until the wait after one failure
/// (an hour) is over; the run after that tries again.
#[test]
fn an_attach_timeout_mirrors_and_is_remembered_until_the_wait_ends() {
    let _env = test_env();
    synthetic_env();
    let memory = memory_dir("remembered");
    let started = SystemTime::now();

    // App run 1. ETERNAL_VDD_TIMEOUT_SECS=10, 0.3.2's wait, keeps it short.
    let display = FakeDisplay::new(None, Duration::from_secs(10));
    let host = Host::start(&display, &memory, started);
    let hello = Instant::now();
    let mut receiver = host.connect(0x5707_1001);
    let mut pictures = watch(&mut receiver, Duration::from_secs(40), |pictures| {
        !pictures.is_empty() && host.status() == VddStatus::Failed
    });
    let first_ms = first_picture_ms(&pictures, hello);
    pictures.extend(watch(&mut receiver, Duration::from_secs(20), |after| {
        after.len() >= 30
    }));
    assert_eq!(
        CURRENT_GENERATION.load(Ordering::SeqCst),
        host.generation,
        "the mirror from before the connection kept streaming"
    );
    assert_eq!(epochs(&pictures).len(), 1, "no restart");
    let toggles = display.toggles();
    assert_eq!(
        toggles.iter().map(|(_, on)| *on).collect::<Vec<_>>(),
        [true, false],
        "turned on, then off after the wait: {toggles:?}"
    );
    // Saved because the fake display says the desktop is there, whatever
    // session the test runs in.
    let saved = saved_failure_soon(&memory).expect("the failure is saved");
    assert_eq!(saved["reason"], "attach_timeout", "{saved}");
    assert_eq!(saved["failures"], 1, "{saved}");
    receiver.send(&ControlMessage::Bye(ByeReason::UserDisconnect));
    let summary_1 = summary(0x5707_1001);
    assert!(
        summary_1.contains(" fallback=attach_timeout "),
        "{summary_1}"
    );
    eprintln!("SECOND_SCREEN_REMEMBERED run=1 first_picture_ms={first_ms}");
    drop(host);

    // App run 2, at once: the mirror, and the display left alone.
    let display = FakeDisplay::new(Some(Duration::ZERO), Duration::from_secs(10));
    let host = Host::start(&display, &memory, SystemTime::now());
    let hello = Instant::now();
    let mut receiver = host.connect(0x5707_1002);
    let pictures = watch(&mut receiver, Duration::from_secs(20), |pictures| {
        pictures.len() >= 30 && hello.elapsed() >= Duration::from_secs(2)
    });
    let first_ms = first_picture_ms(&pictures, hello);
    assert_eq!(epochs(&pictures).len(), 1, "no switch");
    assert_eq!(display.toggles(), [], "the display is not tried again");
    assert_eq!(host.status(), VddStatus::Failed);
    receiver.send(&ControlMessage::Bye(ByeReason::UserDisconnect));
    let summary_2 = summary(0x5707_1002);
    assert!(summary_2.contains(" fallback=remembered "), "{summary_2}");
    assert_eq!(saved_failure(&memory).unwrap()["reason"], "attach_timeout");
    eprintln!("SECOND_SCREEN_REMEMBERED run=2 first_picture_ms={first_ms}");
    drop(host);

    // App run 3, once the hour after one failure is over: it is tried
    // again, attaches, and its first image forgets the failure. The display
    // attaches only once the mirror has shown, so both epochs reach the iPad.
    // Moving the fake clock on is safe here: no failure is saved at it.
    let display = FakeDisplay::new(Some(Duration::from_secs(25)), Duration::from_secs(30));
    let later = started + Duration::from_secs(60 * 60 + 60);
    let host = Host::start(&display, &memory, later);
    let mut receiver = host.connect(0x5707_1003);
    let mut advanced = false;
    let pictures = watch(&mut receiver, Duration::from_secs(30), |pictures| {
        if !advanced && !pictures.is_empty() && !display.toggles().is_empty() {
            // Mirroring while the display attaches. Now its 25 s pass.
            assert_eq!(epochs(pictures).len(), 1);
            assert_eq!(host.status(), VddStatus::Attaching);
            display.advance(Duration::from_secs(25));
            advanced = true;
        }
        after_first_epoch(pictures) >= 10
    });
    assert_eq!(epochs(&pictures).len(), 2, "one switch");
    assert_eq!(display.toggles().len(), 1, "{:?}", display.toggles());
    assert_eq!(host.status(), VddStatus::Active);
    let deadline = Instant::now() + Duration::from_secs(2);
    while saved_failure(&memory).is_some() {
        assert!(Instant::now() < deadline, "the image forgets the failure");
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!("SECOND_SCREEN_REMEMBERED run=3 pictures={}", pictures.len());
    drop(host);
    let _ = std::fs::remove_dir_all(&memory);
}

/// A display that attaches 25 s after its Enable, past the iPad's 10 s and
/// 0.3.2's 10 s wait. Over USB the iPad mirrors meanwhile; then the stream
/// moves onto the second screen as one new epoch on the same session and
/// tunnel, without a reconnect, and the picture goes on.
#[test]
fn a_late_attach_moves_the_stream_as_a_new_epoch_on_the_same_tunnel() {
    use eternal_host::transport::link::{read_datagram, write_datagram};

    let _env = test_env();
    synthetic_env();
    let memory = memory_dir("late");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    std::env::set_var(
        "ETERNAL_USB_DIRECT",
        listener.local_addr().unwrap().to_string(),
    );
    let display = FakeDisplay::new(Some(Duration::from_secs(25)), Duration::from_secs(30));
    let host = Host::start(&display, &memory, SystemTime::now());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let mut stream = accept_host_tunnel(&listener, Duration::from_secs(30)).await;
        stream.set_nodelay(true).unwrap();
        let hello = ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 0x5707_2001,
            listen_port: 0,
            decoder_caps: CAP_DECODE_H264,
            feature_caps: 0,
            screen_px_w: SYNTH_W as u16,
            screen_px_h: SYNTH_H as u16,
            screen_pt_w: SYNTH_W as u16,
            screen_pt_h: SYNTH_H as u16,
            refresh_hz: 60,
            device_name: "USB E2E iPad".into(),
            device_id: 77,
            preferred_fps: 60,
            auth_token: [0; 16],
            pairing_code: 0,
        });
        let sent = Instant::now();
        write_datagram(&mut stream, &encode_control(0, 1, &hello))
            .await
            .unwrap();

        let origin = Instant::now();
        let mut reassembler = Reassembler::new();
        let mut decoder = H264TestDecoder::new();
        let mut pictures: Pictures = Vec::new();
        let (mut session, mut acks, mut msg_seq, mut highest_seq) = (0, 0, 1, 0);
        let mut last_report = Instant::now();
        let mut advanced = false;
        while pictures.is_empty() || after_first_epoch(&pictures) < 30 {
            assert!(
                sent.elapsed() < Duration::from_secs(40),
                "{} pictures, epochs {:?}, status {:?}",
                pictures.len(),
                epochs(&pictures),
                host.status()
            );
            if !advanced && pictures.len() >= 30 {
                // Mirroring while the display attaches. Now its 25 s pass.
                assert_eq!(host.status(), VddStatus::Attaching);
                assert_eq!(CURRENT_GENERATION.load(Ordering::SeqCst), host.generation);
                display.advance(Duration::from_secs(25));
                advanced = true;
            }
            if session != 0 && last_report.elapsed() >= Duration::from_millis(300) {
                last_report = Instant::now();
                msg_seq += 1;
                let report = ControlMessage::ReceiverReport(ReceiverReport {
                    stream_epoch: pictures.last().map_or(0, |(epoch, _)| *epoch),
                    highest_seq,
                    frames_complete: pictures.len() as u32,
                    decode_fps_x10: 600,
                    ..ReceiverReport::default()
                });
                write_datagram(&mut stream, &encode_control(session, msg_seq, &report))
                    .await
                    .unwrap();
            }
            let bytes = tokio::time::timeout(Duration::from_secs(10), read_datagram(&mut stream))
                .await
                .expect("the tunnel stays active")
                .expect("the tunnel stays open");
            if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&bytes) {
                assert_eq!(ack.status, HelloStatus::Ok);
                session = ack.session_id;
                acks += 1;
                continue;
            }
            let Ok((header, payload)) = MediaHeader::decode(&bytes) else {
                continue;
            };
            assert_eq!(header.session_id, session, "one session throughout");
            highest_seq = header.frame_seq;
            if let AddOutcome::Completed(frame) = reassembler.add_fragment(
                header.frame_seq,
                header.frag_index,
                header.frag_count,
                header.stream_epoch,
                payload,
                stream_time(origin, &header),
            ) {
                for _ in decoder.decode(&frame) {
                    pictures.push((header.stream_epoch, Instant::now()));
                }
            }
        }
        let first_ms = first_picture_ms(&pictures, sent);
        // The host dials again only after its tunnel closed: this one
        // carried both epochs, so there was no reconnect. Other programs on
        // a shared machine may dial this loopback port, so the listener
        // can't tell.
        assert_eq!(acks, 1, "no new handshake");
        assert_eq!(epochs(&pictures).len(), 2, "one epoch change");
        assert_eq!(
            CURRENT_GENERATION.load(Ordering::SeqCst),
            host.generation + 1,
            "one restart onto the second screen"
        );
        assert_eq!(host.status(), VddStatus::Active);
        assert_eq!(host.shared.session.lock().session_id(), Some(session));
        assert_eq!(display.toggles().len(), 1, "{:?}", display.toggles());
        let gap = pictures
            .windows(2)
            .find(|pair| pair[0].0 != pair[1].0)
            .map(|pair| pair[1].1.duration_since(pair[0].1))
            .unwrap();
        assert!(gap < IPAD_TIMEOUT, "the picture paused for {gap:?}");
        eprintln!(
            "SECOND_SCREEN_LATE first_picture_ms={first_ms} switch_gap_ms={} pictures={}",
            gap.as_millis(),
            pictures.len()
        );
    });
    drop(host);
    std::env::remove_var("ETERNAL_USB_DIRECT");
    let _ = std::fs::remove_dir_all(&memory);
}
