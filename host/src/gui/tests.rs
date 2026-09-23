use super::page_stream::{audio_summary, usb_summary};
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
}

#[test]
fn usb_row_shows_install_hint_only_when_the_service_is_unavailable() {
    let missing = usb_summary(false, 2, "Connected");
    assert!(!missing.available);
    assert_eq!(missing.badge, "Unavailable");
    assert!(missing
        .text
        .contains("Install the Apple Devices app from the Microsoft Store (or iTunes) to use USB"));

    let connected = usb_summary(true, 1, "Connected");
    assert!(connected.available);
    assert_eq!(connected.badge, "Connected");
    assert!(!connected.text.contains("Install"));

    // "Waiting for an iPad" must never become a whole label: UI Automation
    // reads that exact name as "no iPad connected".
    let idle = usb_summary(true, 0, "Waiting for an iPad");
    assert_eq!(idle.badge, "Ready");
    assert_ne!(idle.text, "Waiting for an iPad");
    assert!(idle.text.contains("Plug in your iPad"), "{}", idle.text);

    let app_closed = usb_summary(true, 1, "Waiting for the iPad app");
    assert!(
        app_closed.text.contains("Open EternalMonitor"),
        "{}",
        app_closed.text
    );

    let unknown = usb_summary(true, 2, "Waiting for direct test listener");
    assert_eq!(
        unknown.text,
        "Waiting for direct test listener · 2 devices plugged in"
    );
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
