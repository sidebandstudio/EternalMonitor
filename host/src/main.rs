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
    // Before anything below creates the settings folder or a file in it: a
    // folder that held nothing means a fresh install, which opens first-run
    // setup. Upgrades never see it.
    eternal_host::settings::note_launch();
    // Helper modes are dispatched first, before logging and before the
    // single-instance check below: a `--failure-reporter` copy outlives the
    // host it watches and must never be turned away as a second instance,
    // open the session log or touch the virtual display the way a host does.
    eternal_host::startup::run_reporter_if_requested();
    // Before any DXGI query or window (a restart copy's message too):
    // monitor rects and injected input must be physical pixels from the
    // start, not only once the GUI's event loop opts in. Unaware, a 125%
    // monitor reports 2048x1152 instead of 2560x1440.
    #[cfg(windows)]
    eternal_host::input::windows_inject::become_per_monitor_dpi_aware();

    // A copy started by "Restart EternalMonitor" tells the old window it runs
    // before anything else, then waits for the old host to exit and takes
    // over its single-instance mutex (or explains and exits).
    let restart_started = std::time::Instant::now();
    let restarted = eternal_host::relaunch::take_over_if_requested();
    let restart_wait = restarted.is_some().then(|| restart_started.elapsed());
    eternal_host::console::join_parent_console();

    // One host per installation; a second launch shows the first one and
    // exits. Checked after helper-mode dispatch and before anything a host
    // owns: opening the session log rotates it and the uploader drains the
    // shared outbox, so a launch that loses here must not reach either, and
    // must not reset the virtual display the first host is streaming.
    let instance = match restarted {
        Some(Some(instance)) => instance,
        Some(None) => return Ok(()),
        None => {
            let Some(instance) = eternal_host::single_instance::acquire() else {
                eprintln!("EternalMonitor is already running; showing its window");
                eternal_host::single_instance::show_existing();
                return Ok(());
            };
            instance
        }
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
    if let Some(waited) = restart_wait {
        info!(
            waited_ms = waited.as_millis() as u64,
            "Started by Restart EternalMonitor once the previous copy had exited"
        );
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
    // Before the renderer: a new version that fails to start still logs how
    // the update to it went.
    eternal_host::updates::report_previous_update();

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
    eternal_host::network::log_addresses();

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
    // Each Troubleshooting knob: an environment variable (developers, CI)
    // wins, then the saved setting, then Automatic.
    let overrides = eternal_host::troubleshooting::Overrides::from_process(&persisted)
        .map_err(eternal_host::startup::fatal)?;
    let gpu_value = eternal_host::gpu_preference::read_value().ok().flatten();
    overrides.log(eternal_host::gpu_preference::preference_in(
        gpu_value.as_deref(),
    ));
    if let Some(record) = persisted.gpu_switch.as_mut().filter(|r| r.checking) {
        let saved =
            eternal_host::gpu_preference::started_after_switch(record, gpu_value.as_deref());
        info!(
            gpu = %record.gpu,
            preference = ?record.preference,
            saved,
            "Graphics card switch: started again after the switch"
        );
        if !saved {
            let _ = persisted.save();
        }
    }
    let max_dgram = overrides.max_dgram.value;
    shared
        .max_dgram
        .store(u32::from(max_dgram), std::sync::atomic::Ordering::SeqCst);
    info!(max_dgram, "Media packet size configured");
    shared.target_fps.store(
        overrides.target_fps.value,
        std::sync::atomic::Ordering::SeqCst,
    );
    *shared.encoder_override.lock() = overrides.encoder.value.clone();
    *shared.encoder_input.lock() = overrides.encoder_input.value;
    *shared.gpu_switch.lock() = persisted.gpu_switch.clone();
    *shared.capture_target.lock() =
        eternal_host::control::CaptureTarget::from_setting(persisted.capture_display.as_deref());
    eternal_host::remote_config::load();
    #[cfg(windows)]
    eternal_host::capture::second_screen::load_memory(&shared);
    eternal_host::remote_config::log_launch();
    shared
        .hevc_enabled
        .store(overrides.hevc.value, std::sync::atomic::Ordering::SeqCst);
    shared
        .audio_enabled
        .store(persisted.stream_audio, std::sync::atomic::Ordering::SeqCst);

    let listen_addr = gui::detect_local_ip(listen_port);
    {
        let mut stats = stats::PIPELINE_STATS.lock();
        stats.set_bitrate(initial_bitrate);
        stats.set_listen_addr(listen_addr);
        stats.set_target_addr(shared.target_addr.lock().to_string());
        stats.set_gpu_name(gpu_info.name.clone());
        stats.set_codec_name(gpu_info.codec_display_name.clone());
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
            eternal_host::capture::second_screen::retry_saving(&gui_control.shared);
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

    // The window is gone and the display is off, so a slow disk keeps no one
    // waiting on screen: give the second screen record up to 30 s to land.
    eternal_host::capture::second_screen::finish_saving(&gui_control.shared);

    eternal_host::startup::simulate_slow_exit();
    info!("EternalMonitor shutting down");
    // process::exit skips destructors. Flush the log writers and the upload
    // outbox first, so a normal quit is told apart from a crash.
    logging::flush_before_exit(_session_guard, _uploads, _stdout_guard);
    eternal_host::startup::finish();
    std::process::exit(0);
}
