#![cfg(target_os = "macos")]
//! Settings and every first-run setup step in eight languages, driven
//! through the English automation names every language keeps. On macOS the
//! system fonts stand in for the Windows ones (see `gui/fonts.rs`).
//!
//! Setup is drawn at the window's minimum size, 820 × 560, where it must
//! fit in every language.

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::{kittest::Queryable, Harness, SnapshotResults};
use eternal_host::control::{GuiControl, SharedControl};
use eternal_host::gui::AnalyzerApp;
use eternal_host::settings::SettingsFile;
use eternal_host::stats::{PipelineStats, PIPELINE_STATS};

const LANGUAGES: [&str; 8] = ["en", "ar", "zh-Hans", "hi", "ja", "ko", "ru", "de"];
const STEPS: [&str; 5] = ["language", "startup", "connect", "show", "pair"];

fn fixture() -> (std::path::PathBuf, SharedControl) {
    let dir = std::env::temp_dir().join(format!("eternal-gui-languages-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("EternalMonitor")).unwrap();
    // This integration test never touches the user's real settings.
    std::env::set_var("APPDATA", &dir);
    let shared = SharedControl::new(9876, 15_000_000);
    shared.pairing.lock().configure(true, [0x11; 16]);
    let mut stats = PipelineStats::new();
    stats.listen_addr = "192.0.2.10:9876".into();
    stats.pipeline_running = true;
    // Apple's USB service is not running, so setup checks for Apple Devices.
    stats.usb_service_reachable = false;
    *PIPELINE_STATS.lock() = stats;
    (dir, shared)
}

fn save_language(language: Option<&str>) {
    SettingsFile {
        check_for_updates: false,
        language: language.map(str::to_string),
        ..Default::default()
    }
    .save()
    .unwrap();
}

fn harness(shared: &SharedControl, size: egui::Vec2) -> Harness<'static, AnalyzerApp> {
    let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
    let control = GuiControl {
        shared: shared.clone(),
        supervisor_tx,
    };
    let mut harness = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .with_theme(egui::Theme::Dark)
        .wgpu()
        .build_eframe(|cc| AnalyzerApp::new(cc, control));
    harness.run_steps(4);
    harness
}

/// The six random pairing digits are masked; finding them proves the
/// window shows the host's actual code.
fn snapshot(harness: &mut Harness<'_, AnalyzerApp>, shared: &SharedControl, name: &str) {
    harness.run_steps(6);
    let code = format!("{:06}", shared.pairing.lock().code());
    if let Some(node) = harness.query_by_label(&format!("{} {}", &code[..3], &code[3..])) {
        harness.mask(node.rect());
    }
    harness.snapshot(name);
}

fn click(harness: &mut Harness<'_, AnalyzerApp>, role: Role, name: &str) {
    harness.get_by_role_and_label(role, name).click();
    harness.run_steps(4);
}

/// One test: the windows share the settings folder, the pipeline figures
/// and the launch's fresh-install flag.
#[test]
fn languages_settings_and_setup() {
    let (dir, shared) = fixture();
    let mut results = SnapshotResults::new();
    settings_and_setup_in_each_language(&shared, &mut results);
    a_fresh_install_opens_setup_and_a_language_applies_at_once(&dir, &shared);
    std::fs::remove_dir_all(dir).unwrap();
    results.unwrap();
}

fn settings_and_setup_in_each_language(shared: &SharedControl, results: &mut SnapshotResults) {
    let shared = shared.clone();

    for code in LANGUAGES {
        save_language(Some(code));

        // Settings, whole page.
        let mut settings = harness(&shared, egui::vec2(1040.0, 1780.0));
        click(&mut settings, Role::Button, "Settings");
        // Every name below stays English whatever the window shows.
        for label in [
            "DISPLAY",
            "Frame rate",
            "Language",
            "Setup",
            "Video quality limit",
        ] {
            settings.get_by_label(label);
        }
        snapshot(&mut settings, &shared, &format!("lang-{code}-settings"));
        results.extend_harness(&mut settings);

        // Setup, at the minimum window size.
        let mut setup = harness(&shared, egui::vec2(820.0, 560.0));
        click(&mut setup, Role::Button, "Settings");
        setup.get_by_label("Run setup again").scroll_to_me();
        setup.run_steps(4);
        click(&mut setup, Role::Button, "Run setup again");
        for (index, step) in STEPS.iter().enumerate() {
            // The pairing step looks up the LAN address, like the QR dialog.
            // Put the fixed fixture back for a deterministic image.
            PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
            if *step == "connect" {
                // USB brings up the Apple Devices check; this Mac has none.
                click(&mut setup, Role::RadioButton, "USB");
                setup.get_by_label("Get Apple Devices");
            }
            snapshot(
                &mut setup,
                &shared,
                &format!("lang-{code}-setup-{}-{step}", index + 1),
            );
            let next = if index + 1 == STEPS.len() {
                "Done"
            } else {
                "Next"
            };
            click(&mut setup, Role::Button, next);
        }
        assert!(
            setup.query_by_label("Skip setup").is_none(),
            "Done closes setup ({code})"
        );
        results.extend_harness(&mut setup);
    }
}

/// A fresh install opens setup at launch; a language picked there applies
/// at once, and the Stream page keeps its English automation names.
fn a_fresh_install_opens_setup_and_a_language_applies_at_once(
    dir: &std::path::Path,
    shared: &SharedControl,
) {
    let settings = dir.join("EternalMonitor/settings.json");
    let _ = std::fs::remove_file(&settings);
    eternal_host::settings::note_launch();
    assert!(eternal_host::settings::fresh_install());
    save_language(None);

    let mut h = harness(shared, egui::vec2(1040.0, 720.0));
    h.get_by_label("Skip setup");
    h.get_by_label("Welcome to EternalMonitor");
    click(&mut h, Role::RadioButton, "Deutsch");
    // The window draws German now; its names stay English.
    assert_eq!(eternal_host::i18n::active(), eternal_host::i18n::Lang::De);
    assert_eq!(eternal_host::i18n::tr("Settings"), "Einstellungen");
    h.get_by_label("Welcome to EternalMonitor");
    let saved: SettingsFile =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(saved.language.as_deref(), Some("de"));

    click(&mut h, Role::Button, "Next");
    click(&mut h, Role::Button, "Back");
    h.get_by_label("Welcome to EternalMonitor");
    click(&mut h, Role::Button, "Skip setup");
    assert!(h.query_by_label("Skip setup").is_none());

    // scripts/win/Inspect-Host.ps1 reads these names on a German window too.
    for label in [
        "Pairing",
        "USB connection",
        "PC audio",
        "Connected iPad",
        "Waiting for an iPad",
    ] {
        // "Waiting for an iPad" is in the sidebar and the iPad card.
        assert!(
            h.get_all_by_label(label).next().is_some(),
            "{label} is named in English"
        );
    }
    h.get_by_role_and_label(Role::Button, "Settings");
    h.get_by_role_and_label(Role::Button, "QR code");

    // Mirror or second screen, from the Stream page, keeps working.
    h.get_by_label("Second screen").click();
    h.run_steps(4);
    assert_eq!(
        *shared.capture_target.lock(),
        eternal_host::control::CaptureTarget::VirtualExtended
    );
}

/// The Stream page with an iPad streaming, and the QR dialog, in each
/// language, for review. Not references; never compared.
/// `EM_UI_REVIEW_DIR=/tmp/review cargo test -p eternal-host --test gui_languages -- --ignored`
#[test]
#[ignore = "writes review images; run explicitly"]
fn review_stream_pages() {
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

    let out = std::path::PathBuf::from(
        std::env::var("EM_UI_REVIEW_DIR").unwrap_or_else(|_| "/tmp/eternal-ui-review".into()),
    );
    std::fs::create_dir_all(&out).unwrap();
    let (dir, shared) = fixture();
    {
        let mut stats = PIPELINE_STATS.lock();
        stats.capture_fps = 60.0;
        stats.capture_resolution = (1920, 1080);
        stats.usb_service_reachable = true;
        stats.usb_devices = 1;
        stats.usb_link_state = "Connected".into();
    }
    let peer = PeerId::usb(7);
    let now = std::time::Instant::now();
    {
        let mut session = shared.session.lock();
        session.handle_control(
            peer,
            0,
            ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 12,
                listen_port: 9877,
                decoder_caps: 3,
                feature_caps: 3 | eternal_wire::v2::control::FEATURE_CHECKS_UPDATES,
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
        let session_id = session.session_id().unwrap();
        session.handle_control(
            peer,
            session_id,
            ControlMessage::ReceiverReport(ReceiverReport {
                stream_epoch: 1,
                frags_received: 970,
                frags_lost: 10,
                frags_repaired: 20,
                decode_fps_x10: 599,
                e2e_latency_ms_x10: 184,
                rtt_ms_x10: 32,
                audio_buffer_ms: 60,
                ..Default::default()
            }),
            &Fixture,
            now,
        );
    }
    for code in LANGUAGES {
        save_language(Some(code));
        let mut h = harness(&shared, egui::vec2(1000.0, 640.0));
        h.run_steps(6);
        h.render()
            .unwrap()
            .save(out.join(format!("lang-{code}-stream.png")))
            .unwrap();
        click(&mut h, Role::Button, "QR code");
        PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
        h.run_steps(6);
        h.render()
            .unwrap()
            .save(out.join(format!("lang-{code}-qr.png")))
            .unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}
