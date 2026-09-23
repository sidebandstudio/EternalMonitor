#![cfg(target_os = "macos")]

use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::{kittest::Queryable, Harness};
use eternal_host::control::{GuiControl, SharedControl, VddStatus};
use eternal_host::gui::AnalyzerApp;
use eternal_host::settings::SettingsFile;
use eternal_host::stats::{PipelineStats, PIPELINE_STATS};
use eternal_host::transport::link::PeerId;
use eternal_host::transport::session::ConfigSource;
use eternal_wire::v2::control::{ControlMessage, Hello2, ReceiverReport, StreamConfig};

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
    SettingsFile {
        check_for_updates: false,
        ..Default::default()
    }
    .save();
    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        dir.join("EternalMonitor/release-check.json"),
        format!(r#"{{"checked_at":{checked_at},"tag":"v99.0.0"}}"#),
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
    let peer = PeerId::usb(7);
    let now = Instant::now();
    let mut session = shared.session.lock();
    let actions = session.handle_control(
        peer,
        0,
        ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 12,
            listen_port: 9877,
            decoder_caps: 3,
            feature_caps: 3,
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
    harness.get_by_label("120").click();
    harness.run_steps(4);
    assert_eq!(shared.target_fps.load(Ordering::SeqCst), 120);
    harness.get_by_label("BGRA").click();
    harness.run_steps(4);
    assert_eq!(
        *shared.encoder_input.lock(),
        eternal_host::encoder::input::EncoderInput::Bgra
    );
    // Encoder input applies on restart, so the page asks for one.
    harness.get_by_label("Restart the stream to apply your changes");
    snapshot(&mut harness, &shared, "settings");

    // General sits below the fold of the Settings page.
    harness
        .get_by_label("Check for updates daily")
        .scroll_to_me();
    harness.run_steps(4);
    harness.get_by_label("Check for updates daily").click();
    harness.run_steps(4);
    nav(&mut harness, "Stream");
    for _ in 0..30 {
        harness.run_steps(4);
        if harness.query_by_label("Dismiss").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    harness.get_by_label("EternalMonitor v99.0.0 is available");
    snapshot(&mut harness, &shared, "update-banner");
    harness.get_by_label("Dismiss").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Dismiss").is_none());

    harness.get_by_label("Restart now").click();
    harness.run_steps(4);
    assert!(harness
        .query_by_label("Restart the stream to apply your changes")
        .is_none());

    PIPELINE_STATS.lock().using_software_fallback = true;
    *shared.vdd_status.lock() = VddStatus::TaskFailed;
    harness.run_steps(4);
    harness.get_by_label("Hardware encoder unavailable. Encoding on the CPU (libx264).");
    harness.get_by_label("Extended display unavailable. Mirroring the main screen instead.");
    snapshot(&mut harness, &shared, "software-and-display-task-failure");
    std::fs::remove_dir_all(dir).unwrap();
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
    std::fs::remove_dir_all(dir).unwrap();

    // Website product shot: realistic names and a healthy link, default window size.
    let (dir, shared) = fixture();
    connect_named(&shared, "iPad Pro", 0, 4);
    PIPELINE_STATS.lock().listen_addr = "192.168.1.20:9876".into();
    let mut site = harness(&shared, egui::vec2(1000.0, 640.0), 2.0);
    save(&mut site, "website-host");
    std::fs::remove_dir_all(dir).unwrap();
}
