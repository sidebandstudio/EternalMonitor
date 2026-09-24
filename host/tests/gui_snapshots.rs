#![cfg(target_os = "macos")]

use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
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
    // The production pairing code remains random. Only its six digits are
    // masked; the test asserts that AccessKit exposes the host's actual code.
    let code = format!("{:06}", shared.pairing.lock().code());
    if let Some(node) = harness.query_by_label(&code) {
        harness.mask(node.rect());
    }
    harness.snapshot(name);
}

#[test]
fn stream_settings_pairing_and_recovery_are_visible_and_operable() {
    let dir = std::env::temp_dir().join(format!("eternal-gui-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("EternalMonitor")).unwrap();
    // This integration test runs alone in its process and never uses real settings.
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
    let peer = PeerId::usb(7);
    let now = Instant::now();
    {
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
                device_name: "Test iPad Pro".into(),
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
                frags_lost: 10,
                frags_repaired: 20,
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
    *PIPELINE_STATS.lock() = stats;
    let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
    let control = GuiControl {
        shared: shared.clone(),
        supervisor_tx,
    };
    let mut harness = Harness::builder()
        .with_size(egui::vec2(960.0, 1300.0))
        .with_theme(egui::Theme::Dark)
        .wgpu()
        .build_eframe(|cc| AnalyzerApp::new(cc, control));
    harness.run_steps(4);
    for label in [
        "CONNECTED IPAD",
        "Test iPad Pro · USB",
        "1.00%",
        "2.00%",
        "59.9 fps",
        "3.2 ms",
        "18.4 ms",
        "60 ms",
        "PC AUDIO",
        "USB CONNECTION",
        "PC headphones",
        "Require pairing",
    ] {
        harness.get_by_label(label);
    }
    snapshot(&mut harness, &shared, "stream-connected");
    let old_code = shared.pairing.lock().code();
    harness.get_by_label("NEW CODE").click();
    harness.run_steps(4);
    assert_ne!(shared.pairing.lock().code(), old_code);
    harness.get_by_label("QR CODE").click();
    harness.run_steps(4);
    // QR opening refreshes the LAN address. Restore the fixed fixture after
    // exercising that action, before taking the deterministic image.
    PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
    harness.run_steps(4);
    harness.get_by_label("Scan this QR with the iPad camera to connect.");
    snapshot(&mut harness, &shared, "pairing-qr");
    harness.get_by_label("CLOSE").click();
    harness.run_steps(4);
    harness.get_by_label("PERFORMANCE").click();
    harness.run_steps(4);
    harness.get_by_label("Uptime");
    snapshot(&mut harness, &shared, "performance");
    harness.get_by_label("SETTINGS").click();
    harness.run_steps(4);
    harness.get_by_label("120").click();
    harness.run_steps(4);
    assert_eq!(shared.target_fps.load(Ordering::SeqCst), 120);
    harness.get_by_label("BGRA").click();
    harness.run_steps(4);
    assert_eq!(
        *shared.encoder_input.lock(),
        eternal_host::encoder::input::EncoderInput::Bgra
    );
    snapshot(&mut harness, &shared, "settings");
    harness.get_by_label("Check for updates daily").click();
    for _ in 0..30 {
        harness.run_steps(4);
        if harness.query_by_label("Dismiss").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    harness.get_by_label("EternalMonitor v99.0.0 is available. The iPad app must match.");
    snapshot(&mut harness, &shared, "update-banner");
    harness.get_by_label("Dismiss").click();
    harness.run_steps(4);
    assert!(harness.query_by_label("Dismiss").is_none());
    harness.get_by_label("STREAM").click();
    PIPELINE_STATS.lock().using_software_fallback = true;
    *shared.vdd_status.lock() = VddStatus::TaskFailed;
    harness.run_steps(4);
    harness.get_by_label("⚠ Hardware encoder unavailable — encoding on CPU (libx264)");
    snapshot(&mut harness, &shared, "software-and-display-task-failure");
    std::fs::remove_dir_all(dir).unwrap();
}
