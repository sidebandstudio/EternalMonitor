// A GUI program, so Windows opens no console window next to the app. Debug
// builds too: CI checks the build it makes, and a terminal still shows the
// log because the host joins its console (`console::join_parent_console`).
#![windows_subsystem = "windows"]

use std::sync::mpsc;

use eternal_host::control::{GuiControl, SharedControl};
use eternal_host::pipeline::DEFAULT_BITRATE_BPS;
use eternal_host::{capture, clock, discovery, gpu, gui, logging, stats, vdd};
use tracing::{error, info};
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Helper modes are dispatched first, before logging and before the
    // single-instance check below: a `--failure-reporter` copy outlives the
    // host it watches and must never be turned away as a second instance,
    // open the session log or touch the virtual display the way a host does.
    eternal_host::startup::run_reporter_if_requested();
    eternal_host::console::join_parent_console();

    // Before any DXGI query or window: monitor rects and injected input must
    // be physical pixels from the start, not only once the GUI's event loop
    // opts in. Unaware, a 125% monitor reports 2048x1152 instead of 2560x1440.
    #[cfg(windows)]
    eternal_host::input::windows_inject::become_per_monitor_dpi_aware();

    // One host per installation; a second launch shows the first one and
    // exits. Checked after helper-mode dispatch and before anything a host
    // owns: opening the session log rotates it and the uploader drains the
    // shared outbox, so a launch that loses here must not reach either, and
    // must not reset the virtual display the first host is streaming.
    let Some(instance) = eternal_host::single_instance::acquire() else {
        eprintln!("EternalMonitor is already running; showing its window");
        eternal_host::single_instance::show_existing();
        return Ok(());
    };

    // Anchor the process-wide media clock before anything else runs.
    clock::init();

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,mdns_sd=warn"));
    let (stdout_writer, _stdout_guard) = logging::non_blocking_output(std::io::stdout());
    let (memory_writer, _session_guard) = logging::MemoryLogWriter::start();
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(stdout_writer.with_max_level(tracing::Level::INFO))
        .with_filter(logging::MdnsDedupFilter::new());
    let memory_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false)
        .with_writer(move || memory_writer.clone())
        .with_filter(logging::MdnsDedupFilter::new());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(memory_layer)
        .with(eternal_host::diagnostics::FallbackWatch)
        .init();

    let _uploads = eternal_host::telemetry::UploadGuard::start();

    info!(
        path = %logging::session_log_path().display(),
        "Session log file initialized"
    );
    if let Some(problem) = &instance.problem {
        tracing::warn!(%problem, "Could not check for another EternalMonitor");
    }

    // Never leave a virtual display attached across runs. If a previous run crashed or was
    // force-killed while streaming the virtual extended display, the VDD device is still enabled
    // and shows up as a phantom monitor. Disabling it unconditionally at startup guarantees the
    // managed display only ever exists while EternalMonitor is actively using it. It runs beside
    // startup: a slow or hung task used to hold a Mirror stream's first frame for up to 30 s.
    vdd::disable_in_background();

    // Belt-and-suspenders: a panic anywhere (GUI, supervisor, pipeline threads) must still tear
    // the virtual display down before the process unwinds, so a crash can't strand a phantom
    // monitor. A panic or native crash that ends the process is also shown to the user and
    // queued for support instead of the window closing without a word.
    eternal_host::startup::install_failure_hooks();
    eternal_host::startup::take_previous_crashes();

    let listen_port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(9876);

    // Initialize FFmpeg early so encoder probing works during GPU detection
    if let Err(e) = ffmpeg_next::init() {
        error!(error = %e, "FFmpeg init failed");
        return Err(eternal_host::startup::fatal(e).into());
    }
    eternal_host::diagnostics::install_ffmpeg_logging();
    eternal_host::diagnostics::log_system_report();

    let gpu_info = gpu::GpuInfo::detect();

    // Best-effort capture source for the banner — reflects the default (primary). The
    // authoritative per-run source (including a persisted/fallback selection) is logged
    // inside the capture loop when the stream starts.
    let capture_summary = {
        let outputs = capture::enumerate_outputs();
        outputs
            .iter()
            .find(|o| o.is_primary)
            .or_else(|| outputs.first())
            .map(|o| format!("{} ({}x{})", o.device_name, o.width, o.height))
            .unwrap_or_else(|| "unknown".to_string())
    };

    info!("══════════════════════════════════");
    info!("  EternalMonitor v{}", env!("CARGO_PKG_VERSION"));
    info!("  GPU:     {} ({})", gpu_info.name, gpu_info.vendor);
    info!("  VRAM:    {} MB", gpu_info.dedicated_vram_mb);
    info!("  Encoder: {}", gpu_info.codec_display_name);
    info!("  Capture: {}", capture_summary);
    info!("  Listen:  0.0.0.0:{}", listen_port);
    info!("══════════════════════════════════");

    // Load persisted settings BEFORE the first pipeline spawns, so generation
    // 0 already streams with the user's bitrate/fps/encoder/display/codec.
    // (Previously only the GUI applied them, seconds later — and headless
    // runs never did.) The GUI re-applies the same values at startup, which
    // is idempotent.
    let mut persisted = eternal_host::settings::SettingsFile::load();
    let auth_token = persisted
        .ensure_auth_token()
        .map_err(eternal_host::startup::fatal)?;
    let initial_bitrate = if persisted.bitrate_mbps > 0.0 {
        (persisted.bitrate_mbps * 1_000_000.0).round() as u32
    } else {
        DEFAULT_BITRATE_BPS
    };

    let shared = SharedControl::new(listen_port, initial_bitrate);
    {
        let mut pairing = shared.pairing.lock();
        pairing.configure(persisted.require_pairing, auth_token);
        pairing.log_code();
    }
    let max_dgram = std::env::var("ETERNAL_MAX_DGRAM")
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(persisted.max_dgram)
        .clamp(576, 1400);
    shared
        .max_dgram
        .store(u32::from(max_dgram), std::sync::atomic::Ordering::SeqCst);
    info!(max_dgram, "Media packet size configured");
    if [30, 60, 90, 120].contains(&persisted.target_fps) {
        shared
            .target_fps
            .store(persisted.target_fps, std::sync::atomic::Ordering::SeqCst);
    }
    if let Some(name) = persisted.encoder_override.clone() {
        *shared.encoder_override.lock() = Some(name);
    }
    *shared.encoder_input.lock() = match std::env::var("ETERNAL_INPUT") {
        Ok(value) => eternal_host::encoder::input::EncoderInput::from_env(&value)
            .ok_or_else(|| format!("Invalid ETERNAL_INPUT {value:?}; use auto, bgra or yuv420"))
            .map_err(eternal_host::startup::fatal)?,
        Err(_) => persisted.encoder_input,
    };
    *shared.capture_target.lock() =
        eternal_host::control::CaptureTarget::from_setting(persisted.capture_display.as_deref());
    eternal_host::remote_config::load();
    #[cfg(windows)]
    eternal_host::capture::second_screen::load_memory(&shared);
    eternal_host::remote_config::log_launch();
    shared
        .hevc_enabled
        .store(persisted.hevc_enabled, std::sync::atomic::Ordering::SeqCst);
    shared
        .audio_enabled
        .store(persisted.stream_audio, std::sync::atomic::Ordering::SeqCst);

    {
        let mut stats = stats::PIPELINE_STATS.lock();
        stats.set_bitrate(initial_bitrate);
        stats.set_listen_addr(gui::detect_local_ip(listen_port));
        stats.set_target_addr(shared.target_addr.lock().to_string());
        stats.set_gpu_name(gpu_info.name.clone());
        stats.set_codec_name(gpu_info.codec_display_name.clone());
    }

    // Optional encoder override from the environment, applied BEFORE the first pipeline run
    // so it takes effect without a manual stream restart. Useful for testing a specific
    // encoder, e.g. `set ETERNAL_ENCODER=h264_amf` to force the AMD path.
    if let Ok(name) = std::env::var("ETERNAL_ENCODER") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            info!(encoder = %name, "Encoder forced from ETERNAL_ENCODER (no restart needed)");
            *shared.encoder_override.lock() = Some(name);
        }
    }
    // Optional HEVC preference override for automation (the E2E harness runs headless without
    // the GUI toggle). When present it is authoritative in BOTH directions, so
    // ETERNAL_HEVC=0 pins H.264 regardless of the persisted setting.
    if let Ok(value) = std::env::var("ETERNAL_HEVC") {
        let enabled = value.trim() == "1";
        info!(enabled, "HEVC preference forced from ETERNAL_HEVC");
        shared
            .hevc_enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }
    // Optional target FPS override (e.g. ETERNAL_FPS=30) to lighten encode load — useful when
    // testing a hardware encoder on a weak/integrated GPU.
    if let Ok(fps) = std::env::var("ETERNAL_FPS") {
        if let Ok(fps) = fps.trim().parse::<u32>() {
            if fps > 0 {
                info!(fps, "Target FPS forced from ETERNAL_FPS");
                shared
                    .target_fps
                    .store(fps, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    let (supervisor_tx, supervisor_rx) = mpsc::channel();
    let _diagnostics = eternal_host::diagnostics::Monitor::start(shared.clone());
    let gui_control = GuiControl {
        shared: shared.clone(),
        supervisor_tx: supervisor_tx.clone(),
    };

    /// Set by the termination-signal handler; the headless loop polls it.
    static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    #[cfg(unix)]
    extern "C" fn on_terminate_signal(_signal: libc::c_int) {
        // Async-signal-safe: only a flag store — the polling loop does the work.
        SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(windows)]
    unsafe extern "system" fn on_console_control(event: u32) -> windows::Win32::Foundation::BOOL {
        use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
        if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
            SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
            true.into()
        } else {
            false.into()
        }
    }

    let supervisor_thread = std::thread::spawn(move || {
        eternal_host::supervisor::run(listen_port, shared, gpu_info, supervisor_tx, supervisor_rx);
    });

    let mdns = discovery::advertise_service(listen_port);

    // Headless mode for automation (the iOS E2E harness runs the host without
    // a window): stream until SIGTERM/SIGINT, then fall through to the same
    // clean teardown as a GUI quit — mDNS goodbye, VDD disable, bounded
    // supervisor join. (Previously a kill skipped all of that.)
    let mut window_error = None;
    if std::env::var("ETERNAL_HEADLESS").is_ok_and(|v| v.trim() == "1") {
        info!("ETERNAL_HEADLESS=1 — running without GUI until terminated");
        #[cfg(unix)]
        unsafe {
            let handler = on_terminate_signal as extern "C" fn(libc::c_int);
            libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
            libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        }
        #[cfg(windows)]
        unsafe {
            eternal_host::console::ensure_console();
            if let Err(error) = windows::Win32::System::Console::SetConsoleCtrlHandler(
                Some(on_console_control),
                true,
            ) {
                tracing::warn!(%error, "Could not register the console shutdown handler");
            }
        }
        let log_e2e = std::env::var("ETERNAL_E2E_LOG").is_ok_and(|v| v == "1");
        let mut last_sample = std::time::Instant::now();
        while !SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if log_e2e && last_sample.elapsed() >= std::time::Duration::from_secs(1) {
                last_sample = std::time::Instant::now();
                let sample = stats::PIPELINE_STATS.lock();
                info!(
                    captured = sample.capture_frame_count,
                    encoded = sample.encode_frame_count,
                    sent = sample.transport_packets_sent,
                    capture_fps = sample.capture_fps,
                    encode_fps = sample.encode_fps,
                    send_fps = sample.transport_fps,
                    encode_us = sample.encode_time_us,
                    retransmits = sample.transport_retransmits,
                    "E2E_HOST_STATS"
                );
            }
        }
        info!("Termination signal received — shutting down cleanly");
    } else if let Err(e) = gui::run_gui(gui_control.clone()) {
        error!(error = %e, "GUI exited with error");
        window_error = Some(format!("Window error: {e}"));
    }

    let stop_streaming = || {
        eternal_host::diagnostics::log_session_end_at_quit(&gui_control.shared);
        gui_control.shared.stop();
        gui_control.request_shutdown();

        // Never leave the virtual display attached after we exit — it should only exist while
        // EternalMonitor is actively using it. No Enable starts once the second screen worker
        // is shut down, so this Disable is the last change. If it may still be on, the failure
        // reporter turns it off after this process exits.
        eternal_host::capture::second_screen::shut_down(&gui_control.shared);
        eternal_host::startup::display_off_at_quit(vdd::disable);
    };
    // A window error is saved first and shown after the cleanup, as a panic is.
    match window_error {
        Some(detail) => eternal_host::startup::report_after(&detail, stop_streaming),
        None => stop_streaming(),
    }

    // Give the supervisor a bounded amount of time to shut down cleanly,
    // exiting the moment it finishes instead of always burning the full
    // window. If it doesn't finish (blocked on DXGI acquire, etc.),
    // force-exit so the process never lingers as an invisible zombie.
    let (supervisor_done_tx, supervisor_done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Err(error) = supervisor_thread.join() {
            error!(error = ?error, "Supervisor thread panicked");
        }
        let _ = supervisor_done_tx.send(());
    });

    // Say goodbye on mDNS while the supervisor winds down, so iPads drop the
    // host from their scan list promptly (process::exit skips destructors —
    // without this no goodbye is ever sent).
    if let Some(mdns) = mdns {
        discovery::say_goodbye(mdns);
    }

    if supervisor_done_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .is_err()
    {
        tracing::warn!("Supervisor did not shut down within 3s — forcing exit");
    }

    info!("EternalMonitor shutting down");
    // process::exit skips destructors. Flush the log writers and the upload
    // outbox first, so a normal quit is told apart from a crash.
    logging::flush_before_exit(_session_guard, _uploads, _stdout_guard);
    eternal_host::startup::finish();
    std::process::exit(0);
}
