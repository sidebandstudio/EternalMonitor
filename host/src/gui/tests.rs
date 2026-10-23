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
