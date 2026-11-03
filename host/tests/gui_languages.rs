#![cfg(target_os = "macos")]
//! Settings and every first-run setup step in eight languages, driven
//! through the English AutomationIds (AccessKit `author_id`) that widgets
//! showing a translation keep in every language. On macOS the system fonts
//! stand in for the Windows ones (see `gui/fonts.rs`).
//!
//! Setup is drawn at the window's minimum size, 820 × 560, where it must
//! fit in every language.

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::kittest::{AccessKitNode, By, NodeT, Queryable};
use egui_kittest::{Harness, SnapshotResults};
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
    // Windows built on default settings check for updates; nothing answers.
    std::env::set_var("ETERNAL_UPDATE_BASE", "http://127.0.0.1:9");
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

/// A widget's name as UI Automation reads it: a label's text is its value.
fn name_of(node: &AccessKitNode<'_>) -> Option<String> {
    if node.role() == Role::Label {
        node.value()
    } else {
        node.label()
    }
}

/// The widget `name` names in English, as `scripts/win/Inspect-Host.ps1`
/// finds it: by its AutomationId, which whatever shows a translation has in
/// every language, else, for text never translated, by its name.
fn id(name: &str) -> By<'_> {
    By::new().predicate(move |node| match node.author_id() {
        Some(id) => id == name,
        None => name_of(node).as_deref() == Some(name),
    })
}

fn click(harness: &mut Harness<'_, AnalyzerApp>, role: Role, name: &str) {
    harness.get(id(name).role(role)).click();
    harness.run_steps(4);
}

/// One test: the windows share the settings folder, the pipeline figures
/// and the launch's fresh-install flag.
#[test]
fn languages_settings_and_setup() {
    let (dir, shared) = fixture();
    let mut results = SnapshotResults::new();
    settings_and_setup_in_each_language(&shared, &mut results);
    paused_card_and_pairing_off_in_long_languages(&shared, &mut results);
    stream_header_text_ends_before_its_buttons(&shared);
    upgrades_never_open_setup(&dir, &shared);
    a_fresh_install_opens_setup_and_a_language_applies_at_once(&dir, &shared);
    every_translation_keeps_an_english_id(&shared);
    every_switch_has_an_english_id(&shared);
    typed_text_stays_as_typed(&shared);
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
        // Every id below stays English whatever the window shows. The
        // language menu has its row's title as its id.
        for label in ["DISPLAY", "Frame rate", "Setup", "Video quality limit"] {
            settings.get(id(label));
        }
        settings.get(id("Language").role(Role::Label));
        let lang = eternal_host::i18n::Lang::from_code(code).unwrap();
        assert_combo(&settings, code, "Language", lang.native_name());
        let mirror = eternal_host::i18n::tr("Mirror my screen");
        let mirror = eternal_host::i18n::logical(mirror).unwrap_or(mirror);
        assert_combo(&settings, code, "How the iPad shows your PC", mirror);
        snapshot(&mut settings, &shared, &format!("lang-{code}-settings"));
        results.extend_harness(&mut settings);

        // Setup, at the minimum window size.
        let mut setup = harness(&shared, egui::vec2(820.0, 560.0));
        click(&mut setup, Role::Button, "Settings");
        setup.get(id("Run setup again")).scroll_to_me();
        setup.run_steps(4);
        click(&mut setup, Role::Button, "Run setup again");
        for (index, step) in STEPS.iter().enumerate() {
            // The pairing step looks up the LAN address, like the QR dialog.
            // Put the fixed fixture back for a deterministic image.
            PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
            if *step == "connect" {
                // USB brings up the Apple Devices check; this Mac has none.
                click(&mut setup, Role::RadioButton, "USB");
                setup.get(id("Get Apple Devices"));
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
            setup.query(id("Skip setup")).is_none(),
            "Done closes setup ({code})"
        );
        results.extend_harness(&mut setup);
    }
}

/// Too many wrong codes, as from iPads on the network, pause code pairing.
fn pause_code_pairing(shared: &SharedControl) {
    use eternal_host::transport::link::PeerId;
    use eternal_wire::v2::control::Hello2;

    let now = std::time::Instant::now();
    let mut pairing = shared.pairing.lock();
    let wrong = pairing.code() % 999_999 + 1;
    for n in 0..eternal_host::pairing::CODE_BUDGET as u32 {
        let peer = PeerId::udp(std::net::SocketAddr::from((
            [192, 0, 2, 100 + n as u8],
            50000,
        )));
        let hello = Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 900 + n,
            listen_port: 50000,
            decoder_caps: 3,
            feature_caps: 0,
            screen_px_w: 2420,
            screen_px_h: 1668,
            screen_pt_w: 1210,
            screen_pt_h: 834,
            refresh_hz: 120,
            device_name: "Unknown iPad".into(),
            device_id: 99,
            preferred_fps: 60,
            auth_token: [0; 16],
            pairing_code: wrong,
        };
        assert!(pairing.authorize(peer, &hello, now).is_err());
    }
    assert!(pairing.code_pairing_paused(now).is_some());
}

/// The Stream page's paused card, whose Resume button is about 30
/// characters in French, and Settings' pairing-off warning, in French and
/// Arabic: inside the window, and the text never under the button or the
/// next row.
fn paused_card_and_pairing_off_in_long_languages(
    shared: &SharedControl,
    results: &mut SnapshotResults,
) {
    let paused = "Too many wrong codes were typed, so code pairing is paused for 10 more minutes. The QR code and USB still work. Resuming gives a new code.";
    let warning = "Pairing is off, so any iPad on your network can connect, and use this PC's mouse and keyboard, without a code. Turn it on unless every device on this network is yours.";
    let size = egui::vec2(1040.0, 900.0);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    for code in ["fr", "ar"] {
        save_language(Some(code));
        pause_code_pairing(shared);
        let mut h = harness(shared, size);
        let text = h.get(id(paused)).rect();
        let resume = h.get(id("Resume code pairing").role(Role::Button)).rect();
        assert!(window.contains_rect(text), "{code}: {text:?}");
        assert!(window.contains_rect(resume), "{code}: {resume:?}");
        assert!(text.bottom() <= resume.top(), "{code}: {text:?} {resume:?}");
        snapshot(
            &mut h,
            shared,
            &format!("lang-{code}-stream-code-pairing-paused"),
        );
        results.extend_harness(&mut h);
        click(&mut h, Role::Button, "Resume code pairing");
        assert!(h.query(id("Resume code pairing")).is_none(), "{code}");

        let mut h = harness(shared, size);
        click(&mut h, Role::Button, "Settings");
        let switch = || id("Require pairing").role(Role::CheckBox);
        h.get(switch()).scroll_to_me();
        h.run_steps(4);
        click(&mut h, Role::CheckBox, "Require pairing");
        assert!(!shared.pairing.lock().required, "{code}");
        h.get(id(warning)).scroll_to_me();
        h.run_steps(4);
        let text = h.get(id(warning)).rect();
        let next = h.get(id("Forget all iPads").role(Role::Button)).rect();
        assert!(window.contains_rect(text), "{code}: {text:?}");
        assert!(text.bottom() <= next.top(), "{code}: {text:?} {next:?}");
        snapshot(&mut h, shared, &format!("lang-{code}-settings-pairing-off"));
        results.extend_harness(&mut h);
        click(&mut h, Role::CheckBox, "Require pairing");
        assert!(shared.pairing.lock().required, "{code}");
    }
}

/// The Stream page's header, at the window's minimum size, in every
/// language, with the stream on and off: its title and description end the
/// header's spacing before its first button, whose width is its translated
/// label's. macOS's Arabic stand-in font is narrower than Segoe UI, so in
/// Arabic the buttons took that spacing here where they covered the text
/// on Windows.
fn stream_header_text_ends_before_its_buttons(shared: &SharedControl) {
    const HEADER_SPACING: f32 = 14.0;
    let size = egui::vec2(820.0, 560.0);
    let mut under = Vec::new();
    for running in [true, false] {
        PIPELINE_STATS.lock().pipeline_running = running;
        let (title, description) = if running {
            (
                "Ready for your iPad",
                "Open EternalMonitor on your iPad and tap this PC, or plug in a USB cable.",
            )
        } else {
            (
                "Streaming is off",
                "Click Start streaming so your iPad can connect.",
            )
        };
        for lang in eternal_host::i18n::Lang::ALL {
            let code = lang.code();
            save_language(Some(code));
            let mut h = harness(shared, size);
            h.run_steps(4);
            let first = ["Start streaming", "Restart", "Stop"]
                .into_iter()
                .filter_map(|label| h.query(id(label).role(Role::Button)))
                .map(|button| button.rect())
                .min_by(|a, b| a.left().total_cmp(&b.left()))
                .unwrap_or_else(|| panic!("{code} running={running}: no header button"));
            for text in [title, description] {
                // "Streaming is off" is in the sidebar too, left of the page.
                let rect = h
                    .get_all(id(text))
                    .map(|node| node.rect())
                    .max_by(|a, b| a.left().total_cmp(&b.left()))
                    .unwrap();
                if rect.right() + HEADER_SPACING > first.left() {
                    under.push(format!(
                        "{code} running={running}: {text:?} {rect:?} is under or within 14 px of {first:?}"
                    ));
                }
            }
        }
    }
    PIPELINE_STATS.lock().pipeline_running = true;
    assert!(under.is_empty(), "{}", under.join("\n"));
}

/// A menu in a Settings row: named by the row's title as a screen reader
/// reads it, English in English, with the title's English as its id, and
/// the choice it shows, as written, as its value.
fn assert_combo(h: &Harness<'_, AnalyzerApp>, code: &str, title: &'static str, choice: &str) {
    let node = h.get(id(title).role(Role::ComboBox)).accesskit_node();
    let shown = eternal_host::i18n::tr(title);
    let written = eternal_host::i18n::logical(shown).unwrap_or(shown);
    assert_eq!(
        node.label().as_deref(),
        Some(written),
        "{code}: the {title:?} menu is named by its row's title"
    );
    assert_eq!(
        node.value().as_deref(),
        Some(choice),
        "{code}: the {title:?} menu's value is its choice"
    );
}

/// A settings folder with anything from an earlier run is an upgrade, which
/// never opens setup by itself, whatever became of settings.json.
fn upgrades_never_open_setup(dir: &std::path::Path, shared: &SharedControl) {
    use std::os::unix::fs::PermissionsExt;

    let profile = dir.join("EternalMonitor");
    let settings = profile.join("settings.json");
    let launch = |case: &str| {
        eternal_host::settings::note_launch();
        assert!(!eternal_host::settings::fresh_install(), "{case}");
        let mut h = harness(shared, egui::vec2(1040.0, 720.0));
        assert!(h.query(id("Skip setup")).is_none(), "{case}");
        click(&mut h, Role::Button, "Settings");
        h.get(id("Run setup again"));
    };
    let fresh_profile = || {
        let _ = std::fs::remove_dir_all(&profile);
        std::fs::create_dir_all(profile.join("logs")).unwrap();
    };

    // An older version that never saved its settings keeps its session log.
    fresh_profile();
    std::fs::write(profile.join("logs/session.log"), "earlier run\n").unwrap();
    launch("only logs");

    fresh_profile();
    std::fs::remove_dir(profile.join("logs")).unwrap();
    std::fs::write(profile.join("update-state.json"), "{}").unwrap();
    save_language(Some("de"));
    std::fs::remove_file(&settings).unwrap();
    launch("settings.json deleted");

    // Opening the window leaves a file it cannot read as it was.
    fresh_profile();
    std::fs::remove_dir(profile.join("logs")).unwrap();
    std::fs::write(&settings, "{ not json").unwrap();
    launch("malformed settings.json");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{ not json");

    fresh_profile();
    save_language(Some("de"));
    let saved = std::fs::read(&settings).unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o000)).unwrap();
    launch("unreadable settings.json");
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(std::fs::read(&settings).unwrap(), saved);
}

/// A fresh install, with no settings folder yet, opens setup at launch; a
/// language picked there applies at once, and the Stream page keeps its
/// English AutomationIds.
fn a_fresh_install_opens_setup_and_a_language_applies_at_once(
    dir: &std::path::Path,
    shared: &SharedControl,
) {
    let settings = dir.join("EternalMonitor/settings.json");
    std::fs::remove_dir_all(dir.join("EternalMonitor")).unwrap();
    eternal_host::settings::note_launch();
    assert!(eternal_host::settings::fresh_install());
    save_language(None);

    let mut h = harness(shared, egui::vec2(1040.0, 720.0));
    h.get_by_label("Skip setup");
    h.get_by_label("Welcome to EternalMonitor");
    click(&mut h, Role::RadioButton, "Deutsch");
    // The window draws and is named in German now; its ids stay English.
    assert_eq!(eternal_host::i18n::active(), eternal_host::i18n::Lang::De);
    assert_eq!(eternal_host::i18n::tr("Settings"), "Einstellungen");
    let welcome = h.get(id("Welcome to EternalMonitor"));
    assert_eq!(
        name_of(&welcome.accesskit_node()).as_deref(),
        Some(eternal_host::i18n::tr("Welcome to EternalMonitor"))
    );
    assert!(h.query_by_label("Welcome to EternalMonitor").is_none());
    let saved: SettingsFile =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(saved.language.as_deref(), Some("de"));

    click(&mut h, Role::Button, "Next");
    click(&mut h, Role::Button, "Back");
    h.get(id("Welcome to EternalMonitor"));
    click(&mut h, Role::Button, "Skip setup");
    assert!(h.query(id("Skip setup")).is_none());

    // scripts/win/Inspect-Host.ps1 finds these on a German window too.
    for label in [
        "Pairing",
        "USB connection",
        "PC audio",
        "Connected iPad",
        "Waiting for an iPad",
    ] {
        // "Waiting for an iPad" is in the sidebar and the iPad card.
        assert!(
            h.get_all(id(label)).next().is_some(),
            "{label} has its English id"
        );
    }
    h.get(id("Settings").role(Role::Button));
    h.get(id("QR code").role(Role::Button));

    // Mirror or second screen, from the Stream page, keeps working.
    h.get(id("Second screen")).click();
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
    let out = std::path::PathBuf::from(
        std::env::var("EM_UI_REVIEW_DIR").unwrap_or_else(|_| "/tmp/eternal-ui-review".into()),
    );
    std::fs::create_dir_all(&out).unwrap();
    let (dir, shared) = fixture();
    stream_to_an_ipad(&shared);
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

/// An iPad streaming over USB, with a receiver report for the metrics.
fn stream_to_an_ipad(shared: &SharedControl) {
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
}

/// Every page, the display menu and each setup step in German and Arabic,
/// with an iPad streaming: whatever shows a translation has an English id
/// and is named by the translation, in reading order, as a screen reader
/// says it; Arabic is drawn reordered (see `i18n/bidi.rs`).
fn every_translation_keeps_an_english_id(shared: &SharedControl) {
    use eternal_host::i18n::{bidi, tr, Lang};

    let english: std::collections::BTreeMap<String, String> =
        serde_json::from_str(include_str!("../src/i18n/strings/en.json")).unwrap();
    let keys: Vec<&'static str> = english
        .into_keys()
        .map(|key| &*Box::leak(key.into_boxed_str()))
        .collect();
    // The settings folder is in use now: an upgrade, without setup.
    eternal_host::settings::note_launch();
    stream_to_an_ipad(shared);

    for code in ["de", "ar"] {
        save_language(Some(code));
        let mut h = harness(shared, egui::vec2(1040.0, 1780.0));
        let lang = Lang::from_code(code).unwrap();
        // Key → (as drawn, as written), where the translation differs.
        let translations: Vec<(&str, &str, String)> = keys
            .iter()
            .filter_map(|key| {
                let shown = tr(key);
                let written = eternal_host::i18n::logical(shown).unwrap_or(shown);
                (shown != *key).then(|| (*key, shown, written.to_string()))
            })
            .collect();
        // Which translation `text` is, and whether it is in reading order.
        let translation = |text: &str| -> Option<bool> {
            translations.iter().find_map(|(_, shown, written)| {
                let matches = |form: &str| text == form || text == form.to_uppercase();
                if matches(written) {
                    Some(true)
                } else if matches(shown) {
                    Some(false)
                } else {
                    None
                }
            })
        };
        let check = |h: &Harness<'_, AnalyzerApp>, place: &str| {
            for node in h.root().children_recursive() {
                let node = node.accesskit_node();
                // What someone types is checked in `typed_text_stays_as_typed`.
                let typed = |node: &AccessKitNode<'_>| {
                    matches!(
                        node.role(),
                        Role::TextInput | Role::MultilineTextInput | Role::TextRun
                    )
                };
                if typed(&node) || node.parent().is_some_and(|parent| typed(&parent)) {
                    continue;
                }
                let Some(name) = name_of(&node) else { continue };
                let parts: Vec<Option<bool>> = name
                    .split('·')
                    .map(|part| translation(part.trim()))
                    .collect();
                if parts.iter().all(Option::is_none) {
                    // Never translated: English, or a value such as a name.
                    continue;
                }
                assert!(
                    !parts.contains(&Some(false)) || !lang.is_rtl(),
                    "{code} {place}: {name:?} is named in drawing order, not as written"
                );
                let id = node.author_id();
                assert!(
                    id.is_some_and(|id| id != name
                        && translation(id).is_none()
                        && !id.chars().any(bidi::is_arabic)),
                    "{code} {place}: {name:?} has no English id ({id:?})"
                );
            }
            // A switch is named by its row's title, as the title reads, so
            // it speaks the row's language.
            let labels: Vec<String> = h
                .query_all(By::new().role(Role::Label))
                .filter_map(|node| name_of(&node.accesskit_node()))
                .collect();
            for node in h.query_all(By::new().role(Role::CheckBox)) {
                let node = node.accesskit_node();
                let name = name_of(&node).unwrap_or_default();
                assert!(
                    labels.contains(&name),
                    "{code} {place}: switch {name:?} is not named by its row's title"
                );
            }
            // Whatever has a translated key as its id is named by the
            // translation; a combo box by its row's title.
            for (key, _, written) in &translations {
                let has_id = By::new().predicate(|node| node.author_id() == Some(key));
                for node in h.query_all(has_id) {
                    let node = node.accesskit_node();
                    assert_eq!(
                        name_of(&node).as_deref(),
                        Some(written.as_str()),
                        "{code} {place}: {key:?} is named by its translation"
                    );
                }
            }
        };

        h.run_steps(6);
        // The Connected iPad card and its figures, and text with values.
        for label in [
            "Connected iPad",
            "Decode rate",
            "Repaired fragments",
            &format!("Version {}", env!("CARGO_PKG_VERSION")),
        ] {
            h.get(id(label));
        }
        let streaming = h.get(id("Streaming to Test iPad Pro"));
        let written =
            eternal_host::i18n::trf("Streaming to {device}", &[("device", "Test iPad Pro")]);
        let written = if lang.is_rtl() {
            serde_json::from_str::<std::collections::BTreeMap<String, String>>(include_str!(
                "../src/i18n/strings/ar.json"
            ))
            .unwrap()["Streaming to {device}"]
                .replace(
                    "{device}",
                    &format!("{}Test iPad Pro{}", bidi::FSI, bidi::PDI),
                )
        } else {
            written
        };
        assert_eq!(
            name_of(&streaming.accesskit_node()),
            Some(written),
            "{code}: text with values is read as written"
        );
        check(&h, "Stream");
        click(&mut h, Role::Button, "Performance");
        check(&h, "Performance");
        click(&mut h, Role::Button, "Settings");
        // The display menu's choices.
        h.get(id("How the iPad shows your PC").role(Role::ComboBox))
            .click();
        h.run_steps(4);
        h.get(id("Mirror my screen"));
        h.get(id("Second screen"));
        check(&h, "Settings, display menu open");
        h.key_press(egui::Key::Escape);
        h.run_steps(4);
        // Settings' audio row and setup's show one translation.
        h.get(id("Stream PC audio").role(Role::Label));
        click(&mut h, Role::Button, "Run setup again");
        for (index, step) in STEPS.iter().enumerate() {
            h.get(id(&format!("STEP {} OF {}", index + 1, STEPS.len())));
            if *step == "connect" {
                click(&mut h, Role::RadioButton, "USB");
            }
            if *step == "show" {
                h.get(id("Play PC audio").role(Role::Label));
            }
            check(&h, &format!("setup step {}", index + 1));
            let next = if index + 1 == STEPS.len() {
                "Done"
            } else {
                "Next"
            };
            click(&mut h, Role::Button, next);
        }
    }
}

/// Every switch: its AutomationId, its name in English, and whether its
/// row's title is translated. "Start with Windows" shows in Settings on
/// Windows only, and in setup everywhere.
const SWITCHES: [(&str, &str, bool); 12] = [
    ("Pairing", "Require pairing", true),
    ("PC audio", "Stream PC audio", true),
    (
        "Include app and PC details",
        "Include app and PC details",
        false,
    ),
    (
        "Match the iPad's resolution",
        "Match the second screen to the iPad's resolution",
        true,
    ),
    ("Stream PC audio", "Stream PC audio", true),
    ("Require pairing", "Require pairing", true),
    (
        "Check for updates automatically",
        "Automatic update checks",
        true,
    ),
    (
        "Automatically share diagnostics",
        "Automatically share diagnostics",
        false,
    ),
    (
        "Sharper video format (HEVC)",
        "Prefer HEVC (H.265) when the iPad supports it",
        false,
    ),
    (
        "Smaller network packets",
        "Use smaller network packets",
        false,
    ),
    ("Start with Windows", "Start on Windows startup", true),
    ("Play PC audio", "Play PC audio", true),
];

/// In English, German and Arabic every switch on Stream, in Send feedback,
/// in Settings with Troubleshooting open and in setup has its English id.
/// A switch keeps its English name in English and in a row never
/// translated; otherwise it is named by its row's title, as written.
fn every_switch_has_an_english_id(shared: &SharedControl) {
    use eternal_host::i18n::{logical, tr};

    for code in ["en", "de", "ar"] {
        save_language(Some(code));
        let mut seen = std::collections::BTreeSet::new();
        let mut check = |h: &Harness<'_, AnalyzerApp>, place: &str| {
            for node in h.query_all(By::new().role(Role::CheckBox)) {
                let node = node.accesskit_node();
                let name = name_of(&node);
                let id = node.author_id();
                let Some(&(id, english, translated)) =
                    SWITCHES.iter().find(|(known, _, _)| Some(*known) == id)
                else {
                    panic!("{code} {place}: switch {name:?} has no English id ({id:?})");
                };
                let expected = if code == "en" || !translated {
                    english
                } else {
                    logical(tr(id)).unwrap_or(tr(id))
                };
                assert_eq!(
                    name.as_deref(),
                    Some(expected),
                    "{code} {place}: the {id:?} switch's name"
                );
                seen.insert(id);
            }
        };

        let mut h = harness(shared, egui::vec2(1040.0, 2400.0));
        h.run_steps(4);
        check(&h, "Stream");
        click(&mut h, Role::Button, "Send feedback");
        check(&h, "Send feedback");

        let mut h = harness(shared, egui::vec2(1040.0, 2400.0));
        click(&mut h, Role::Button, "Settings");
        h.get(id("Show options")).scroll_to_me();
        h.run_steps(4);
        click(&mut h, Role::Button, "Show options");
        check(&h, "Settings");
        h.get(id("Run setup again")).scroll_to_me();
        h.run_steps(4);
        click(&mut h, Role::Button, "Run setup again");
        for (index, _) in STEPS.iter().enumerate() {
            check(&h, &format!("setup step {}", index + 1));
            let next = if index + 1 == STEPS.len() {
                "Done"
            } else {
                "Next"
            };
            click(&mut h, Role::Button, next);
        }
        let missing: Vec<_> = SWITCHES
            .iter()
            .map(|(id, _, _)| *id)
            .filter(|id| !seen.contains(id))
            .collect();
        assert!(
            missing.is_empty(),
            "{code}: switches not found: {missing:?}"
        );
    }
}

/// What someone types in the feedback box stays exactly as typed, in the
/// field and in the text runs a screen reader reads it from, even when it
/// is a translation the window shows elsewhere ("Einstellungen" is German
/// for Settings), and the field has no id of its own.
fn typed_text_stays_as_typed(shared: &SharedControl) {
    let runs = |h: &Harness<'_, AnalyzerApp>| -> String {
        h.root()
            .children_recursive()
            .map(|node| node.accesskit_node())
            .filter(|node| node.role() == Role::TextRun)
            .filter_map(|node| node.value())
            .collect()
    };
    for code in ["de", "ar"] {
        save_language(Some(code));
        let mut h = harness(shared, egui::vec2(1040.0, 1000.0));
        click(&mut h, Role::Button, "Send feedback");
        h.get_by_role(Role::MultilineTextInput).click();
        h.run_steps(2);
        let mut typed = "Einstellungen Grüße, Ünïcödé — 日本語".to_string();
        h.get_by_role(Role::MultilineTextInput).type_text(&typed);
        h.run_steps(4);
        let field = h.get_by_role(Role::MultilineTextInput);
        assert_eq!(field.value().as_deref(), Some(typed.as_str()), "{code}");
        assert_eq!(field.accesskit_node().author_id(), None, "{code}");
        assert_eq!(runs(&h), typed, "{code}: the text runs read what was typed");
        if code == "ar" {
            // The Arabic for Settings, typed in the Arabic window. egui 0.36
            // builds text runs from the shaped glyphs, which for Arabic are
            // not the characters typed, so only the field's value is exact.
            let arabic = eternal_host::i18n::logical(eternal_host::i18n::tr("Settings")).unwrap();
            h.get_by_role(Role::MultilineTextInput)
                .type_text(&format!(" {arabic}"));
            h.run_steps(4);
            typed = format!("{typed} {arabic}");
            let field = h.get_by_role(Role::MultilineTextInput);
            assert_eq!(field.value().as_deref(), Some(typed.as_str()), "{code}");
        }
    }
}
