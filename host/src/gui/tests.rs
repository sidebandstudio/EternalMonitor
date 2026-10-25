use super::page_stream::{
    audio_summary, capture_block_notice, second_screen_notice, usb_cable_notice, usb_summary,
};
use super::*;

#[test]
fn audio_row_reports_rate_and_keeps_failure_separate_from_video() {
    let mut audio = crate::audio::AudioStats::started("USB Audio Device");
    audio.kbps = 128.0;
    audio.packets_per_sec = 50.0;

    let (text, color) = audio_summary(&audio, true);
    assert!(text.contains("USB Audio Device"), "{text}");
    assert!(
        text.contains("128 kbps") && text.contains("50 packets/s"),
        "{text}"
    );
    assert_eq!(color, ACCENT);

    audio.stopped("Unavailable");
    audio.error = Some("Output was removed".into());
    let (text, color) = audio_summary(&audio, true);
    assert!(
        text.contains("Output was removed") && text.contains("Video continues"),
        "{text}"
    );
    assert!(!text.contains("128 kbps"), "{text}");
    assert_eq!(color, WARNING);

    audio.error = None;
    let (text, _) = audio_summary(&audio, false);
    assert!(text.starts_with("Off."), "{text}");

    let (text, color) = audio_summary(&crate::audio::AudioStats::default(), true);
    assert_eq!(text, "Starts when an iPad connects.");
    assert_eq!(color, TEXT_MUTED);
}

#[test]
fn usb_row_shows_install_hint_only_when_the_service_is_unavailable() {
    let missing = usb_summary(false, 2, "Connected", 0, false);
    assert!(!missing.available);
    assert_eq!(missing.badge, "Unavailable");
    assert!(missing
        .text
        .contains("Install the Apple Devices app from the Microsoft Store to use USB"));

    let connected = usb_summary(true, 1, "Connected", 0, false);
    assert!(connected.available);
    assert_eq!(connected.badge, "Connected");
    assert!(!connected.text.contains("Install"));

    // "Waiting for an iPad" must never become a whole label: UI Automation
    // reads that exact name as "no iPad connected".
    let idle = usb_summary(true, 0, "Waiting for an iPad", 0, false);
    assert_eq!(idle.badge, "Ready");
    assert_ne!(idle.text, "Waiting for an iPad");
    assert!(idle.text.contains("Plug in your iPad"), "{}", idle.text);

    let app_closed = usb_summary(true, 1, "Waiting for the iPad app", 0, false);
    assert!(
        app_closed.text.contains("Open EternalMonitor"),
        "{}",
        app_closed.text
    );

    let unknown = usb_summary(true, 2, "Waiting for direct test listener", 0, false);
    assert_eq!(
        unknown.text,
        "Waiting for direct test listener · 2 devices plugged in"
    );
}

#[test]
fn usb_row_says_to_open_or_install_apple_devices() {
    let closed = usb_summary(false, 0, "Apple device service unavailable", 0, true);
    assert!(!closed.available);
    assert!(
        closed.text.starts_with("Open Apple Devices"),
        "{}",
        closed.text
    );

    let plugged_closed = usb_summary(false, 0, "Apple device service unavailable", 1, true);
    assert!(
        plugged_closed
            .text
            .contains("iPad plugged in. Open Apple Devices"),
        "{}",
        plugged_closed.text
    );

    let plugged_missing = usb_summary(false, 0, "Apple device service unavailable", 1, false);
    assert!(
        plugged_missing.text.contains("not installed")
            && plugged_missing.text.contains("Microsoft Store"),
        "{}",
        plugged_missing.text
    );
}

#[test]
fn cable_notice_appears_only_for_a_plugged_in_ipad_without_the_service() {
    assert_eq!(usb_cable_notice(true, 1, true), None);
    assert_eq!(usb_cable_notice(false, 0, false), None);

    let (title, body) = usb_cable_notice(false, 1, true).unwrap();
    assert!(title.contains("Apple Devices is not running"), "{title}");
    assert!(body.contains("leave it running"), "{body}");

    let (title, body) = usb_cable_notice(false, 2, false).unwrap();
    assert!(title.contains("Apple Devices is not installed"), "{title}");
    assert!(body.contains("Microsoft Store"), "{body}");
}

#[test]
fn a_connected_ipad_without_a_picture_is_not_shown_as_streaming() {
    let mut snap = StatsSnapshot::take();
    snap.pipeline_running = true;
    let mut client = ClientSnapshot {
        info: Some(ClientInfo {
            device_name: "iPad".into(),
            device_id: 1,
            screen_px: (2388, 1668),
            refresh_hz: 120,
            preferred_fps: 60,
            decoder_caps: 1,
            feature_caps: 0,
            connected_at: Instant::now(),
        }),
        peer: None,
        session_id: None,
        report: None,
        capture_block: Some(CaptureBlock::UnsupportedOnThisGpu),
    };
    // Capture retries this forever while heartbeats stay current.
    assert!(StreamState::of(&snap, &client) == StreamState::Paused);
    let (_, title, body) = capture_block_notice(client.capture_block, true).unwrap();
    assert!(title.contains("graphics card"), "{title}");
    assert!(body.contains("Power saving"), "{body}");

    client.capture_block = None;
    assert!(StreamState::of(&snap, &client) == StreamState::Streaming);
    assert!(capture_block_notice(None, true).is_none());
}

#[test]
fn a_stopped_stream_does_not_promise_the_picture_will_come_back() {
    let block = Some(CaptureBlock::DisplayChanging);
    assert!(capture_block_notice(block, true).is_some());
    // Stop leaves the last block recorded, but nothing retries any more.
    assert!(capture_block_notice(block, false).is_none());
}

#[test]
fn codec_names_read_as_codec_and_engine() {
    assert_eq!(friendly_codec("h264_nvenc"), "H.264 · NVENC");
    assert_eq!(friendly_codec("hevc_amf"), "HEVC · AMF");
    assert_eq!(friendly_codec("h264_qsv"), "H.264 · Quick Sync");
    assert_eq!(friendly_codec("libx264"), "H.264 · CPU");
    assert_eq!(friendly_codec("libx265"), "HEVC · CPU");
    assert_eq!(friendly_codec("h264_videotoolbox"), "H.264 · VideoToolbox");
    assert_eq!(friendly_codec(""), "Detecting encoder");
    assert_eq!(friendly_codec("mystery"), "mystery");
}

#[test]
fn uptime_switches_to_hours_after_an_hour() {
    assert_eq!(format_uptime(0.0), "0m 00s");
    assert_eq!(format_uptime(75.4), "1m 15s");
    assert_eq!(format_uptime(3.0 * 3600.0 + 125.0), "3h 02m");
}

#[test]
fn state_colors_mix_toward_the_accent() {
    assert_eq!(mix(SURFACE, ACCENT, 0), SURFACE);
    assert_eq!(mix(SURFACE, ACCENT, 100), ACCENT);
    let halfway = mix(Color32::BLACK, Color32::WHITE, 50);
    assert_eq!((halfway.r(), halfway.g(), halfway.b()), (128, 128, 128));
}

#[test]
fn chart_steps_are_round_numbers() {
    for (value, step) in [
        (0.3, 0.5),
        (0.75, 1.0),
        (1.2, 2.0),
        (2.1, 2.5),
        (3.0, 5.0),
        (7.0, 10.0),
        (12.0, 20.0),
        (15.0, 20.0),
        (26.0, 50.0),
    ] {
        let got = widgets::nice_step(value);
        assert!((got - step).abs() < 1e-4, "{value} -> {got}, want {step}");
    }
}

#[test]
fn screen_choice_says_what_the_ipad_shows_and_that_it_reconnects() {
    use super::page_stream::screen_mode_text;
    use crate::control::VddStatus;

    let mirror = screen_mode_text("", None, VddStatus::Inactive, false);
    assert_eq!(
        mirror,
        "The iPad shows the same picture as your main screen."
    );
    let running = screen_mode_text("", None, VddStatus::Inactive, true);
    assert!(running.contains("reconnects by itself"), "{running}");

    let monitor = screen_mode_text(
        r"\\.\DISPLAY2",
        Some("DISPLAY2"),
        VddStatus::Inactive,
        false,
    );
    assert!(
        monitor.contains("DISPLAY2") && monitor.contains("Settings"),
        "{monitor}"
    );

    let waiting = screen_mode_text(
        CAPTURE_VIRTUAL_SENTINEL,
        None,
        VddStatus::WaitingForClient,
        true,
    );
    assert!(waiting.contains("as soon as it connects"), "{waiting}");
    let active = screen_mode_text(CAPTURE_VIRTUAL_SENTINEL, None, VddStatus::Active, true);
    assert!(active.contains("Drag windows"), "{active}");
    for failed in [VddStatus::Failed, VddStatus::TaskFailed] {
        let text = screen_mode_text(CAPTURE_VIRTUAL_SENTINEL, None, failed, true);
        assert!(text.contains("mirrored"), "{text}");
    }
    // No driver or protocol words on the main screen.
    for text in [mirror, running, monitor, waiting, active] {
        for jargon in ["VDD", "virtual display", "capture", "DXGI", "encoder"] {
            assert!(!text.contains(jargon), "{text}");
        }
    }
}

#[test]
fn connection_quality_is_one_plain_word() {
    assert_eq!(connection_quality(0, 0, 0, 30), "Measuring");
    assert_eq!(connection_quality(1000, 0, 0, 30), "Excellent");
    assert_eq!(connection_quality(1000, 0, 10, 300), "Good");
    assert_eq!(connection_quality(1000, 0, 40, 300), "Fair");
    assert_eq!(connection_quality(1000, 0, 200, 300), "Poor");
    assert_eq!(connection_quality(1000, 0, 0, 2000), "Poor");
}

#[test]
fn connection_rating_follows_the_latest_interval_not_the_session_total() {
    let report = |stream_epoch: u32, frags_received: u32, frags_lost: u32| ReceiverReport {
        stream_epoch,
        frags_received,
        frags_lost,
        rtt_ms_x10: 30,
        ..Default::default()
    };
    let mut meter = QualityMeter::default();
    assert_eq!(meter.update(None, None), "Measuring");
    assert_eq!(
        meter.update(Some(7), Some(&report(1, 1_000_000, 0))),
        "Excellent"
    );
    // 8,000 received and 2,000 lost since the last report is a 20% loss,
    // although the session total is still 0.2%.
    let bad = report(1, 1_008_000, 2_000);
    assert_eq!(meter.update(Some(7), Some(&bad)), "Poor");
    // The same report read again on the next frame keeps the rating.
    assert_eq!(meter.update(Some(7), Some(&bad)), "Poor");
    assert_eq!(
        meter.update(Some(7), Some(&report(1, 1_018_000, 2_000))),
        "Excellent"
    );
    // A host restart (new epoch) and a new session start from their own counters.
    assert_eq!(meter.update(Some(7), Some(&report(2, 900, 100))), "Poor");
    assert_eq!(
        meter.update(Some(7), Some(&report(2, 10_900, 100))),
        "Excellent"
    );
    assert_eq!(
        meter.update(Some(8), Some(&report(2, 9_000, 1_000))),
        "Poor"
    );
    // A quiet interval keeps the last word instead of "Measuring".
    assert_eq!(
        meter.update(Some(8), Some(&report(2, 9_000, 1_000))),
        "Poor"
    );
    let mut quiet = report(2, 9_000, 1_000);
    quiet.highest_seq = 1;
    assert_eq!(meter.update(Some(8), Some(&quiet)), "Poor");
    // Disconnecting forgets it.
    assert_eq!(meter.update(None, None), "Measuring");
}

#[test]
fn display_choices_use_the_names_shared_with_the_ipad() {
    assert_eq!(CAPTURE_AUTO_LABEL, "Mirror my screen");
    assert_eq!(CAPTURE_VIRTUAL_LABEL, "Second screen");
    for (_, body) in [
        usb_cable_notice(false, 1, true).unwrap(),
        usb_cable_notice(false, 1, false).unwrap(),
    ] {
        assert!(body.contains("Wi-Fi") && !body.contains("WiFi"), "{body}");
    }
}

#[test]
fn a_blocked_screen_is_explained_only_while_the_stream_runs() {
    use crate::capture::recovery::CaptureBlock;
    let blocks = [
        CaptureBlock::SecureDesktop,
        CaptureBlock::DisplayChanging,
        CaptureBlock::UnsupportedOnThisGpu,
        CaptureBlock::TooManyCaptureApps,
        CaptureBlock::SessionDisconnected,
        #[cfg(target_os = "macos")]
        CaptureBlock::ScreenRecordingDenied,
    ];
    assert!(capture_block_notice(None, true).is_none());
    let mut titles = Vec::new();
    for block in blocks {
        // Stopping the stream clears the notice with the condition.
        assert!(capture_block_notice(Some(block), false).is_none());
        let (_, title, body) = capture_block_notice(Some(block), true).unwrap();
        for jargon in ["DXGI", "duplication", "capture", "HRESULT", "GPU"] {
            assert!(
                !title.contains(jargon) && !body.contains(jargon),
                "{title} {body}"
            );
        }
        titles.push(title);
    }
    titles.dedup();
    assert_eq!(titles.len(), blocks.len());
    let (_, _, gpu) = capture_block_notice(Some(CaptureBlock::UnsupportedOnThisGpu), true).unwrap();
    assert!(
        gpu.contains("Settings > System > Display > Graphics"),
        "{gpu}"
    );
    assert!(gpu.contains("open it again"), "{gpu}");
    let (_, lock, _) = capture_block_notice(Some(CaptureBlock::SecureDesktop), true).unwrap();
    assert!(lock.contains("lock screen"), "{lock}");
}

#[test]
fn the_second_screen_notice_says_which_driver_problem_applies() {
    use crate::control::VddStatus;
    use crate::vdd::VddTaskHealth;
    for status in [
        VddStatus::Inactive,
        VddStatus::WaitingForClient,
        VddStatus::Attaching,
        VddStatus::Active,
    ] {
        for health in [
            VddTaskHealth::Ok,
            VddTaskHealth::NotResponding,
            VddTaskHealth::TasksMissing,
        ] {
            assert_eq!(second_screen_notice(status, health), None);
        }
    }
    for status in [VddStatus::Failed, VddStatus::TaskFailed] {
        // A hung driver needs a Windows restart; trying again cannot help.
        let hung = second_screen_notice(status, VddTaskHealth::NotResponding).unwrap();
        assert!(hung.body.contains("Restart the PC"), "{}", hung.body);
        assert!(!hung.retry && !hung.installer);
        // Missing driver tasks need the installer.
        let missing = second_screen_notice(status, VddTaskHealth::TasksMissing).unwrap();
        assert!(missing.body.contains("isn't installed"), "{}", missing.body);
        assert!(missing.retry && missing.installer);
    }
    // Without a known driver problem the earlier wording stays.
    let task = second_screen_notice(VddStatus::TaskFailed, VddTaskHealth::Ok).unwrap();
    assert!(task.body.starts_with("Part of the EternalMonitor install"));
    assert!(task.installer);
    let slow = second_screen_notice(VddStatus::Failed, VddTaskHealth::Ok).unwrap();
    assert!(slow
        .body
        .starts_with("Windows didn't add the second screen in time"));
    assert!(slow.retry && !slow.installer);
}

#[test]
fn the_update_popup_says_what_update_now_does_from_here() {
    use super::update_banner::popup_hint;
    assert_eq!(
        popup_hint(true),
        "Windows will ask for permission, then EternalMonitor restarts."
    );
    // A copy Setup would not replace only opens the website.
    assert_eq!(popup_hint(false), "Update now opens the download page.");
}

/// The popup never covers a picture the iPad is showing, or another dialog.
#[test]
fn the_update_popup_waits_for_the_stream_and_other_dialogs() {
    use super::update_banner::PopupGate::{self, *};
    // (open, connected, other dialog, on Stream, busy)
    assert_eq!(PopupGate::of(false, false, false, true, false), Show);
    assert_eq!(
        PopupGate::of(true, false, false, false, true),
        Show,
        "stays"
    );
    assert_eq!(PopupGate::of(false, false, false, false, false), Wait);
    assert_eq!(PopupGate::of(false, false, false, true, true), Wait);
    for open in [false, true] {
        for (connected, other) in [(true, false), (false, true), (true, true)] {
            assert_eq!(PopupGate::of(open, connected, other, true, false), Hide);
        }
    }
}

#[test]
fn feedback_details_leave_out_names_and_addresses() {
    use crate::feedback::{body, mailto, Kind};
    let client = ClientSnapshot {
        info: Some(ClientInfo {
            device_name: "Jane Doe's iPad".into(),
            device_id: 0x5EC2E7,
            screen_px: (2420, 1668),
            refresh_hz: 120,
            preferred_fps: 60,
            decoder_caps: 1,
            feature_caps: 0,
            connected_at: Instant::now(),
        }),
        peer: Some(PeerId::usb(7)),
        session_id: Some(3),
        report: None,
        capture_block: None,
    };
    let details = super::feedback_panel::details(true, &client);
    let text = body(Kind::Problem, "It froze", Some(&details));
    assert!(
        text.contains("Setup: Second screen, USB, iPad 2420 × 1668 at 120 Hz"),
        "{text}"
    );
    let (url, _) = mailto(Kind::Problem, "It froze", Some(&details));
    let mut private = vec![
        "Jane".to_string(),
        "iPad Pro".to_string(),
        "5EC2E7".to_string(),
        "192.".to_string(),
        "10.0.".to_string(),
        "USB device 7".to_string(),
        detect_local_ip(9876),
    ];
    // Whatever this machine's user and computer are called.
    for var in ["USER", "USERNAME", "COMPUTERNAME", "HOSTNAME", "LOGNAME"] {
        if let Ok(value) = std::env::var(var) {
            if value.len() > 2 {
                private.push(value);
            }
        }
    }
    for secret in &private {
        assert!(!text.contains(secret.as_str()), "{secret} in {text}");
        assert!(!url.contains(secret.as_str()), "{secret} in {url}");
    }
    // Disconnected: no iPad line at all.
    let idle = super::feedback_panel::details(
        false,
        &ClientSnapshot {
            info: None,
            peer: None,
            session_id: None,
            report: None,
            capture_block: None,
        },
    );
    assert!(body(Kind::Idea, "", Some(&idle)).contains("Setup: Mirror, no iPad connected"));
}

/// A session counts as nearby, and so does one that ended less than the
/// quiet period ago, so a reconnecting iPad never flashes the popup.
#[test]
fn the_update_popup_waits_until_no_ipad_has_been_connected_for_a_while() {
    use super::update_banner::{ipad_nearby, POPUP_QUIET};
    assert!(POPUP_QUIET >= Duration::from_secs(15) && POPUP_QUIET <= Duration::from_secs(30));
    let quiet = Duration::from_secs(20);
    let start = Instant::now();
    let mut last = None;
    assert!(
        !ipad_nearby(false, &mut last, start, quiet),
        "never connected"
    );
    assert!(ipad_nearby(true, &mut last, start, quiet));
    // Three drops and reconnects, a few seconds apart.
    for cycle in 1..=3u64 {
        let lost = start + Duration::from_secs(5 * cycle);
        assert!(ipad_nearby(true, &mut last, lost, quiet));
        assert!(ipad_nearby(
            false,
            &mut last,
            lost + Duration::from_millis(1),
            quiet
        ));
        assert!(ipad_nearby(
            false,
            &mut last,
            lost + Duration::from_secs(3),
            quiet
        ));
    }
    let gone = start + Duration::from_secs(15);
    assert!(ipad_nearby(
        false,
        &mut last,
        gone + quiet - Duration::from_millis(1),
        quiet
    ));
    assert!(!ipad_nearby(false, &mut last, gone + quiet, quiet));
}

#[cfg(target_os = "macos")]
#[test]
fn mac_second_screen_status_line_says_what_the_device_shows() {
    use super::mac_second_screen::{mode_label, status_line};
    use crate::virtual_display::SecondScreenStatus as S;
    let text = |on, status: &S, block, device, size, running| {
        status_line(on, status, block, device, size, running).text
    };
    let duo = Some("iPhone");
    let size = (2670, 1878);

    assert_eq!(
        text(true, &S::Active(7), None, duo, size, true),
        "Second screen: on the iPhone (2670×1878)"
    );
    // Before its first frame there is no size to show.
    assert_eq!(
        text(true, &S::Active(7), None, duo, (0, 0), true),
        "Second screen: on the iPhone"
    );
    // Kept through the grace period after the device left.
    assert!(text(true, &S::Active(7), None, None, size, true).contains("until it reconnects"));
    assert_eq!(
        text(true, &S::Mirroring, None, None, size, true),
        "Mirroring your main display (no device connected)"
    );
    assert_eq!(
        text(true, &S::NotCapturable(7), None, duo, size, true),
        "Mirroring: macOS didn't list the new display yet"
    );
    assert!(
        text(true, &S::Unavailable("x".into()), None, duo, size, true).starts_with("Mirroring:")
    );
    assert!(text(false, &S::Off, None, None, size, true).starts_with("Off."));
    assert!(text(true, &S::Mirroring, None, None, size, false).starts_with("On."));

    // Without Screen Recording nothing else matters, and only then is
    // System Settings offered.
    let denied = status_line(
        true,
        &S::Active(7),
        Some(CaptureBlock::ScreenRecordingDenied),
        duo,
        size,
        true,
    );
    assert_eq!(denied.text, "Screen Recording is off: open System Settings");
    assert!(denied.open_settings);
    assert!(!status_line(true, &S::Mirroring, None, None, size, true).open_settings);
    // Switched off, the old capture state is not news.
    assert!(
        !status_line(
            false,
            &S::Off,
            Some(CaptureBlock::ScreenRecordingDenied),
            None,
            size,
            true
        )
        .open_settings
    );

    assert_eq!(mode_label(true, &S::Active(7)), "Second screen");
    assert_eq!(mode_label(true, &S::NotCapturable(7)), "Mirroring");
    assert_eq!(mode_label(false, &S::Active(7)), "Test picture");
}
