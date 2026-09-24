//! Inject only into the focused test window created by Test-PenInput.ps1.
#[cfg(windows)]
fn main() {
    use eternal_host::input::{CaptureGeometry, InputRelay};
    use eternal_wire::v2::control::InputEvent;
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).unwrap() };
    tracing_subscriber::fmt().init();
    let args: Vec<i32> = std::env::args()
        .skip(1)
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(
        args.len(),
        5,
        "expected probe PID, left, top, width, height"
    );
    std::env::set_var("ETERNAL_INPUT_WINDOW_PID", args[0].to_string());
    std::env::set_var("ETERNAL_INPUT_RECORDER_LOG", "1");
    let output = CaptureGeometry {
        left: args[1],
        top: args[2],
        width: args[3] as u32,
        height: args[4] as u32,
    };
    let mut relay = InputRelay::default();
    assert!(relay.pen_available(), "native pen creation failed");
    let mut event = InputEvent {
        input_ver: 2,
        kind: 1,
        phase: 1,
        buttons: 0,
        event_id: 1,
        x_norm: 10000,
        y_norm: 32768,
        pressure_x1000: 0,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: -40,
        tilt_y: 30,
    };
    relay.relay(1, &event, output);
    std::thread::sleep(std::time::Duration::from_millis(30));
    for step in 1..=10 {
        event.event_id += 1;
        event.phase = if step == 1 { 0 } else { 1 };
        event.buttons = 1;
        event.x_norm = 10000 + step * 3000;
        event.pressure_x1000 = step * 100;
        event.tilt_x = if step < 6 { -40 } else { 40 };
        relay.relay(1, &event, output);
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    event.event_id += 1;
    event.phase = 2;
    relay.relay(1, &event, output);
    std::thread::sleep(std::time::Duration::from_millis(30));
    event.event_id += 1;
    event.phase = 0;
    relay.relay(1, &event, output);
    // An entire coalesced batch arrives without sleeps. Every sample must
    // survive Windows' injection timestamp resolution.
    for _ in 0..32 {
        event.event_id += 1;
        event.phase = 1;
        relay.relay(1, &event, output);
    }
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        relay.keep_pen_alive();
    }
    relay.reset(); // disconnect mid-stroke must deliver a cancelled pen-up
    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Run scripts/win/Test-PenInput.ps1 on an interactive Windows desktop.");
}
