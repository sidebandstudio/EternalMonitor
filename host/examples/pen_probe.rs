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
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "mapping" || mode == "stroke" {
        return placement(&mode);
    }
    if mode == "sequences" {
        return sequences();
    }
    if mode == "recreate" {
        return recreate();
    }
    if mode == "soak" {
        return soak();
    }
    if mode == "uipi" {
        return uipi();
    }
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
        relay.keep_pen_alive(Some(output));
    }
    relay.reset(); // disconnect mid-stroke must deliver a cancelled pen-up
    std::thread::sleep(std::time::Duration::from_millis(100));
}

/// Placement checks for scripts/win/Test-PenMapping.ps1, which covers one
/// whole monitor with its probe window and passes that monitor's rect.
///
/// `mapping <pid> <left> <top> <width> <height> <nx,ny>...` taps the pen, then
/// a touch, at each normalized point. `stroke <pid> <left> <top> <width>
/// <height> <steps>` draws one horizontal stroke across the centre, one step
/// every 20 ms, while the script changes the display layout underneath it.
#[cfg(windows)]
fn placement(mode: &str) {
    use eternal_host::input::{CaptureGeometry, InputRelay, KIND_PENCIL, KIND_TOUCH};
    use eternal_wire::v2::control::InputEvent;
    use std::time::Duration;
    let args: Vec<String> = std::env::args().skip(2).collect();
    assert!(
        args.len() >= 6,
        "expected PID, left, top, width, height and points or steps"
    );
    let number = |index: usize| args[index].parse::<i32>().unwrap();
    std::env::set_var("ETERNAL_INPUT_WINDOW_PID", &args[0]);
    std::env::set_var("ETERNAL_INPUT_RECORDER_LOG", "1");
    let output = CaptureGeometry {
        left: number(1),
        top: number(2),
        width: number(3) as u32,
        height: number(4) as u32,
    };
    let mut relay = InputRelay::default();
    assert!(relay.pen_available(), "native pen creation failed");
    let mut event = InputEvent {
        input_ver: 2,
        kind: KIND_PENCIL,
        phase: 0,
        buttons: 1,
        event_id: 0,
        x_norm: 0,
        y_norm: 0,
        pressure_x1000: 500,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: 0,
        tilt_y: 0,
    };
    let send = |relay: &mut InputRelay, event: &mut InputEvent, phase: u8, wait_ms: u64| {
        event.event_id += 1;
        event.phase = phase;
        relay.relay(1, event, output);
        std::thread::sleep(Duration::from_millis(wait_ms));
    };
    if mode == "mapping" {
        let points: Vec<(u16, u16)> = args[5..]
            .iter()
            .map(|point| {
                let (x, y) = point.split_once(',').expect("points are nx,ny");
                (x.parse().unwrap(), y.parse().unwrap())
            })
            .collect();
        for &(x, y) in &points {
            (event.x_norm, event.y_norm) = (x, y);
            send(&mut relay, &mut event, 0, 60);
            send(&mut relay, &mut event, 1, 60);
            send(&mut relay, &mut event, 2, 150);
        }
        event.input_ver = 1;
        event.kind = KIND_TOUCH;
        event.pressure_x1000 = 0;
        for &(x, y) in &points {
            (event.x_norm, event.y_norm) = (x, y);
            send(&mut relay, &mut event, 0, 60);
            // A second click at the same spot within the double-click time
            // is still a separate button-down for the probe.
            send(&mut relay, &mut event, 2, 600);
        }
    } else {
        let steps = number(5).max(2) as u16;
        event.y_norm = 32_768;
        for step in 0..steps {
            event.x_norm = 6_000 + step * (54_000 / (steps - 1));
            send(&mut relay, &mut event, if step == 0 { 0 } else { 1 }, 20);
        }
        send(&mut relay, &mut event, 2, 150);
    }
    relay.reset();
    std::thread::sleep(Duration::from_millis(100));
}

/// `sequences <pid> <left> <top> <width> <height>` sends raw frame sequences
/// to the centre of the probe window, each on a fresh pen device, and prints
/// what Windows returns for every frame. It records which sequences Windows
/// rejects: those the 0.3.1 injector could send after a lost datagram, and a
/// contact with zero pressure. It bypasses the relay and the probe guard, so
/// every frame stays within a few pixels of the window centre.
#[cfg(windows)]
fn sequences() {
    use windows::Win32::UI::Controls::{
        CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, POINTER_FEEDBACK_NONE,
    };
    use windows::Win32::UI::Input::Pointer::{
        POINTER_FLAGS, POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT,
        POINTER_FLAG_INRANGE, POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::PT_PEN;
    let args: Vec<i32> = std::env::args()
        .skip(2)
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(args.len(), 5, "expected PID, left, top, width, height");
    let (cx, cy) = (args[1] + args[3] / 2, args[2] + args[4] / 2);
    let contact = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON;
    let down = POINTER_FLAG_DOWN | contact;
    let moved = POINTER_FLAG_UPDATE | contact;
    let hover = POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE;
    // (flags, x offset from the centre, pressure)
    type Frame = (POINTER_FLAGS, i32, u32);
    let cases: [(&str, &[Frame]); 8] = [
        (
            "control_stroke",
            &[(down, 0, 512), (moved, 0, 512), (POINTER_FLAG_UP, 0, 0)],
        ),
        (
            "control_hover_then_down_elsewhere",
            &[(hover, 0, 0), (down, 5, 512), (POINTER_FLAG_UP, 5, 0)],
        ),
        (
            "down_zero_pressure",
            &[(down, 0, 0), (moved, 0, 0), (POINTER_FLAG_UP, 0, 0)],
        ),
        ("leave_without_hover", &[(POINTER_FLAG_UPDATE, 0, 0)]),
        ("up_without_contact", &[(POINTER_FLAG_UP, 0, 0)]),
        (
            "update_contact_without_down",
            &[(moved, 0, 512), (POINTER_FLAG_UP, 0, 0)],
        ),
        (
            "down_while_in_contact",
            &[(down, 0, 512), (down, 5, 512), (POINTER_FLAG_UP, 5, 0)],
        ),
        (
            "up_away_from_last_update",
            &[(down, 0, 512), (POINTER_FLAG_UP, 5, 0)],
        ),
    ];
    for (name, frames) in cases {
        let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) }
            .expect("native pen creation failed");
        for (index, &(flags, dx, pressure)) in frames.iter().enumerate() {
            let result = inject_raw(device, flags, (cx + dx, cy), pressure);
            println!("SEQ {name} {index} {result}");
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        unsafe { DestroySyntheticPointerDevice(device) };
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// `recreate <pid> <left> <top> <width> <height>` holds a contact at the
/// centre of the probe window, destroys the pen device without an up, then
/// draws a stroke on a new device 20 px to the right. The host recreates
/// its device to end a pointer it cannot end with a frame; the placement
/// check confirms Windows ends that contact and the new device works.
#[cfg(windows)]
fn recreate() {
    use windows::Win32::UI::Controls::{
        CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, POINTER_FEEDBACK_NONE,
    };
    use windows::Win32::UI::Input::Pointer::{
        POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
        POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::PT_PEN;
    let args: Vec<i32> = std::env::args()
        .skip(2)
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(args.len(), 5, "expected PID, left, top, width, height");
    let (cx, cy) = (args[1] + args[3] / 2, args[2] + args[4] / 2);
    let contact = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON;
    let create = || {
        unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) }
            .expect("native pen creation failed")
    };
    let step = |name: &str, result: String| {
        println!("REC {name} {result}");
        std::thread::sleep(std::time::Duration::from_millis(40));
    };
    let held = create();
    step(
        "down",
        inject_raw(held, POINTER_FLAG_DOWN | contact, (cx, cy), 512),
    );
    for dx in 1..=3 {
        step(
            "move",
            inject_raw(held, POINTER_FLAG_UPDATE | contact, (cx + dx, cy), 512),
        );
    }
    unsafe { DestroySyntheticPointerDevice(held) };
    step("destroyed", "ok".to_string());
    std::thread::sleep(std::time::Duration::from_millis(300));
    let fresh = create();
    let at = (cx + 20, cy);
    step(
        "down",
        inject_raw(fresh, POINTER_FLAG_DOWN | contact, at, 512),
    );
    step(
        "move",
        inject_raw(fresh, POINTER_FLAG_UPDATE | contact, at, 512),
    );
    step("up", inject_raw(fresh, POINTER_FLAG_UP, at, 0));
    unsafe { DestroySyntheticPointerDevice(fresh) };
    std::thread::sleep(std::time::Duration::from_millis(150));
}

/// `soak <pid> <left> <top> <width> <height> <seconds> <pattern> [options]`
/// draws strokes near the centre of the probe window for `seconds` on one
/// device and prints a `SOAK` line for every frame Windows refuses, with the
/// timing and geometry that led up to it, and a summary every ten seconds.
///
/// Patterns: `paced` sends one sample every 4 ms; `batch` four back to back
/// every 16 ms, as the iPad sends coalesced samples; `burst` 64 back to back
/// every 250 ms, as after a Wi-Fi stall; `same` 32 identical moves back to
/// back; `load` alternates `batch` and `burst` strokes with every CPU busy.
/// Options: `stamp` sets each frame's PerformanceCount instead of letting
/// Windows stamp it; `spacing_us=N` waits N microseconds between frames of
/// a batch; `recover=retry` answers a refused move by sending the same move
/// again each millisecond for up to 50 ms before starting a new contact;
/// `relay` sends the same samples through the host's `InputRelay` instead,
/// so the probe's counts show what the product delivers.
/// Otherwise recovery follows the host: ERROR_NOT_READY is retried, and a
/// refused move starts a new contact at the same point.
#[cfg(windows)]
fn soak() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use windows::Win32::UI::Controls::{
        CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, POINTER_FEEDBACK_NONE,
    };
    use windows::Win32::UI::Input::Pointer::{
        POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
        POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, PT_PEN,
    };
    let args: Vec<String> = std::env::args().skip(2).collect();
    assert!(
        args.len() >= 7,
        "expected PID, left, top, width, height, seconds and pattern"
    );
    let number = |index: usize| args[index].parse::<i64>().unwrap();
    let pid = number(0) as u32;
    let (cx, cy) = (
        (number(1) + number(3) / 2) as i32,
        (number(2) + number(4) / 2) as i32,
    );
    let seconds = number(5) as u64;
    let pattern = args[6].clone();
    let options = &args[7..];
    let stamp = options.iter().any(|option| option == "stamp");
    let option = |name: &str| {
        options
            .iter()
            .find_map(|option| option.strip_prefix(name)?.strip_prefix('='))
    };
    let spacing = Duration::from_micros(option("spacing_us").map_or(0, |n| n.parse().unwrap()));
    let retry = option("recover") == Some("retry");
    if options.iter().any(|option| option == "relay") {
        let bounds = (
            number(1) as i32,
            number(2) as i32,
            number(3) as u32,
            number(4) as u32,
        );
        return relay_soak(pid, bounds, seconds, &pattern, spacing);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let hogs: Vec<_> = if pattern == "load" {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        (0..cpus)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        x = std::hint::black_box(
                            x.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let foreground_is_probe = || unsafe {
        let mut owner = 0;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut owner));
        owner == pid
    };
    let contact = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON;
    let down = POINTER_FLAG_DOWN | contact;
    let moved = POINTER_FLAG_UPDATE | contact;
    let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) }
        .expect("native pen creation failed");
    let mut soak = Soak::new(device, stamp, retry);
    println!(
        "SOAK options pattern={pattern} stamp={stamp} spacing_us={} recover_retry={retry}",
        spacing.as_micros()
    );
    let started = Instant::now();
    let mut stroke = 0u64;
    let mut away_since: Option<Instant> = None;
    let mut next_summary = Duration::from_secs(10);
    while started.elapsed() < Duration::from_secs(seconds) {
        if started.elapsed() >= next_summary {
            soak.summary(&pattern, started.elapsed());
            next_summary += Duration::from_secs(10);
        }
        // Inject only while the probe has focus; give up if it never returns.
        if !foreground_is_probe() {
            let since = *away_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(20) {
                println!("SOAK aborted reason=probe_lost_focus");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        away_since = None;
        stroke += 1;
        let (batch, gap) = match pattern.as_str() {
            "paced" => (1, 4),
            "batch" => (4, 16),
            "burst" => (64, 250),
            "same" => (32, 250),
            _ if stroke.is_multiple_of(2) => (64, 250),
            _ => (4, 16),
        };
        let samples = if batch == 1 { 128 } else { (batch * 4).max(32) };
        let mut x = cx - 64;
        let mut pressure = 300;
        soak.frame(&pattern, down, (x, cy), pressure, stroke, 0, 0);
        let mut index = 1;
        while index < samples {
            for burst_pos in 0..batch {
                if pattern != "same" {
                    x = cx - 64 + (index % 128) as i32;
                    pressure = 300 + (index % 7) * 100;
                }
                soak.frame(&pattern, moved, (x, cy), pressure, stroke, index, burst_pos);
                index += 1;
                let sent = Instant::now();
                while sent.elapsed() < spacing {
                    std::hint::spin_loop();
                }
            }
            std::thread::sleep(Duration::from_millis(gap));
        }
        soak.frame(&pattern, POINTER_FLAG_UP, (x, cy), 0, stroke, index, 0);
        std::thread::sleep(Duration::from_millis(30));
    }
    soak.summary(&pattern, started.elapsed());
    stop.store(true, Ordering::Relaxed);
    for hog in hogs {
        let _ = hog.join();
    }
    unsafe { DestroySyntheticPointerDevice(device) };
}

/// The soak's strokes as iPad Pencil events through `InputRelay`, guarded
/// like the other relay checks, with `spacing` between events of a batch.
#[cfg(windows)]
fn relay_soak(
    pid: u32,
    (left, top, width, height): (i32, i32, u32, u32),
    seconds: u64,
    pattern: &str,
    spacing: std::time::Duration,
) {
    use eternal_host::input::{CaptureGeometry, InputRelay, KIND_PENCIL};
    use eternal_wire::v2::control::InputEvent;
    use std::time::{Duration, Instant};
    std::env::set_var("ETERNAL_INPUT_WINDOW_PID", pid.to_string());
    let output = CaptureGeometry {
        left,
        top,
        width,
        height,
    };
    let mut relay = InputRelay::default();
    assert!(relay.pen_available(), "native pen creation failed");
    let mut event = InputEvent {
        input_ver: 2,
        kind: KIND_PENCIL,
        phase: 0,
        buttons: 1,
        event_id: 0,
        x_norm: 0,
        y_norm: 32_768,
        pressure_x1000: 300,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: 0,
        tilt_y: 0,
    };
    // Samples sweep 128 pixels left to right around the centre.
    let norm = |offset: u32| ((width / 2 - 64 + offset % 128) * 65_535 / (width - 1)) as u16;
    let started = Instant::now();
    let (mut strokes, mut samples_sent) = (0u64, 0u64);
    while started.elapsed() < Duration::from_secs(seconds) {
        strokes += 1;
        let (batch, gap) = match pattern {
            "paced" => (1, 4),
            "batch" => (4, 16),
            "burst" => (64, 250),
            "same" => (32, 250),
            _ if strokes.is_multiple_of(2) => (64, 250),
            _ => (4, 16),
        };
        let samples = if batch == 1 { 128 } else { (batch * 4).max(32) };
        let mut send = |phase: u8, offset: u32, pressure: u16| {
            event.event_id += 1;
            event.phase = phase;
            event.x_norm = norm(if pattern == "same" { 0 } else { offset });
            event.pressure_x1000 = pressure;
            relay.relay(1, &event, output);
        };
        send(0, 0, 300);
        let mut index = 1;
        while index < samples {
            for _ in 0..batch {
                send(1, index, 300 + (index % 7) as u16 * 100);
                index += 1;
                let sent = Instant::now();
                while sent.elapsed() < spacing {
                    std::hint::spin_loop();
                }
            }
            std::thread::sleep(Duration::from_millis(gap));
        }
        send(2, index - 1, 0);
        samples_sent += u64::from(index) + 1;
        std::thread::sleep(Duration::from_millis(30));
    }
    relay.reset();
    std::thread::sleep(Duration::from_millis(100));
    println!(
        "SOAK relay summary pattern={pattern} seconds={} strokes={strokes} samples={samples_sent}",
        started.elapsed().as_secs()
    );
}

/// `uipi <pid> <left> <top> <width> <height>`, run without elevation while
/// the probe window runs as administrator: prints what Windows answers a
/// raw pen stroke and `SendInput` with (and its last error), then sends a
/// Pencil stroke, a tap and a key through the host's relay and prints which
/// app the host found Windows keeping its input from.
#[cfg(windows)]
fn uipi() {
    use eternal_host::input::{
        privilege, CaptureGeometry, InputRelay, KIND_KEY, KIND_PENCIL, KIND_TOUCH,
    };
    use eternal_wire::v2::control::InputEvent;
    use std::time::Duration;
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::UI::Controls::{
        CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, POINTER_FEEDBACK_NONE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    };
    use windows::Win32::UI::Input::Pointer::{
        POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
        POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::PT_PEN;
    let args: Vec<i32> = std::env::args()
        .skip(2)
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(args.len(), 5, "expected PID, left, top, width, height");
    let (cx, cy) = (args[1] + args[3] / 2, args[2] + args[4] / 2);
    let contact = POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON;
    let device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) }
        .expect("native pen creation failed");
    for (name, flags) in [
        ("down", POINTER_FLAG_DOWN | contact),
        ("move", POINTER_FLAG_UPDATE | contact),
        ("up", POINTER_FLAG_UP),
    ] {
        println!(
            "UIPI raw_pen {name} {}",
            inject_raw(device, flags, (cx, cy), 512)
        );
        std::thread::sleep(Duration::from_millis(40));
    }
    unsafe { DestroySyntheticPointerDevice(device) };
    let screen = eternal_host::input::windows_inject::virtual_screen();
    let (ax, ay) = eternal_host::input::desktop_pixel_to_abs(cx, cy, screen);
    let moved = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: i32::from(ax),
                dy: i32::from(ay),
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ..Default::default()
            },
        },
    };
    let sent = unsafe { SendInput(&[moved], std::mem::size_of::<INPUT>() as i32) };
    let error = unsafe { GetLastError() };
    println!("UIPI raw_sendinput_move sent={sent} last_error={}", error.0);
    std::thread::sleep(Duration::from_millis(100));

    std::env::set_var("ETERNAL_INPUT_WINDOW_PID", args[0].to_string());
    std::env::set_var("ETERNAL_INPUT_RECORDER_LOG", "1");
    let output = CaptureGeometry {
        left: args[1],
        top: args[2],
        width: args[3] as u32,
        height: args[4] as u32,
    };
    let mut relay = InputRelay::default();
    let mut event = InputEvent {
        input_ver: 2,
        kind: KIND_PENCIL,
        phase: 0,
        buttons: 1,
        event_id: 0,
        x_norm: 32_768,
        y_norm: 32_768,
        pressure_x1000: 500,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: 0,
        tilt_y: 0,
    };
    let send = |relay: &mut InputRelay, event: &mut InputEvent, phase: u8| {
        event.event_id += 1;
        event.phase = phase;
        relay.relay(1, event, output);
        std::thread::sleep(Duration::from_millis(30));
    };
    for phase in [0, 1, 1, 1, 2] {
        send(&mut relay, &mut event, phase);
    }
    println!("UIPI after_pen blocked_by={:?}", privilege::blocked_by());
    // The mouse fallback keeps refused input for a second, then drops it.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(50));
        relay.keep_pen_alive(Some(output));
    }
    event.input_ver = 1;
    event.kind = KIND_TOUCH;
    event.pressure_x1000 = 0;
    send(&mut relay, &mut event, 0);
    send(&mut relay, &mut event, 2);
    event.kind = KIND_KEY;
    event.keycode = 0x04; // HID 'a'
    send(&mut relay, &mut event, 0);
    send(&mut relay, &mut event, 2);
    println!(
        "UIPI after_touch_and_key blocked_by={:?}",
        privilege::blocked_by()
    );
    relay.reset();
    std::thread::sleep(Duration::from_millis(100));
}

/// One soak device, its pointer state as the host would track it, and counts.
#[cfg(windows)]
struct Soak {
    device: windows::Win32::UI::Controls::HSYNTHETICPOINTERDEVICE,
    stamp: bool,
    retry: bool,
    retried_ok: u64,
    longest_retry_us: u128,
    last_stamp: i64,
    in_contact: bool,
    started: std::time::Instant,
    last_call: std::time::Instant,
    last_ok: std::time::Instant,
    calls: u64,
    accepted: u64,
    not_ready: u64,
    not_ready_waits: u64,
    longest_not_ready_us: u128,
    slow_calls: u64,
    refused: std::collections::BTreeMap<String, u64>,
    restarts: u64,
    restarts_refused: u64,
    /// For the last call: the gap since the call before it, since the last
    /// accepted frame, and how long the call itself took.
    previous_call_gap_us: u128,
    refused_gap_us: u128,
    call_us: u128,
}

#[cfg(windows)]
impl Soak {
    fn new(
        device: windows::Win32::UI::Controls::HSYNTHETICPOINTERDEVICE,
        stamp: bool,
        retry: bool,
    ) -> Self {
        let now = std::time::Instant::now();
        Self {
            device,
            stamp,
            retry,
            retried_ok: 0,
            longest_retry_us: 0,
            last_stamp: 0,
            in_contact: false,
            started: now,
            last_call: now,
            last_ok: now,
            calls: 0,
            accepted: 0,
            not_ready: 0,
            not_ready_waits: 0,
            longest_not_ready_us: 0,
            slow_calls: 0,
            refused: Default::default(),
            restarts: 0,
            restarts_refused: 0,
            previous_call_gap_us: 0,
            refused_gap_us: 0,
            call_us: 0,
        }
    }

    /// One frame, retrying ERROR_NOT_READY as the host's injector does for
    /// 2 ms and then once per millisecond for up to a second, as its queue
    /// would at later samples and ticks. Returns 0 or the final HRESULT.
    fn call(
        &mut self,
        flags: windows::Win32::UI::Input::Pointer::POINTER_FLAGS,
        at: (i32, i32),
        pressure: u32,
    ) -> i32 {
        use std::time::{Duration, Instant};
        const NOT_READY: i32 = 0x8007_0015_u32 as i32;
        let first = Instant::now();
        self.previous_call_gap_us = first.duration_since(self.last_call).as_micros();
        self.refused_gap_us = first.duration_since(self.last_ok).as_micros();
        let mut counted = false;
        let code = loop {
            let performance = self.stamp.then(|| {
                let mut now = 0i64;
                unsafe {
                    let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut now);
                }
                self.last_stamp = now.max(self.last_stamp + 1);
                self.last_stamp as u64
            });
            let before = Instant::now();
            let code = inject_code(self.device, flags, at, pressure, performance);
            let took = before.elapsed();
            self.calls += 1;
            if took > Duration::from_millis(20) {
                self.slow_calls += 1;
                println!(
                    "SOAK slow_call t_ms={} call_us={} code={code:#010x}",
                    self.started.elapsed().as_millis(),
                    took.as_micros()
                );
            }
            if code != NOT_READY {
                break code;
            }
            if !std::mem::replace(&mut counted, true) {
                self.not_ready += 1;
            }
            let waited = first.elapsed();
            if waited >= Duration::from_secs(1) {
                break code;
            }
            if waited < Duration::from_millis(2) {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let now = Instant::now();
        self.call_us = now.duration_since(first).as_micros();
        if counted {
            let waited = self.call_us;
            self.longest_not_ready_us = self.longest_not_ready_us.max(waited);
            if waited >= 2_000 {
                self.not_ready_waits += 1;
            }
        }
        self.last_call = now;
        if code == 0 {
            self.accepted += 1;
            self.last_ok = now;
        } else {
            *self.refused.entry(format!("{code:#010x}")).or_default() += 1;
        }
        code
    }

    fn summary(&self, pattern: &str, elapsed: std::time::Duration) {
        println!(
            "SOAK summary pattern={pattern} stamp={} seconds={} calls={} accepted={} not_ready_samples={} not_ready_past_2ms={} longest_not_ready_us={} slow_calls={} refused={:?} restarts={} restarts_refused={} retried_ok={} longest_retry_us={}",
            self.stamp,
            elapsed.as_secs(),
            self.calls,
            self.accepted,
            self.not_ready,
            self.not_ready_waits,
            self.longest_not_ready_us,
            self.slow_calls,
            self.refused,
            self.restarts,
            self.restarts_refused,
            self.retried_ok,
            self.longest_retry_us,
        );
    }

    /// Bring the pointer to `flags` at `at` the way `PenContact` would: a
    /// move without a contact starts one, and an up without one is skipped.
    #[allow(clippy::too_many_arguments)]
    fn frame(
        &mut self,
        pattern: &str,
        flags: windows::Win32::UI::Input::Pointer::POINTER_FLAGS,
        at: (i32, i32),
        pressure: u32,
        stroke: u64,
        index: u32,
        burst_pos: u32,
    ) {
        use windows::Win32::UI::Input::Pointer::{
            POINTER_FLAG_DOWN, POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
        };
        let is_up = flags.0 & POINTER_FLAG_UP.0 != 0;
        if is_up && !self.in_contact {
            return;
        }
        let flags = if !self.in_contact && flags.0 & POINTER_FLAG_UPDATE.0 != 0 {
            (flags & !POINTER_FLAG_UPDATE) | POINTER_FLAG_DOWN
        } else {
            flags
        };
        let code = self.call(flags, at, pressure);
        if code == 0 {
            self.in_contact = !is_up;
            return;
        }
        // Any refusal other than NOT_READY cancels the contact.
        self.in_contact = false;
        let (since_call, since_ok, call_us) =
            (self.previous_call_gap_us, self.refused_gap_us, self.call_us);
        let screen = eternal_host::input::windows_inject::virtual_screen();
        let mut restart = String::from("none");
        if self.retry && flags.0 & POINTER_FLAG_UPDATE.0 != 0 {
            let refused = std::time::Instant::now();
            for _ in 0..50 {
                std::thread::sleep(std::time::Duration::from_millis(1));
                if self.call(flags, at, pressure) == 0 {
                    let waited = refused.elapsed().as_micros();
                    self.retried_ok += 1;
                    self.longest_retry_us = self.longest_retry_us.max(waited);
                    self.in_contact = true;
                    restart = format!("retried_after_us={waited}");
                    break;
                }
            }
        }
        if !self.in_contact && flags.0 & POINTER_FLAG_UPDATE.0 != 0 {
            self.restarts += 1;
            let contact = (flags & !POINTER_FLAG_UPDATE) | POINTER_FLAG_DOWN;
            let code = self.call(contact, at, pressure);
            self.in_contact = code == 0;
            if code != 0 {
                self.restarts_refused += 1;
            }
            restart = if code == 0 {
                "ok".into()
            } else {
                format!("{code:#010x}")
            };
        }
        println!(
            "SOAK refused code={code:#010x} pattern={pattern} stamp={} t_ms={} stroke={stroke} index={index} burst_pos={burst_pos} flags={:#x} x={} y={} pressure={pressure} since_previous_call_us={since_call} since_last_accepted_us={since_ok} call_us={call_us} virtual={},{},{}x{} restart={restart}",
            self.stamp,
            self.started.elapsed().as_millis(),
            flags.0,
            at.0,
            at.1,
            screen.left,
            screen.top,
            screen.width,
            screen.height,
        );
    }
}

/// Inject one raw pen frame at a desktop pixel, optionally with our own
/// PerformanceCount; 0 or the HRESULT.
#[cfg(windows)]
fn inject_code(
    device: windows::Win32::UI::Controls::HSYNTHETICPOINTERDEVICE,
    flags: windows::Win32::UI::Input::Pointer::POINTER_FLAGS,
    (x, y): (i32, i32),
    pressure: u32,
    performance: Option<u64>,
) -> i32 {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::Controls::{POINTER_TYPE_INFO, POINTER_TYPE_INFO_0};
    use windows::Win32::UI::Input::Pointer::{
        InjectSyntheticPointerInput, POINTER_FLAG_PRIMARY, POINTER_INFO, POINTER_PEN_INFO,
    };
    use windows::Win32::UI::WindowsAndMessaging::{PEN_MASK_PRESSURE, PT_PEN};
    let screen = eternal_host::input::windows_inject::virtual_screen();
    let (x, y) = eternal_host::input::desktop_pixel_to_pointer_location(x, y, screen);
    let info = POINTER_TYPE_INFO {
        r#type: PT_PEN,
        Anonymous: POINTER_TYPE_INFO_0 {
            penInfo: POINTER_PEN_INFO {
                pointerInfo: POINTER_INFO {
                    pointerType: PT_PEN,
                    pointerId: 1,
                    pointerFlags: flags | POINTER_FLAG_PRIMARY,
                    ptPixelLocation: POINT { x, y },
                    PerformanceCount: performance.unwrap_or(0),
                    ..Default::default()
                },
                penMask: PEN_MASK_PRESSURE,
                pressure,
                ..Default::default()
            },
        },
    };
    match unsafe { InjectSyntheticPointerInput(device, &[info]) } {
        Ok(()) => 0,
        Err(error) => error.code().0,
    }
}

/// Inject one raw pen frame at a desktop pixel; "ok" or the error code.
#[cfg(windows)]
fn inject_raw(
    device: windows::Win32::UI::Controls::HSYNTHETICPOINTERDEVICE,
    flags: windows::Win32::UI::Input::Pointer::POINTER_FLAGS,
    (x, y): (i32, i32),
    pressure: u32,
) -> String {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::Controls::{POINTER_TYPE_INFO, POINTER_TYPE_INFO_0};
    use windows::Win32::UI::Input::Pointer::{
        InjectSyntheticPointerInput, POINTER_FLAG_PRIMARY, POINTER_INFO, POINTER_PEN_INFO,
    };
    use windows::Win32::UI::WindowsAndMessaging::{PEN_MASK_PRESSURE, PT_PEN};
    let screen = eternal_host::input::windows_inject::virtual_screen();
    let (x, y) = eternal_host::input::desktop_pixel_to_pointer_location(x, y, screen);
    let info = POINTER_TYPE_INFO {
        r#type: PT_PEN,
        Anonymous: POINTER_TYPE_INFO_0 {
            penInfo: POINTER_PEN_INFO {
                pointerInfo: POINTER_INFO {
                    pointerType: PT_PEN,
                    pointerId: 1,
                    pointerFlags: flags | POINTER_FLAG_PRIMARY,
                    ptPixelLocation: POINT { x, y },
                    ..Default::default()
                },
                penMask: PEN_MASK_PRESSURE,
                pressure,
                ..Default::default()
            },
        },
    };
    match unsafe { InjectSyntheticPointerInput(device, &[info]) } {
        Ok(()) => "ok".to_string(),
        Err(error) => format!("{:#010x}", error.code().0),
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Run scripts/win/Test-PenInput.ps1 on an interactive Windows desktop.");
}
