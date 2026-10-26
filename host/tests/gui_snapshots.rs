#![cfg(target_os = "macos")]

use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::{kittest::Queryable, Harness, SnapshotResults};
use eternal_host::capture::recovery::CaptureBlock;
use eternal_host::control::{GuiControl, SharedControl, VddStatus};
use eternal_host::gui::AnalyzerApp;
use eternal_host::settings::SettingsFile;
use eternal_host::stats::{PipelineStats, PIPELINE_STATS};
use eternal_host::transport::link::PeerId;
use eternal_host::transport::session::ConfigSource;
use eternal_wire::v2::control::{
    ByeReason, ControlMessage, Hello2, ReceiverReport, StreamConfig, FEATURE_CHECKS_UPDATES,
};

struct Fixture;
impl ConfigSource for Fixture {
    fn host_name(&self) -> String {
        "Reference PC".into()
    }
    fn stream_config(&self) -> StreamConfig {
        StreamConfig {
            stream_epoch: 1,
            width: 1920,
            height: 1080,
            fps: 60,
            codec: 1,
            flags: 0,
            bitrate_bps: 15_000_000,
        }
    }
}

const POPUP_QUIET_MS: u64 = 3000;

fn popup_shown(harness: &Harness<'_, AnalyzerApp>) -> bool {
    harness
        .query_by_role_and_label(Role::Dialog, "Update ready")
        .is_some()
}

/// The popup opens once no iPad has been connected for the quiet period.
fn wait_for_popup(harness: &mut Harness<'_, AnalyzerApp>) {
    let deadline = Instant::now() + std::time::Duration::from_millis(POPUP_QUIET_MS + 7000);
    while !popup_shown(harness) {
        assert!(Instant::now() < deadline, "the popup never opened");
        std::thread::sleep(std::time::Duration::from_millis(50));
        harness.run_steps(1);
    }
    harness.run_steps(4);
}

/// Draws frames for `millis` and checks the popup stays closed in each.
fn no_popup_for(harness: &mut Harness<'_, AnalyzerApp>, millis: u64, why: &str) {
    let end = Instant::now() + std::time::Duration::from_millis(millis);
    loop {
        harness.run_steps(1);
        assert!(!popup_shown(harness), "{why}");
        if Instant::now() >= end {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn snapshot(harness: &mut Harness<'_, AnalyzerApp>, shared: &SharedControl, name: &str) {
    harness.run_steps(4);
    mask_pairing_code(harness, shared);
    harness.snapshot(name);
}

/// The production pairing code remains random. Only its six digits are
/// masked; finding the node proves AccessKit exposes the host's actual code.
fn mask_pairing_code(harness: &mut Harness<'_, AnalyzerApp>, shared: &SharedControl) {
    let code = format!("{:06}", shared.pairing.lock().code());
    let shown = format!("{} {}", &code[..3], &code[3..]);
    if let Some(node) = harness.query_by_label(&shown) {
        harness.mask(node.rect());
    }
}

fn nav(harness: &mut Harness<'_, AnalyzerApp>, page: &str) {
    harness.get_by_role_and_label(Role::Button, page).click();
    harness.run_steps(4);
}

/// Isolated settings, a cached update notice and one connected USB iPad.
fn fixture() -> (std::path::PathBuf, SharedControl) {
    let dir = std::env::temp_dir().join(format!("eternal-gui-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("EternalMonitor")).unwrap();
    // This integration test never touches the user's real settings.
    std::env::set_var("APPDATA", &dir);
    // The popup's quiet period after an iPad leaves, shortened from 20 s.
    std::env::set_var("ETERNAL_UPDATE_POPUP_QUIET_MS", POPUP_QUIET_MS.to_string());
    SettingsFile {
        check_for_updates: false,
        ..Default::default()
    }
    .save();
    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let latest = serde_json::json!({
        "checked_at": checked_at,
        "latest": {
            "version": "99.0.0",
            "installer": "https://github.com/sidebandstudio/EternalMonitor/releases/download/v99.0.0/EternalMonitor-Setup.exe",
            "sha256": "0".repeat(64),
        },
        // Already announced, so the test never shows a Windows notification.
        "notified": "99.0.0",
    });
    std::fs::write(
        dir.join("EternalMonitor/update-state.json"),
        latest.to_string(),
    )
    .unwrap();

    let shared = SharedControl::new(9876, 15_000_000);
    shared.pairing.lock().configure(true, [0x11; 16]);
    *PIPELINE_STATS.lock() = pipeline_stats();
    (dir, shared)
}

fn connect_ipad(shared: &SharedControl) {
    connect_named(shared, "Test iPad Pro", 10, 20);
}

fn connect_named(shared: &SharedControl, device_name: &str, frags_lost: u32, frags_repaired: u32) {
    // A 0.3.2 or later iPad app.
    connect_app(
        shared,
        device_name,
        frags_lost,
        frags_repaired,
        3 | FEATURE_CHECKS_UPDATES,
    );
}

fn connect_app(
    shared: &SharedControl,
    device_name: &str,
    frags_lost: u32,
    frags_repaired: u32,
    feature_caps: u16,
) {
    let peer = PeerId::usb(7);
    let now = Instant::now();
    let mut session = shared.session.lock();
    let actions = session.handle_control(
        peer,
        0,
        ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            // Each app connects afresh rather than retransmitting.
            client_nonce: 12 + u32::from(feature_caps),
            listen_port: 9877,
            decoder_caps: 3,
            feature_caps,
            screen_px_w: 2420,
            screen_px_h: 1668,
            screen_pt_w: 1210,
            screen_pt_h: 834,
            refresh_hz: 120,
            device_name: device_name.into(),
            device_id: 7,
            preferred_fps: 60,
            auth_token: [0x11; 16],
            pairing_code: 0,
        }),
        &Fixture,
        now,
    );
    assert_eq!(actions.new_target, Some(peer));
    let session_id = session.session_id().unwrap();
    session.handle_control(
        peer,
        session_id,
        ControlMessage::ReceiverReport(ReceiverReport {
            stream_epoch: 1,
            highest_seq: 1200,
            frames_complete: 1200,
            frames_dropped: 1,
            frags_received: 970,
            frags_lost,
            frags_repaired,
            jitter_us: 450,
            decode_fps_x10: 599,
            assembler_depth: 0,
            decode_depth: 0,
            e2e_latency_ms_x10: 184,
            rtt_ms_x10: 32,
            nacks_sent: 10,
            audio_packets_lost: 0,
            audio_buffer_ms: 60,
        }),
        &Fixture,
        now,
    );
}

/// The connected iPad says goodbye, as when its user taps Disconnect.
fn disconnect_ipad(shared: &SharedControl) {
    let mut session = shared.session.lock();
    let session_id = session.session_id().unwrap();
    session.handle_control(
        PeerId::usb(7),
        session_id,
        ControlMessage::Bye(ByeReason::UserDisconnect),
        &Fixture,
        Instant::now(),
    );
    assert!(session.session_id().is_none());
}

/// Another receiver report from the connected iPad, with cumulative counters.
fn report(shared: &SharedControl, frags_received: u32, frags_lost: u32) {
    let mut session = shared.session.lock();
    let session_id = session.session_id().unwrap();
    session.handle_control(
        PeerId::usb(7),
        session_id,
        ControlMessage::ReceiverReport(ReceiverReport {
            stream_epoch: 1,
            frags_received,
            frags_lost,
            decode_fps_x10: 599,
            e2e_latency_ms_x10: 184,
            rtt_ms_x10: 32,
            audio_buffer_ms: 60,
            ..Default::default()
        }),
        &Fixture,
        Instant::now(),
    );
}

fn pipeline_stats() -> PipelineStats {
    let mut stats = PipelineStats::new();
    stats.listen_addr = "192.0.2.10:9876".into();
    stats.target_addr = "USB device 7".into();
    stats.pipeline_running = true;
    stats.capture_fps = 60.0;
    stats.encode_fps = 60.0;
    stats.transport_fps = 60.0;
    stats.capture_resolution = (1920, 1080);
    stats.capture_display = "Extended display".into();
    stats.gpu_name = "Reference NVIDIA GPU".into();
    stats.codec_name = "h264_nvenc".into();
    stats.bitrate_bps = 15_000_000;
    stats.encode_time_us = 1250;
    stats.latency_ms = 18.4;
    stats.bandwidth_mbps = 14.8;
    stats.usb_service_reachable = true;
    stats.usb_devices = 1;
    stats.usb_link_state = "Connected".into();
    stats.audio = eternal_host::audio::AudioStats::started("PC headphones");
    stats.audio.kbps = 128.0;
    stats.audio.packets_per_sec = 50.0;
    // A fixed, plausible encode-time trace so the chart is deterministic.
    stats.encode_time_history = (0..300)
        .map(|i| 1.1 + 0.35 * ((i as f64) * 0.21).sin() + if i % 47 == 0 { 1.6 } else { 0.0 })
        .collect();
    stats
}

fn harness(
    shared: &SharedControl,
    size: egui::Vec2,
    pixels_per_point: f32,
) -> Harness<'static, AnalyzerApp> {
    let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
    let control = GuiControl {
        shared: shared.clone(),
        supervisor_tx,
    };
    let mut harness = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(pixels_per_point)
        .with_theme(egui::Theme::Dark)
        .wgpu()
        .build_eframe(|cc| AnalyzerApp::new(cc, control));
    harness.run_steps(4);
    harness
}

#[test]
fn stream_settings_pairing_and_recovery_are_visible_and_operable() {
    let (dir, shared) = fixture();
    connect_ipad(&shared);
    let mut harness = harness(&shared, egui::vec2(1040.0, 1180.0), 1.0);

    // The names below are also what scripts/win/Inspect-Host.ps1 reads
    // through UI Automation on the reference PC.
    for label in [
        "Streaming to Test iPad Pro",
        "Connected iPad",
        "Test iPad Pro",
        "3.2 ms",
        "18.4 ms",
        "59.9 fps",
        "1.00%",
        "2.00%",
        "60 ms",
        "Decode rate",
        "Repaired fragments",
        "Pairing",
        "USB connection",
        "PC audio",
        "Require pairing",
        "Stream PC audio",
        "192.0.2.10:9876",
    ] {
        harness.get_by_label(label);
    }
    assert!(harness.query_by_label("Waiting for an iPad").is_none());
    // Mirror or second screen is chosen at the top of Stream, by the same
    // names the iPad uses.
    harness.get_by_label("Use the iPad as");
    harness.get_by_label("Mirror my screen");
    harness.get_by_label("Connection");
    // 1% of fragments lost in the fixture.
    harness.get_by_label("Good");
    harness.get_by_label_contains("Streaming PC headphones");
    snapshot(&mut harness, &shared, "stream-connected");

    let old_code = shared.pairing.lock().code();
    harness.get_by_label("New code").click();
    harness.run_steps(4);
    assert_ne!(shared.pairing.lock().code(), old_code);

    harness
        .get_by_role_and_label(Role::Button, "QR code")
        .click();
    harness.run_steps(4);
    // Opening the dialog refreshes the LAN address. Restore the fixed
    // fixture after exercising that action, before the deterministic image.
    PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
    harness.run_steps(4);
    harness.get_by_label("Scan to connect");
    harness.get_by_label("QR CODE");
    snapshot(&mut harness, &shared, "pairing-qr");
    harness.get_by_label("Done").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Scan to connect").is_none());

    nav(&mut harness, "Performance");
    harness.get_by_label("Uptime");
    harness.get_by_label("Encode time");
    snapshot(&mut harness, &shared, "performance");

    nav(&mut harness, "Settings");
    harness.get_by_label("Encoder input");
    harness.get_by_label("ADVANCED");
    harness.get_by_label("120").click();
    harness.run_steps(4);
    assert_eq!(shared.target_fps.load(Ordering::SeqCst), 120);
    // Encoder input sits under Advanced, below the fold.
    harness.get_by_label("BGRA").scroll_to_me();
    harness.run_steps(4);
    harness.get_by_label("BGRA").click();
    harness.run_steps(4);
    assert_eq!(
        *shared.encoder_input.lock(),
        eternal_host::encoder::input::EncoderInput::Bgra
    );
    // Encoder input applies on restart, so the page asks for one.
    harness
        .get_by_label("Restart the stream to apply your changes")
        .scroll_to_me();
    harness.run_steps(4);
    snapshot(&mut harness, &shared, "settings");

    // General sits below the fold of the Settings page.
    harness
        .get_by_label("Automatic update checks")
        .scroll_to_me();
    harness.run_steps(4);
    assert!(harness.query_by_label("Update now").is_none());
    harness.get_by_label("Automatic update checks").click();
    harness.run_steps(4);
    // The banner sits above every page, Settings included.
    harness.get_by_label("EternalMonitor 99.0.0 is available");
    harness.get_by_label("Version 99.0.0 is ready to install.");
    // The popup waits for the Stream page, and while the iPad streams, since
    // this window may be on the screen the iPad shows.
    assert!(harness
        .query_by_role_and_label(Role::Dialog, "Update ready")
        .is_none());
    nav(&mut harness, "Stream");
    assert!(harness
        .query_by_role_and_label(Role::Dialog, "Update ready")
        .is_none());
    // Nor over another dialog: the stream ends while Send feedback is open.
    harness
        .get_all_by_role_and_label(Role::Button, "Send feedback")
        .next()
        .expect("the sidebar button")
        .click();
    harness.run_steps(4);
    harness.get_by_label("Tell us how it's going");
    disconnect_ipad(&shared);
    harness.run_steps(4);
    assert!(harness
        .query_by_role_and_label(Role::Dialog, "Update ready")
        .is_none());
    harness.get_by_label("Close").click();
    wait_for_popup(&mut harness);
    {
        let popup = harness.get_by_role_and_label(Role::Dialog, "Update ready");
        popup.get_by_label("EternalMonitor 99.0.0 is ready");
        popup.get_by_label("Updating is highly recommended.");
        popup.get_by_label("Update now opens the download page.");
        popup.get_by_role_and_label(Role::Button, "Update now");
    }
    snapshot(&mut harness, &shared, "update-popup");
    // A stream that starts puts it away for now; it isn't a Later.
    connect_ipad(&shared);
    harness.run_steps(4);
    assert!(harness
        .query_by_role_and_label(Role::Dialog, "Update ready")
        .is_none());
    harness.get_by_label("EternalMonitor 99.0.0 is available");
    disconnect_ipad(&shared);
    wait_for_popup(&mut harness);
    harness
        .get_by_role_and_label(Role::Dialog, "Update ready")
        .get_by_label("Later")
        .click();
    harness.run_steps(4);
    assert!(harness
        .query_by_role_and_label(Role::Dialog, "Update ready")
        .is_none());
    connect_ipad(&shared);
    harness.run_steps(4);
    // Later hides the popup for three days, not the banner.
    nav(&mut harness, "Settings");
    nav(&mut harness, "Stream");
    assert!(harness
        .query_by_role_and_label(Role::Dialog, "Update ready")
        .is_none());
    harness.get_by_label("EternalMonitor 99.0.0 is available");
    // Setup did not install this test binary, so Update now only opens the
    // download page.
    harness.get_by_label_contains("Update now opens the download page.");
    assert!(harness
        .query_by_label_contains("EternalMonitor restarts")
        .is_none());
    harness.get_by_role_and_label(Role::Button, "Update now");
    snapshot(&mut harness, &shared, "update-banner");
    harness.get_by_label("Later").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Later").is_none());

    harness.get_by_label("Restart now").click();
    harness.run_steps(4);
    assert!(harness
        .query_by_label("Restart the stream to apply your changes")
        .is_none());

    PIPELINE_STATS.lock().using_software_fallback = true;
    *shared.vdd_status.lock() = VddStatus::TaskFailed;
    harness.run_steps(4);
    harness.get_by_label("Video is using the processor instead of the graphics card");
    harness.get_by_label("The second screen couldn't start, so your main screen is mirrored");
    harness.get_by_label("Download installer");
    // Encoder input is still BGRA from above, so the notice offers to undo it.
    harness.get_by_label("Use recommended settings");
    snapshot(&mut harness, &shared, "software-and-display-task-failure");
    harness.get_by_label("Use recommended settings").click();
    harness.run_steps(4);
    assert_eq!(
        *shared.encoder_input.lock(),
        eternal_host::encoder::input::EncoderInput::Yuv420
    );

    // Windows keeping the screen from the app is explained until frames
    // flow again.
    *shared.capture_block.lock() = Some(CaptureBlock::SecureDesktop);
    harness.run_steps(4);
    harness.get_by_label("Your PC is showing the lock screen or a permission prompt");
    *shared.capture_block.lock() = None;
    harness.run_steps(4);
    assert!(harness
        .query_by_label("Your PC is showing the lock screen or a permission prompt")
        .is_none());

    // Choosing Second screen applies at once: no separate restart prompt.
    *shared.vdd_status.lock() = VddStatus::WaitingForClient;
    PIPELINE_STATS.lock().using_software_fallback = false;
    // Let the notices disappear first, so the click lands where the choice is.
    harness.run_steps(4);
    harness.get_by_label("Second screen").click();
    harness.run_steps(4);
    assert_eq!(
        *shared.capture_target.lock(),
        eternal_host::control::CaptureTarget::VirtualExtended
    );
    assert!(harness
        .query_by_label("Restart the stream to apply your changes")
        .is_none());
    harness.get_by_label_contains("as soon as it connects");

    // The Connection rating follows the latest interval. After a million
    // clean fragments, 8,000 received and 2,000 lost is a bad interval even
    // though the session total is 0.2% loss.
    report(&shared, 1_000_000, 10);
    harness.run_steps(4);
    harness.get_by_label("Excellent");
    report(&shared, 1_008_000, 2_010);
    harness.run_steps(4);
    harness.get_by_label("Poor");

    // One set of snapshot results for every window this test opened.
    let mut results = SnapshotResults::new();
    results.extend_harness(&mut harness);
    feedback_and_star(&shared, &dir, &mut results);
    long_feedback_keeps_its_buttons_on_screen(&shared, &mut results);

    // This iPad app works but can't tell its user about updates. A 0.3.2 or
    // later app, as above, gets no such notice.
    assert!(harness
        .query_by_label("Your iPad app is out of date")
        .is_none());
    connect_app(&shared, "Test iPad Pro", 10, 20, 3);
    harness.run_steps(4);
    harness.get_by_label("Your iPad app is out of date");
    harness.get_by_label("Update it in TestFlight. It's highly recommended.");
    harness.get_by_label("OK").click();
    harness.run_steps(4);
    assert!(harness
        .query_by_label("Your iPad app is out of date")
        .is_none());
    popup_waits_while_the_ipad_has_a_session(&shared, &dir, &mut results);
    disconnect_ipad(&shared);
    popup_waits_after_launch(&shared, &dir, &mut results);
    update_arrives_while_feedback_is_open(&shared, &dir, &mut results);
    std::fs::remove_dir_all(dir).unwrap();
    results.unwrap();
}

/// The session, not the pipeline, says whether an iPad is there: a capture
/// or encoder restart, a display change or the second screen attaching stops
/// the pipeline briefly while the iPad stays connected. A USB link that drops
/// and comes back, again and again, never flashes the popup either.
fn popup_waits_while_the_ipad_has_a_session(
    shared: &SharedControl,
    dir: &std::path::Path,
    results: &mut SnapshotResults,
) {
    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        dir.join("EternalMonitor/update-state.json"),
        serde_json::json!({
            "checked_at": checked_at,
            "latest": {
                "version": "99.0.0",
                "installer": "https://github.com/sidebandstudio/EternalMonitor/releases/download/v99.0.0/EternalMonitor-Setup.exe",
                "sha256": "0".repeat(64),
            },
            "notified": "99.0.0",
        })
        .to_string(),
    )
    .unwrap();
    SettingsFile {
        check_for_updates: true,
        feedback_ask_done: true,
        star_clicked: true,
        ..Default::default()
    }
    .save();
    connect_ipad(shared);
    let mut window = harness(shared, egui::vec2(1040.0, 720.0), 1.0);
    window.get_by_label("EternalMonitor 99.0.0 is available");
    no_popup_for(&mut window, 200, "the iPad is streaming");

    // The pipeline restarts; the session stays. Longer than the quiet period.
    PIPELINE_STATS.lock().pipeline_running = false;
    no_popup_for(
        &mut window,
        POPUP_QUIET_MS + 1000,
        "the iPad is still connected while the stream restarts",
    );
    PIPELINE_STATS.lock().pipeline_running = true;
    no_popup_for(&mut window, 200, "the stream is back");

    // Three USB drops, each followed by the iPad reconnecting.
    for _ in 0..3 {
        shared.session.lock().link_lost(PeerId::usb(7).link);
        no_popup_for(&mut window, 300, "the iPad is reconnecting");
        connect_ipad(shared);
        no_popup_for(&mut window, 100, "the iPad reconnected");
    }
    // It stays away this time: the popup opens after the quiet period.
    shared.session.lock().link_lost(PeerId::usb(7).link);
    no_popup_for(&mut window, 500, "too soon after the iPad left");
    wait_for_popup(&mut window);
    window
        .get_by_role_and_label(Role::Dialog, "Update ready")
        .get_by_label("Later")
        .click();
    window.run_steps(4);
    connect_ipad(shared);
    results.extend_harness(&mut window);
}

/// An update known at launch waits the quiet period too: after a login
/// start, a USB iPad connects within seconds and may mirror this window.
fn popup_waits_after_launch(
    shared: &SharedControl,
    dir: &std::path::Path,
    results: &mut SnapshotResults,
) {
    let state = dir.join("EternalMonitor/update-state.json");
    let mut cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    cached["popup"] = serde_json::Value::Null;
    cached["later"] = serde_json::Value::Null;
    std::fs::write(&state, cached.to_string()).unwrap();
    let mut window = harness(shared, egui::vec2(1040.0, 720.0), 1.0);
    window.get_by_label("EternalMonitor 99.0.0 is available");
    no_popup_for(&mut window, POPUP_QUIET_MS - 1000, "too soon after launch");
    wait_for_popup(&mut window);
    window
        .get_by_role_and_label(Role::Dialog, "Update ready")
        .get_by_label("Later")
        .click();
    window.run_steps(4);
    results.extend_harness(&mut window);
}

/// A check that finds an update while Send feedback is open on Stream, with
/// no iPad connected: the popup waits for the panel to close, so there is
/// never more than one dialog.
fn update_arrives_while_feedback_is_open(
    shared: &SharedControl,
    dir: &std::path::Path,
    results: &mut SnapshotResults,
) {
    use std::io::{BufRead, BufReader, Write};
    // A site that answers the check only when told to.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (answer, answered) = std::sync::mpsc::channel::<()>();
    let manifest = serde_json::json!({
        "schema": 1,
        "windows": {
            "version": "99.1.0",
            "installer": format!("{base}/releases/download/v99.1.0/EternalMonitor-Setup.exe"),
            "sha256": "0".repeat(64),
        },
    })
    .to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 2 {
            line.clear();
        }
        let _ = answered.recv();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{manifest}",
            manifest.len()
        );
    });
    // Never checked, so the window checks as it opens; already announced,
    // so no notification.
    std::fs::write(
        dir.join("EternalMonitor/update-state.json"),
        serde_json::json!({ "notified": "99.1.0" }).to_string(),
    )
    .unwrap();
    SettingsFile {
        check_for_updates: true,
        feedback_ask_done: true,
        star_clicked: true,
        ..Default::default()
    }
    .save();
    std::env::set_var("ETERNAL_UPDATE_BASE", &base);
    let mut window = harness(shared, egui::vec2(1040.0, 720.0), 1.0);
    std::env::remove_var("ETERNAL_UPDATE_BASE");
    window
        .get_all_by_role_and_label(Role::Button, "Send feedback")
        .next()
        .expect("the sidebar button")
        .click();
    window.run_steps(4);
    window.get_by_label("Tell us how it's going");
    answer.send(()).unwrap();
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while window
        .query_by_label("EternalMonitor 99.1.0 is available")
        .is_none()
    {
        assert!(Instant::now() < deadline, "the check never finished");
        std::thread::sleep(std::time::Duration::from_millis(50));
        window.run_steps(1);
    }
    window.run_steps(4);
    assert!(
        window
            .query_by_role_and_label(Role::Dialog, "Update ready")
            .is_none(),
        "the popup must not open over Send feedback"
    );
    window.get_by_label("Close").click();
    wait_for_popup(&mut window);
    window
        .get_by_role_and_label(Role::Dialog, "Update ready")
        .get_by_label("EternalMonitor 99.1.0 is ready");
    results.extend_harness(&mut window);
}

/// The text the last frame copied, if any.
fn copied_text(harness: &Harness<'_, AnalyzerApp>) -> Option<String> {
    harness
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
}

/// The link the last frame opened, if any.
fn opened_url(harness: &Harness<'_, AnalyzerApp>) -> Option<String> {
    harness
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|command| match command {
            egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
            _ => None,
        })
}

fn settings_file(dir: &std::path::Path) -> SettingsFile {
    let text = std::fs::read_to_string(dir.join("EternalMonitor/settings.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// A long message never pushes the dialog's buttons out of the window, at
/// the smallest window size or the default one.
fn long_feedback_keeps_its_buttons_on_screen(
    shared: &SharedControl,
    results: &mut SnapshotResults,
) {
    let message = (1..=35)
        .map(|n| format!("Line {n}: the picture stutters when I drag a window."))
        .collect::<Vec<_>>()
        .join("\n");
    for (name, size) in [
        ("feedback-panel-long-minimum", egui::vec2(820.0, 560.0)),
        ("feedback-panel-long", egui::vec2(1040.0, 720.0)),
    ] {
        let mut window = harness(shared, size, 1.0);
        nav(&mut window, "Settings");
        window
            .get_all_by_role_and_label(Role::Button, "Send feedback")
            .next()
            .expect("the sidebar button")
            .click();
        window.run_steps(4);
        window.get_by_role(Role::MultilineTextInput).click();
        window.run_steps(2);
        window
            .get_by_role(Role::MultilineTextInput)
            .type_text(&message);
        window.run_steps(6);
        // The box keeps the caret in view as typing goes on.
        window.get_by_role(Role::MultilineTextInput).type_text("!");
        window.run_steps(6);
        let typed = window
            .get_by_role(Role::MultilineTextInput)
            .value()
            .unwrap_or_default();
        assert_eq!(typed.lines().count(), 35, "{typed}");
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        for label in [
            "Open in my email app",
            "Copy message",
            "Copy with logs",
            "Close",
        ] {
            let rect = window.get_by_role_and_label(Role::Button, label).rect();
            assert!(
                screen.contains_rect(rect),
                "{name}: {label} at {rect:?} is outside {screen:?}"
            );
        }
        window.get_by_label("Tell us how it's going");
        window.get_by_label("No email app? Write to").hover();
        window.run_steps(4);
        snapshot(&mut window, shared, name);
        results.extend_harness(&mut window);
    }
}

/// Send feedback, the one-time ask after three long sessions, and the GitHub
/// star card in the sidebar.
fn feedback_and_star(shared: &SharedControl, dir: &std::path::Path, results: &mut SnapshotResults) {
    let size = egui::vec2(1040.0, 720.0);
    let mut window = harness(shared, size, 1.0);
    let star = window.get_by_role_and_label(Role::Button, "Enjoying EternalMonitor?");
    // In the sidebar footer, above Send feedback.
    let feedback = window.get_by_role_and_label(Role::Button, "Send feedback");
    assert!(star.rect().right() <= 224.0 && star.rect().bottom() <= feedback.rect().top());
    snapshot(&mut window, shared, "sidebar-star");
    window
        .get_by_role_and_label(Role::Button, "Enjoying EternalMonitor?")
        .hover();
    window.run_steps(20);
    snapshot(&mut window, shared, "sidebar-star-hover");

    // Settings has Send feedback next to Source code. The panel opens over
    // Settings, where no random pairing code shows through.
    nav(&mut window, "Settings");
    let links: Vec<_> = window
        .get_all_by_role_and_label(Role::Button, "Send feedback")
        .collect();
    assert_eq!(links.len(), 2);
    window.get_by_label("Source code").scroll_to_me();
    window.run_steps(4);
    snapshot(&mut window, shared, "settings-about");

    // The panel: kind, message, details and the ways to send.
    window
        .get_all_by_role_and_label(Role::Button, "Send feedback")
        .find(|node| node.rect().left() > 224.0)
        .expect("the Settings link")
        .click();
    window.run_steps(4);
    window.get_by_label("Tell us how it's going");
    window.get_by_label("SEND FEEDBACK");
    window.get_by_label("Problem");
    window.get_by_label("Idea");
    window.get_by_label("No email app? Write to");
    window.get_by_label_contains("To include logs, click Copy with logs");
    window.get_by_label_contains("We're a small studio");
    // The details say what is sent: the setup, never the iPad's name.
    window.get_by_label_contains("Second screen, USB, iPad 2420 × 1668 at 120 Hz");
    window.get_by_label("Something I like").click();
    window.run_steps(4);
    window.get_by_label("Thank you! Hearing what works keeps a small studio going.");
    let text = window.get_by_role(Role::MultilineTextInput);
    text.click();
    window.run_steps(2);
    window
        .get_by_role(Role::MultilineTextInput)
        .type_text("Drawing on the iPad in Krita feels great.");
    window.get_by_label("No email app? Write to").hover();
    window.run_steps(4);
    snapshot(&mut window, shared, "feedback-panel");

    window.get_by_label("Copy message").click();
    window.step();
    let copied = copied_text(&window).expect("Copy message copies");
    assert!(copied.starts_with("To: hello@sideband.studio\nSubject: EternalMonitor 0.3.2 for Windows: Something I like"), "{copied}");
    assert!(copied.contains("Drawing on the iPad in Krita feels great."));
    assert!(copied.contains("Setup: Second screen, USB, iPad 2420 × 1668 at 120 Hz"));
    assert!(!copied.contains("Test iPad Pro") && !copied.contains("192.0.2.10"));
    window.run_steps(4);
    window.get_by_label("Copied. Paste it into an email to hello@sideband.studio");
    // Sending feedback any way means the one-time ask never shows.
    assert!(settings_file(dir).feedback_ask_done);

    window.get_by_label("Open in my email app").click();
    window.step();
    let url = opened_url(&window).expect("Open in my email app opens a mailto link");
    assert!(
        url.starts_with(
            "mailto:hello@sideband.studio?subject=EternalMonitor%200.3.2%20for%20Windows"
        ),
        "{url}"
    );
    assert!(url.contains("Krita") && url.len() <= eternal_host::feedback::MAILTO_LIMIT);

    // Without details, nothing about the PC or iPad goes in.
    window
        .get_by_role_and_label(Role::CheckBox, "Include app and PC details")
        .click();
    window.run_steps(4);
    assert!(window.query_by_label_contains("iPad 2420").is_none());
    window.get_by_label("Copy message").click();
    window.step();
    let copied = copied_text(&window).unwrap();
    // The subject keeps the kind and version, for sorting mail.
    assert!(
        copied.contains("Subject: EternalMonitor 0.3.2 for Windows: Something I like"),
        "{copied}"
    );
    assert!(
        !copied.contains("iPad 2420") && !copied.contains("Setup:") && !copied.contains("App:"),
        "{copied}"
    );
    // Copy with logs adds the session log after the message.
    window.get_by_label("Copy with logs").click();
    window.step();
    let copied = copied_text(&window).unwrap();
    assert!(copied.contains("Drawing on the iPad in Krita feels great."));
    window.run_steps(4);
    window.get_by_label_contains("Paste it into an email to hello@sideband.studio");
    window.run_steps(4);
    window.get_by_label("Close").click();
    window.run_steps(4);
    assert!(window.query_by_label("Tell us how it's going").is_none());

    nav(&mut window, "Stream");

    // One click opens the repository and thanks you, for good.
    window
        .get_by_role_and_label(Role::Button, "Enjoying EternalMonitor?")
        .click();
    window.step();
    assert_eq!(
        opened_url(&window).as_deref(),
        Some("https://github.com/sidebandstudio/EternalMonitor")
    );
    window.run_steps(4);
    window.get_by_role_and_label(Role::Button, "Thank you!");
    assert!(settings_file(dir).star_clicked);
    // Move the pointer away so the image shows the card at rest.
    window.get_by_label("Connected iPad").hover();
    window.run_steps(20);
    snapshot(&mut window, shared, "sidebar-star-thanks");
    results.extend_harness(&mut window);
    let reopened = harness(shared, size, 1.0);
    reopened.get_by_role_and_label(Role::Button, "Thank you!");
    assert!(reopened
        .query_by_role_and_label(Role::Button, "Enjoying EternalMonitor?")
        .is_none());

    // The one-time ask: after three sessions of five minutes, on Stream, only
    // while no iPad is connected.
    SettingsFile {
        check_for_updates: false,
        feedback_sessions: 3,
        ..Default::default()
    }
    .save();
    let streaming = harness(shared, size, 1.0);
    assert!(streaming
        .query_by_label("How's EternalMonitor working for you? :)")
        .is_none());
    let idle = SharedControl::new(9876, 15_000_000);
    idle.pairing.lock().configure(true, [0x11; 16]);
    let mut ask = harness(&idle, size, 1.0);
    ask.get_by_label("How's EternalMonitor working for you? :)");
    snapshot(&mut ask, &idle, "feedback-ask");
    results.extend_harness(&mut ask);
    ask.get_by_label("Not now").click();
    ask.run_steps(4);
    assert!(ask
        .query_by_label("How's EternalMonitor working for you? :)")
        .is_none());
    assert!(settings_file(dir).feedback_ask_done);
    let again = harness(&idle, size, 1.0);
    assert!(again
        .query_by_label("How's EternalMonitor working for you? :)")
        .is_none());
}

/// Renders every screen at the real window size for design review.
/// `EM_UI_REVIEW_DIR=/tmp/review cargo test -p eternal-host --test gui_snapshots -- --ignored`
#[test]
#[ignore = "writes review images; run explicitly"]
fn review_screens() {
    let out = std::path::PathBuf::from(
        std::env::var("EM_UI_REVIEW_DIR").unwrap_or_else(|_| "/tmp/eternal-ui-review".into()),
    );
    std::fs::create_dir_all(&out).unwrap();
    let (dir, shared) = fixture();
    let save = |harness: &mut Harness<'_, AnalyzerApp>, name: &str| {
        harness.run_steps(6);
        let image = harness.render().expect("render");
        image.save(out.join(format!("{name}.png"))).unwrap();
    };

    // Waiting for an iPad, at the default window size and on a tall page.
    for (name, size) in [
        ("ready", egui::vec2(1040.0, 720.0)),
        ("ready-full", egui::vec2(1040.0, 1100.0)),
    ] {
        let mut h = harness(&shared, size, 2.0);
        save(&mut h, name);
    }
    // No network, and pairing turned off.
    PIPELINE_STATS.lock().listen_addr = "unknown:9876".into();
    shared.pairing.lock().required = false;
    let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
    save(&mut h, "offline-pairing-off");
    PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
    shared.pairing.lock().required = true;

    // Streaming stopped.
    PIPELINE_STATS.lock().pipeline_running = false;
    let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
    save(&mut h, "stopped");
    PIPELINE_STATS.lock().pipeline_running = true;

    // An iPad on the cable while Apple Devices is missing, then closed.
    for (name, installed) in [
        ("usb-apple-devices-missing", false),
        ("usb-apple-devices-closed", true),
    ] {
        {
            let mut stats = PIPELINE_STATS.lock();
            stats.usb_service_reachable = false;
            stats.usb_cabled_devices = 1;
            stats.apple_devices_installed = installed;
        }
        let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
        save(&mut h, name);
    }
    {
        let mut stats = PIPELINE_STATS.lock();
        stats.usb_service_reachable = true;
        stats.usb_cabled_devices = 0;
    }

    // Second screen chosen, before any iPad connects.
    SettingsFile {
        check_for_updates: false,
        capture_display: Some(eternal_host::control::CAPTURE_VIRTUAL_SENTINEL.into()),
        ..Default::default()
    }
    .save();
    *shared.vdd_status.lock() = VddStatus::WaitingForClient;
    let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
    save(&mut h, "extend-waiting");
    // The display task failed, and the encoder fell back to the CPU.
    *shared.vdd_status.lock() = VddStatus::TaskFailed;
    PIPELINE_STATS.lock().using_software_fallback = true;
    let mut h = harness(&shared, egui::vec2(1040.0, 900.0), 2.0);
    save(&mut h, "problems-display-task-and-encoder");
    *shared.vdd_status.lock() = VddStatus::Failed;
    PIPELINE_STATS.lock().using_software_fallback = false;
    let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
    save(&mut h, "problem-display-timeout");
    // Why the second screen failed, as the driver tasks report it.
    for name in ["driver-not-responding", "driver-tasks-missing"] {
        std::env::set_var("ETERNAL_SIMULATE_PROBLEM", name);
        let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
        save(&mut h, &format!("problem-{name}"));
    }
    std::env::remove_var("ETERNAL_SIMULATE_PROBLEM");
    *shared.vdd_status.lock() = VddStatus::Inactive;
    SettingsFile {
        check_for_updates: false,
        ..Default::default()
    }
    .save();

    // Windows keeping the screen from the app, in each way it can.
    for (name, block) in [
        ("secure-desktop", CaptureBlock::SecureDesktop),
        ("display-changing", CaptureBlock::DisplayChanging),
        ("unsupported-gpu", CaptureBlock::UnsupportedOnThisGpu),
        ("too-many-capture-apps", CaptureBlock::TooManyCaptureApps),
        ("session-disconnected", CaptureBlock::SessionDisconnected),
    ] {
        *shared.capture_block.lock() = Some(block);
        let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
        save(&mut h, &format!("problem-{name}"));
    }
    *shared.capture_block.lock() = None;

    connect_ipad(&shared);
    let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
    save(&mut h, "streaming");
    h.get_by_role_and_label(Role::Button, "QR code").click();
    h.run_steps(4);
    PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
    save(&mut h, "qr");
    h.get_by_label("Done").click();
    nav(&mut h, "Performance");
    save(&mut h, "performance");
    let mut tall = harness(&shared, egui::vec2(1040.0, 1720.0), 2.0);
    nav(&mut tall, "Settings");
    save(&mut tall, "settings-full");
    let mut small = harness(&shared, egui::vec2(820.0, 560.0), 1.5);
    save(&mut small, "minimum-size");

    // The update popup with a note from latest.json, with no iPad
    // connected; then an iPad app older than 0.3.2 connects.
    let state = dir.join("EternalMonitor/update-state.json");
    let mut cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    cached["latest"]["note"] = "Fixes the second screen on laptops with two graphics cards.".into();
    cached["popup"] = serde_json::Value::Null;
    std::fs::write(&state, cached.to_string()).unwrap();
    SettingsFile {
        check_for_updates: true,
        ..Default::default()
    }
    .save();
    disconnect_ipad(&shared);
    let mut h = harness(&shared, egui::vec2(1040.0, 720.0), 2.0);
    wait_for_popup(&mut h);
    save(&mut h, "update-popup-note");
    h.get_by_role_and_label(Role::Dialog, "Update ready")
        .get_by_label("Later")
        .click();
    connect_app(&shared, "Test iPad Pro", 10, 20, 3);
    save(&mut h, "ipad-out-of-date");
    std::fs::remove_dir_all(dir).unwrap();

    // Website product shot: realistic names and a healthy link, default window size.
    let (dir, shared) = fixture();
    connect_named(&shared, "iPad Pro", 0, 4);
    PIPELINE_STATS.lock().listen_addr = "192.168.1.20:9876".into();
    let mut site = harness(&shared, egui::vec2(1000.0, 640.0), 2.0);
    save(&mut site, "website-host");
    std::fs::remove_dir_all(dir).unwrap();
}
