//! The order of `main`'s first steps matters and is easy to break in a merge:
//! a helper process must be dispatched before the single-instance check, and
//! a second launch must exit before it can rotate the running host's session
//! log, start a second uploader, reset the virtual display that host is
//! streaming, or start a failure reporter of its own.

#[test]
fn helpers_then_single_instance_then_the_display_reset() {
    let main = include_str!("../src/main.rs");
    let at = |step: &str| {
        main.find(step)
            .unwrap_or_else(|| panic!("main.rs no longer has {step:?}"))
    };
    let steps = [
        "eternal_host::startup::run_reporter_if_requested();",
        "eternal_host::single_instance::acquire()",
        // A launch that loses the instance check must not rotate the running
        // host's session log or start a second uploader on its outbox.
        "logging::MemoryLogWriter::start()",
        "eternal_host::telemetry::UploadGuard::start()",
        // Beside startup, holding the turn before anything else can ask.
        "vdd::disable_in_background();",
        "eternal_host::startup::install_failure_hooks();",
    ];
    let positions: Vec<usize> = steps.iter().map(|step| at(step)).collect();
    for pair in steps.iter().zip(&positions).collect::<Vec<_>>().windows(2) {
        let ((first, before), (second, after)) = (pair[0], pair[1]);
        assert!(before < after, "{first} must come before {second}");
    }
    // Nothing before the reporter dispatch may touch the display.
    assert!(
        main[..positions[0]].find("vdd::").is_none(),
        "main touches the virtual display before dispatching helper modes"
    );
    // The display reset no longer holds up startup.
    assert!(
        !main.contains("    vdd::disable();\n"),
        "main waits for the startup Disable again"
    );
}
