//! Low-frequency support evidence. Never collect credentials or user documents.
use std::ffi::CStr;
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

use crate::control::{CaptureTarget, SharedControl};
use crate::transport::link::LinkId;
use crate::transport::session::{EndReason, SessionEnd};
use tracing::info;

static SYSTEM_REPORT: OnceLock<String> = OnceLock::new();

pub fn system_report() -> &'static str {
    SYSTEM_REPORT.get_or_init(|| {
        let mut lines = vec![format!(
            "EternalMonitor host {} revision={} | OS={} arch={} logical_cpus={} | avcodec={} avutil={} | report_schema=1",
            env!("CARGO_PKG_VERSION"), env!("ETERNAL_BUILD_REVISION"), std::env::consts::OS, std::env::consts::ARCH,
            std::thread::available_parallelism().map_or(0, usize::from),
            ffmpeg_next::codec::version(), ffmpeg_next::util::version(),
        )];
        #[cfg(windows)]
        lines.extend(windows_system_report());
        let encoders = ["h264_nvenc", "h264_amf", "h264_qsv", "hevc_nvenc", "hevc_amf", "hevc_qsv", "h264_videotoolbox", "hevc_videotoolbox", "libx264"];
        lines.push(format!("Compiled encoders: {}", encoders.iter().map(|name| format!("{name}={}", ffmpeg_next::encoder::find_by_name(name).is_some())).collect::<Vec<_>>().join(" ")));
        lines.join("\n")
    })
}

pub fn log_system_report() {
    for line in system_report().lines() {
        info!(details = line, "Support system information");
    }
}

extern "C" {
    fn eternal_install_ffmpeg_logger(sink: unsafe extern "C" fn(i32, *const libc::c_char));
}

/// Install once, before encoder threads start. The C shim owns the platform's va_list ABI.
pub fn install_ffmpeg_logging() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| unsafe { eternal_install_ffmpeg_logger(ffmpeg_message) });
}

thread_local! {
    /// Set while this thread opens an encoder that is one of several to try.
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `open`, an encoder open with another encoder to try if it fails,
/// with FFmpeg's ERROR lines from this thread logged as warnings. On a PC
/// without that graphics card or driver they are expected, and the next
/// encoder takes over; the caller logs one error if none opens. Every other
/// FFmpeg line keeps its level.
pub fn while_probing<T>(open: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROBING.with(|probing| probing.set(self.0));
        }
    }
    let _restore = Restore(PROBING.with(|probing| probing.replace(true)));
    open()
}

unsafe extern "C" fn ffmpeg_message(level: i32, message: *const libc::c_char) {
    // Never unwind through a C callback, including a third-party tracing subscriber.
    let _ = std::panic::catch_unwind(|| {
        if message.is_null() {
            return;
        }
        let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        let message = message.trim();
        if message.is_empty() {
            return;
        }
        // `ffmpeg_level` keeps FFmpeg's own level either way.
        if level <= ffmpeg_next::ffi::AV_LOG_ERROR && !PROBING.with(std::cell::Cell::get) {
            tracing::error!(target: "ffmpeg", ffmpeg_level = level, "{message}");
        } else {
            tracing::warn!(target: "ffmpeg", ffmpeg_level = level, "{message}");
        }
    });
}

pub struct Monitor(mpsc::Sender<()>, Option<std::thread::JoinHandle<()>>);

impl Monitor {
    pub fn start(shared: SharedControl) -> Self {
        let (stop, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            while receiver.recv_timeout(Duration::from_secs(10))
                == Err(mpsc::RecvTimeoutError::Timeout)
            {
                log_stream_summary(&shared);
            }
            log_stream_summary(&shared);
        });
        Self(stop, Some(thread))
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.0.send(());
        if let Some(thread) = self.1.take() {
            let _ = thread.join();
        }
    }
}

fn log_stream_summary(shared: &SharedControl) {
    use std::sync::atomic::Ordering::Relaxed;
    // Don't keep pipeline locks held while formatting/writing tracing events.
    let session_id = shared.session.lock().session_id();
    let sample = crate::stats::PIPELINE_STATS.lock().clone();
    let now_ms = crate::clock::host_now_us() / 1000;
    info!(session_id = ?session_id, running = sample.pipeline_running,
        captured = sample.capture_frame_count, encoded = sample.encode_frame_count,
        sent = sample.transport_packets_sent, capture_fps = sample.capture_fps,
        encode_fps = sample.encode_fps, send_fps = sample.transport_fps,
        encode_us = sample.encode_time_us, codec = %sample.codec_name,
        cpu_fallback = sample.using_software_fallback,
        resolution = ?sample.capture_resolution, bitrate = sample.bitrate_bps,
        target_fps = shared.effective_fps(), max_dgram = shared.max_dgram.load(Relaxed),
        input = ?*shared.encoder_input.lock(), hevc = shared.hevc_enabled.load(Relaxed),
        retransmits = sample.transport_retransmits, usb_drops = sample.usb_frames_dropped,
        usb_state = %sample.usb_link_state, usb_devices = sample.usb_devices,
        capture_heartbeat_age_ms = now_ms.saturating_sub(shared.hb_capture_frame_ms.load(Relaxed)),
        encode_heartbeat_age_ms = now_ms.saturating_sub(shared.hb_encode_frame_ms.load(Relaxed)),
        "Support stream summary");
}

/// Video for this long, with frames the client reports, makes a session
/// `steady` in its summary.
pub const STEADY_AFTER: Duration = Duration::from_secs(10);

/// What a session summary says about this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    /// `win:<build>.<ubr>`.
    pub os: String,
    /// The graphics card makers, `+`-joined, or `-`.
    pub gpu: String,
    /// More than one graphics card, `None` when they could not be listed.
    pub hybrid: Option<bool>,
}

/// This PC's facts, read once. A Windows build or update number that can't
/// be read is `win:0.0`: the summary has no other value for it.
pub fn host_facts() -> &'static HostFacts {
    static FACTS: OnceLock<HostFacts> = OnceLock::new();
    FACTS.get_or_init(|| {
        #[cfg(windows)]
        {
            let os_key = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
            // The virtual display's own adapter is not a card.
            let adapters = crate::vdd_snapshot::render_adapters(
                &crate::vdd_snapshot::adapters(),
                &crate::vdd_snapshot::targets(),
            );
            let cards: Vec<(u32, u32)> = adapters
                .iter()
                .map(|adapter| (adapter.vendor_id, adapter.device_id))
                .collect();
            HostFacts {
                os: os_token(
                    &registry_string(os_key, "CurrentBuildNumber"),
                    &registry_dword(os_key, "UBR"),
                ),
                gpu: gpu_token(&cards),
                hybrid: hybrid(&cards),
            }
        }
        #[cfg(not(windows))]
        HostFacts {
            os: os_token("", ""),
            gpu: gpu_token(&[]),
            hybrid: None,
        }
    })
}

/// `win:<build>.<ubr>` when both are 1-6 digits, else `win:0.0`.
fn os_token(build: &str, ubr: &str) -> String {
    let number = |text: &str| {
        let text = text.trim();
        (1..=6).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit())
    };
    if number(build) && number(ubr) {
        format!("win:{}.{}", build.trim(), ubr.trim())
    } else {
        "win:0.0".to_string()
    }
}

/// Microsoft's adapters (Basic Render Driver, remote displays) are not cards.
const VENDOR_MICROSOFT: u32 = 0x1414;

/// The makers of `cards` (PCI vendor, device), sorted and de-duplicated:
/// `AMD`, `Intel`, `NVIDIA`, `other`. `-` when there are none.
fn gpu_token(cards: &[(u32, u32)]) -> String {
    let makers: std::collections::BTreeSet<&str> = cards
        .iter()
        .filter(|(vendor, _)| *vendor != VENDOR_MICROSOFT)
        .map(|(vendor, _)| match vendor {
            0x10DE => "NVIDIA",
            0x1002 | 0x1022 => "AMD",
            0x8086 => "Intel",
            _ => "other",
        })
        .collect();
    if makers.is_empty() {
        "-".to_string()
    } else {
        makers.into_iter().collect::<Vec<_>>().join("+")
    }
}

/// Whether `cards` hold more than one graphics card, as on a laptop with
/// integrated and discrete graphics. Windows lists some cards twice.
#[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
fn hybrid(cards: &[(u32, u32)]) -> Option<bool> {
    let distinct: std::collections::BTreeSet<_> = cards
        .iter()
        .filter(|(vendor, _)| *vendor != VENDOR_MICROSOFT)
        .collect();
    (!cards.is_empty()).then_some(distinct.len() > 1)
}

/// The live session with its mode, and the first `second_screen_fallback`
/// reason logged while it was live in Second screen mode. Reasons logged
/// between sessions, or while the session mirrors, are dropped.
struct SessionFallback {
    session_id: Option<u32>,
    mode: Option<Arc<parking_lot::Mutex<CaptureTarget>>>,
    reason: Option<&'static str>,
}

static SESSION_FALLBACK: parking_lot::Mutex<SessionFallback> =
    parking_lot::Mutex::new(SessionFallback {
        session_id: None,
        mode: None,
        reason: None,
    });

/// `session_id` started: fallbacks logged from now on are its own.
pub fn note_session_start(session_id: u32, shared: &SharedControl) {
    *SESSION_FALLBACK.lock() = SessionFallback {
        session_id: Some(session_id),
        mode: Some(Arc::clone(&shared.capture_target)),
        reason: None,
    };
}

fn note_fallback(reason: &str) {
    let (session_id, mode) = {
        let slot = SESSION_FALLBACK.lock();
        match (slot.session_id, &slot.mode) {
            (Some(id), Some(mode)) if slot.reason.is_none() => (id, Arc::clone(mode)),
            _ => return,
        }
    };
    // Bounded: a log line written while the mode is held can't wait on it.
    let second_screen = mode
        .try_lock_for(Duration::from_millis(50))
        .is_some_and(|mode| *mode == CaptureTarget::VirtualExtended);
    let mut slot = SESSION_FALLBACK.lock();
    if second_screen && slot.session_id == Some(session_id) {
        slot.reason.get_or_insert(fallback_token(reason));
    }
}

/// The fallback `session_id` collected, which then collects no more. `None`
/// for a session that isn't the one collecting.
fn take_fallback(session_id: u32) -> Option<&'static str> {
    let mut slot = SESSION_FALLBACK.lock();
    if slot.session_id != Some(session_id) {
        return None;
    }
    slot.session_id = None;
    slot.mode = None;
    slot.reason.take()
}

/// A `second_screen_fallback` reason as the summary names it. `remembered`
/// is a failure saved by an earlier app run.
fn fallback_token(reason: &str) -> &'static str {
    match reason {
        "attach_timeout" => "attach_timeout",
        "enable_failed" => "enable_failed",
        "no_driver" => "no_driver",
        "no_frames" => "no_frames",
        "duplication_failed" => "duplication_failed",
        "remembered" => "remembered",
        _ => "other",
    }
}

/// Notes the `second_screen_fallback` reason of every log event for the
/// session summary, wherever the second screen gives it up.
pub struct FallbackWatch;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FallbackWatch {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Reason;
        impl tracing::field::Visit for Reason {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "second_screen_fallback" {
                    note_fallback(value);
                }
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "second_screen_fallback" {
                    note_fallback(format!("{value:?}").trim_matches('"'));
                }
            }
        }
        if event
            .metadata()
            .fields()
            .field("second_screen_fallback")
            .is_some()
        {
            event.record(&mut Reason);
        }
    }
}

/// Logs the summary of a session, or of a HELLO2 the host turned down. The
/// upload sends it as category `summary` (`crate::telemetry`).
pub fn log_session_end(end: &SessionEnd, shared: &SharedControl) {
    let second_screen = *shared.capture_target.lock() == CaptureTarget::VirtualExtended;
    // `mode=` is the mode at the end; the fallback was counted in Second
    // screen mode, perhaps before a switch to Mirror.
    let fallback = end.session_id.and_then(take_fallback);
    info!(
        "{}",
        session_summary(end, second_screen, fallback, host_facts())
    );
}

/// The app is quitting: the summary of the stream it stops.
pub fn log_session_end_at_quit(shared: &SharedControl) {
    let end = shared.session.lock().shut_down(std::time::Instant::now());
    if let Some(end) = end {
        log_session_end(&end, shared);
    }
}

/// The `session_summary v=1` line (b-session-summary contract): enumerated
/// tokens, bounded numbers and the facts above, nothing else.
pub fn session_summary(
    end: &SessionEnd,
    second_screen: bool,
    fallback: Option<&'static str>,
    host: &HostFacts,
) -> String {
    use eternal_wire::v2::control::{ByeReason, HelloStatus};
    let streamed = end.first_video.is_some();
    // Only the client's reports say a picture arrived: video sent alone is
    // `video_sent`, however long it went on.
    let steady = end.frames_received
        && end
            .first_video
            .is_some_and(|video| end.duration.saturating_sub(video) >= STEADY_AFTER);
    let stage = match (end.reason, end.first_video) {
        (EndReason::Refused(_), _) => "hello_sent",
        (_, None) => "accepted",
        _ if steady => "steady",
        _ if end.frames_received => "first_frame",
        _ => "video_sent",
    };
    let pairing = |status| matches!(status, HelloStatus::Unauthorized | HelloStatus::RateLimited);
    let outcome = match end.reason {
        EndReason::Refused(status) if pairing(status) => "pairing",
        EndReason::Refused(_) => "error",
        _ if streamed => "streamed",
        EndReason::Goodbye(ByeReason::UserDisconnect) => "user_ended",
        EndReason::Goodbye(ByeReason::Error) | EndReason::Liveness => "no_picture",
        EndReason::Goodbye(_) | EndReason::Superseded | EndReason::Shutdown => "app_ended",
        EndReason::Revoked => "pairing",
        EndReason::LinkLost => "error",
    };
    let failure = match end.reason {
        EndReason::Refused(status) if pairing(status) => "none",
        EndReason::Refused(_) => "other",
        EndReason::LinkLost if matches!(end.link, LinkId::Usb { .. }) => "usb_lost",
        EndReason::LinkLost => "other",
        EndReason::Liveness | EndReason::Goodbye(ByeReason::Error) if !streamed => "no_video",
        EndReason::Liveness => "signal_lost",
        EndReason::Goodbye(ByeReason::Error) => "other",
        _ => "none",
    };
    let end_token = match end.reason {
        EndReason::Goodbye(_) => "goodbye",
        EndReason::Liveness => "liveness",
        EndReason::Superseded => "superseded",
        EndReason::Shutdown => "shutdown",
        EndReason::Refused(_) | EndReason::LinkLost | EndReason::Revoked => "error",
    };
    let session_id = end.session_id.map_or("-".to_string(), |id| id.to_string());
    let transport = match end.link {
        LinkId::Udp => "wifi",
        LinkId::Usb { .. } => "usb",
    };
    let ttff = end.first_video.map_or("-".to_string(), |video| {
        (video.as_millis().min(600_000) as u64).to_string()
    });
    let hybrid = host
        .hybrid
        .map_or("-", |hybrid| if hybrid { "true" } else { "false" });
    format!(
        "session_summary v=1 role=pc session_id={session_id} nonce={:016x} transport={transport} \
         mode={} stage={stage} outcome={outcome} failure={failure} fallback={} end={end_token} \
         ttff_ms={ttff} duration_ms={} os={} hw=pc gpu={} hybrid={hybrid}",
        end.client_nonce,
        if second_screen {
            "second_screen"
        } else {
            "mirror"
        },
        if end.session_id.is_some() {
            fallback.unwrap_or("none")
        } else {
            "none"
        },
        end.duration.as_millis().min(2_592_000_000),
        host.os,
        host.gpu,
    )
}

/// The Windows release for a feedback email, e.g. "Windows 11 24H2 (build
/// 26100.2894)". None off Windows or when the registry can't be read.
pub fn windows_version() -> Option<String> {
    #[cfg(windows)]
    {
        let os_key = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
        crate::feedback::windows_label(
            &registry_string(os_key, "DisplayVersion"),
            &registry_string(os_key, "CurrentBuildNumber"),
            &registry_dword(os_key, "UBR"),
        )
    }
    #[cfg(not(windows))]
    None
}

#[cfg(windows)]
fn windows_system_report() -> Vec<String> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let os_key = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let cpu_key = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0";
    let product = registry_string(os_key, "ProductName");
    let build = registry_string(os_key, "CurrentBuildNumber");
    let mut lines = vec![format!(
        // ProductName can retain "Windows 10" on Windows 11. Name the OS from
        // the build, and keep the registry label beside it as the source.
        "Windows os={} registry_product={} installation_type={} display_version={} build={}.{}",
        windows_name(&product, &build),
        product,
        registry_string(os_key, "InstallationType"),
        registry_string(os_key, "DisplayVersion"),
        build,
        registry_dword(os_key, "UBR")
    )];
    lines.push(format!(
        "CPU={} identifier={}",
        registry_string(cpu_key, "ProcessorNameString"),
        registry_string(cpu_key, "Identifier")
    ));
    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    match unsafe { GlobalMemoryStatusEx(&mut memory) } {
        Ok(()) => lines.push(format!(
            "RAM total_mb={} available_mb={} load_percent={}",
            memory.ullTotalPhys / 1048576,
            memory.ullAvailPhys / 1048576,
            memory.dwMemoryLoad
        )),
        Err(error) => lines.push(format!("RAM query failed: {error}")),
    }
    lines
}

/// The Windows name for a registry ProductName and build number. Windows 11
/// starts at build 22000 but kept "Windows 10" in ProductName.
#[cfg_attr(not(windows), allow(dead_code))] // Read from Windows only.
fn windows_name(product: &str, build: &str) -> String {
    let windows_11 = build
        .trim()
        .parse::<u32>()
        .is_ok_and(|build| build >= 22000);
    match product.strip_prefix("Windows 10") {
        Some(edition) if windows_11 => format!("Windows 11{edition}"),
        _ => product.to_string(),
    }
}

#[cfg(windows)]
pub(crate) fn registry_string(key: &str, name: &str) -> String {
    registry_sz(key, name).unwrap_or_else(|code| format!("unavailable({code})"))
}

/// A string value under HKLM, `None` when it is missing or empty.
#[cfg(windows)]
pub(crate) fn registry_value(key: &str, name: &str) -> Option<String> {
    registry_sz(key, name)
        .ok()
        .filter(|value| !value.is_empty())
}

#[cfg(windows)]
fn registry_sz(key: &str, name: &str) -> Result<String, String> {
    use windows::{
        core::PCWSTR,
        Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ},
    };
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut value = [0u16; 512];
    let mut bytes = std::mem::size_of_val(&value) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if result.is_err() {
        return Err(result.0.to_string());
    }
    Ok(String::from_utf16_lossy(
        &value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())],
    )
    .trim()
    .to_string())
}

#[cfg(windows)]
pub(crate) fn registry_dword(key: &str, name: &str) -> String {
    use windows::{
        core::PCWSTR,
        Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD},
    };
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut value = 0u32;
    let mut bytes = 4;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut bytes),
        )
    };
    if result.is_err() {
        format!("unavailable({})", result.0)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use eternal_wire::v2::control::{ByeReason, HelloStatus};

    /// The backend's check of a summary line, ported from
    /// `copy_session_summaries` (b-session-summaries.sql): the whole message
    /// matches, and the numbers are in range. With ETERNAL_SUMMARY_DUMP set,
    /// the line is also appended to that file, for the check in PostgreSQL.
    pub(crate) fn assert_contract(line: &str) {
        static CONTRACT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(concat!(
                r"^session_summary v=1 role=(pc|ipad) session_id=(0|[1-9][0-9]{0,9}|-) nonce=([0-9a-f]{16}|-)",
                r" transport=(usb|wifi) mode=(mirror|second_screen|unknown)",
                r" stage=(started|hello_sent|accepted|video_sent|first_frame|steady)",
                r" outcome=(streamed|no_reply|no_picture|pairing|user_ended|app_ended|error)",
                r" failure=(none|no_reply|no_video|no_frame|vdd_enable_failed|vdd_attach_timeout|vdd_not_attached|capture_failed|encoder_failed|usb_lost|signal_lost|other)",
                r" fallback=(none|attach_timeout|enable_failed|no_driver|no_frames|duplication_failed|remembered|other)",
                r" end=(goodbye|liveness|superseded|shutdown|timeout|user|background|error)",
                r" ttff_ms=(0|[1-9][0-9]{0,5}|-) duration_ms=(0|[1-9][0-9]{0,9})",
                r" os=(win:[0-9]{1,6}\.[0-9]{1,6}|ipados:[0-9]{1,3}\.[0-9]{1,3}(\.[0-9]{1,3})?) hw=(pc|iPad[0-9]{1,2},[0-9]{1,2})",
                r" gpu=(-|AMD(\+Intel)?(\+NVIDIA)?(\+other)?|Intel(\+NVIDIA)?(\+other)?|NVIDIA(\+other)?|other) hybrid=(true|false|-)$",
            ))
            .unwrap()
        });
        assert!(line.len() <= 400 && CONTRACT.is_match(line), "{line}");
        let field = |key: &str| {
            line.split(' ')
                .find_map(|word| word.strip_prefix(key))
                .filter(|value| *value != "-")
                .map(|value| value.parse::<u64>().unwrap())
        };
        assert!(field("session_id=").unwrap_or(0) <= 4_294_967_295, "{line}");
        assert!(field("ttff_ms=").unwrap_or(0) <= 600_000, "{line}");
        assert!(field("duration_ms=").unwrap() <= 2_592_000_000, "{line}");
        if let Some(path) = std::env::var_os("ETERNAL_SUMMARY_DUMP") {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            // One write: tests running at once must not interleave lines.
            file.write_all(format!("{line}\n").as_bytes()).unwrap();
        }
    }

    /// Held by tests that use the session's fallback slot.
    pub(crate) static FALLBACK_SLOT: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// Collects the `session_summary` lines logged.
    pub(crate) struct SummaryCapture(pub std::sync::Arc<parking_lot::Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SummaryCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Message(Option<String>);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = Some(format!("{value:?}"));
                    }
                }
            }
            let mut message = Message(None);
            event.record(&mut message);
            if let Some(line) = message
                .0
                .filter(|line| line.starts_with("session_summary "))
            {
                self.0.lock().push(line);
            }
        }
    }

    fn pc() -> HostFacts {
        HostFacts {
            os: "win:26200.9457".into(),
            gpu: "Intel+NVIDIA".into(),
            hybrid: Some(true),
        }
    }

    fn end(reason: EndReason, first_video_ms: Option<u64>, duration_ms: u64) -> SessionEnd {
        SessionEnd {
            session_id: (!matches!(reason, EndReason::Refused(_))).then_some(2_654_435_769),
            client_nonce: 0x0e2e_0001,
            link: LinkId::Udp,
            reason,
            first_video: first_video_ms.map(Duration::from_millis),
            frames_received: first_video_ms.is_some(),
            duration: Duration::from_millis(duration_ms),
        }
    }

    /// stage, outcome, failure and end, as the line gives them.
    fn tokens(line: &str) -> [&str; 4] {
        ["stage=", "outcome=", "failure=", "end="].map(|key| {
            line.split(' ')
                .find_map(|word| word.strip_prefix(key))
                .unwrap()
        })
    }

    #[test]
    fn a_streamed_session_summary_is_the_contract_line() {
        let line = session_summary(
            &end(
                EndReason::Goodbye(ByeReason::UserDisconnect),
                Some(412),
                61_234,
            ),
            true,
            Some("attach_timeout"),
            &pc(),
        );
        assert_eq!(
            line,
            "session_summary v=1 role=pc session_id=2654435769 nonce=000000000e2e0001 \
             transport=wifi mode=second_screen stage=steady outcome=streamed failure=none \
             fallback=attach_timeout end=goodbye ttff_ms=412 duration_ms=61234 \
             os=win:26200.9457 hw=pc gpu=Intel+NVIDIA hybrid=true"
        );
        assert_contract(&line);
    }

    #[test]
    fn each_way_a_session_ends_has_its_summary() {
        use EndReason::*;
        let usb = SessionEnd {
            link: LinkId::Usb { device_id: 3 },
            ..end(LinkLost, Some(800), 5_000)
        };
        let cases = [
            (
                end(Goodbye(ByeReason::UserDisconnect), None, 900),
                ["accepted", "user_ended", "none", "goodbye"],
            ),
            (
                end(Goodbye(ByeReason::AppBackground), Some(300), 4_000),
                ["first_frame", "streamed", "none", "goodbye"],
            ),
            // Video went out, but no report counted a frame.
            (
                SessionEnd {
                    frames_received: false,
                    ..end(Goodbye(ByeReason::UserDisconnect), Some(300), 60_000)
                },
                ["video_sent", "streamed", "none", "goodbye"],
            ),
            (
                end(Goodbye(ByeReason::Superseded), None, 900),
                ["accepted", "app_ended", "none", "goodbye"],
            ),
            // The iPad gave up without a picture.
            (
                end(Goodbye(ByeReason::Error), None, 10_000),
                ["accepted", "no_picture", "no_video", "goodbye"],
            ),
            (
                end(Goodbye(ByeReason::Error), Some(500), 30_000),
                ["steady", "streamed", "other", "goodbye"],
            ),
            (
                end(Liveness, None, 3_000),
                ["accepted", "no_picture", "no_video", "liveness"],
            ),
            (
                end(Liveness, Some(250), 600_000),
                ["steady", "streamed", "signal_lost", "liveness"],
            ),
            (
                end(Superseded, Some(250), 9_000),
                ["first_frame", "streamed", "none", "superseded"],
            ),
            (
                end(Superseded, None, 100),
                ["accepted", "app_ended", "none", "superseded"],
            ),
            (
                end(Shutdown, Some(250), 20_000),
                ["steady", "streamed", "none", "shutdown"],
            ),
            (
                end(Shutdown, None, 20),
                ["accepted", "app_ended", "none", "shutdown"],
            ),
            (usb, ["first_frame", "streamed", "usb_lost", "error"]),
            (
                end(LinkLost, None, 100),
                ["accepted", "error", "other", "error"],
            ),
            (
                end(Revoked, None, 100),
                ["accepted", "pairing", "none", "error"],
            ),
            (
                end(Refused(HelloStatus::Unauthorized), None, 0),
                ["hello_sent", "pairing", "none", "error"],
            ),
            (
                end(Refused(HelloStatus::RateLimited), None, 0),
                ["hello_sent", "pairing", "none", "error"],
            ),
            (
                end(Refused(HelloStatus::Busy), None, 0),
                ["hello_sent", "error", "other", "error"],
            ),
            (
                end(Refused(HelloStatus::VersionUnsupported), None, 0),
                ["hello_sent", "error", "other", "error"],
            ),
            (
                end(Refused(HelloStatus::Error), None, 0),
                ["hello_sent", "error", "other", "error"],
            ),
        ];
        for (end, expected) in cases {
            let line = session_summary(&end, false, None, &pc());
            assert_eq!(tokens(&line), expected, "{end:?}");
            assert_contract(&line);
        }
        let refused = session_summary(
            &end(Refused(HelloStatus::Busy), None, 0),
            true,
            Some("no_frames"),
            &pc(),
        );
        assert!(
            refused.contains(" session_id=- ")
                && refused.contains(" fallback=none ")
                && refused.contains(" ttff_ms=- "),
            "{refused}"
        );
    }

    #[test]
    fn summary_numbers_stay_in_the_contracts_range() {
        let long = SessionEnd {
            session_id: Some(u32::MAX),
            client_nonce: u32::MAX,
            first_video: Some(Duration::from_secs(4_000)),
            duration: Duration::from_secs(40 * 86_400),
            ..end(EndReason::Liveness, None, 0)
        };
        let line = session_summary(&long, false, None, &pc());
        assert!(line.contains(" nonce=00000000ffffffff "), "{line}");
        assert!(
            line.contains(" ttff_ms=600000 duration_ms=2592000000 "),
            "{line}"
        );
        assert_contract(&line);
        let unknown = HostFacts {
            os: os_token("unavailable(2)", "9457"),
            gpu: gpu_token(&[]),
            hybrid: None,
        };
        let line = session_summary(&end(EndReason::Shutdown, None, 0), false, None, &unknown);
        assert!(line.ends_with(" os=win:0.0 hw=pc gpu=- hybrid=-"), "{line}");
        assert_contract(&line);
    }

    #[test]
    fn the_pc_is_named_by_build_and_graphics_card_makers_only() {
        assert_eq!(os_token("26200", "9457"), "win:26200.9457");
        assert_eq!(os_token(" 22631 ", "4890"), "win:22631.4890");
        for (build, ubr) in [
            ("", "1"),
            ("26200", "unavailable(2)"),
            ("1234567", "1"),
            ("26200", "-1"),
        ] {
            assert_eq!(os_token(build, ubr), "win:0.0");
        }
        let intel = (0x8086, 0x46a6);
        let nvidia = (0x10de, 0x25a2);
        let basic_render = (0x1414, 0x8c);
        assert_eq!(
            gpu_token(&[nvidia, intel, intel, basic_render]),
            "Intel+NVIDIA"
        );
        assert_eq!(
            gpu_token(&[(0x1002, 1), (0x5143, 2), nvidia, intel]),
            "AMD+Intel+NVIDIA+other"
        );
        assert_eq!(gpu_token(&[(0x1022, 1)]), "AMD");
        assert_eq!(gpu_token(&[basic_render]), "-");
        assert_eq!(hybrid(&[intel, nvidia, basic_render]), Some(true));
        // Windows lists some laptops' integrated card twice.
        assert_eq!(hybrid(&[intel, intel, basic_render]), Some(false));
        assert_eq!(hybrid(&[]), None);
    }

    #[test]
    fn a_session_keeps_the_first_second_screen_fallback_it_logs() {
        use tracing_subscriber::prelude::*;
        for (reason, token) in [
            ("attach_timeout", "attach_timeout"),
            ("enable_failed", "enable_failed"),
            ("no_driver", "no_driver"),
            ("no_frames", "no_frames"),
            ("duplication_failed", "duplication_failed"),
            ("remembered", "remembered"),
            ("C:\\Users\\someone", "other"),
        ] {
            assert_eq!(fallback_token(reason), token);
        }
        let _slot = FALLBACK_SLOT.lock();
        let shared = SharedControl::new(9876, 15_000_000);
        *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
        let subscriber = tracing_subscriber::registry().with(FallbackWatch);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(second_screen_fallback = "remembered", "before the session");
            note_session_start(1, &shared);
            tracing::info!("no reason here");
            tracing::info!(
                second_screen_fallback = "remembered",
                reason = "attach_timeout",
                "held"
            );
            tracing::warn!(second_screen_fallback = %"no_frames", "later");
            assert_eq!(take_fallback(2), None, "another session's");
            assert_eq!(take_fallback(1), Some("remembered"));
            tracing::warn!(second_screen_fallback = "enable_failed", "after it ended");
            assert_eq!(take_fallback(1), None);
            note_session_start(3, &shared);
            tracing::warn!(second_screen_fallback = %"something new", "unknown");
            assert_eq!(take_fallback(3), Some("other"));
        });
    }

    /// A fallback counts for the live session in the mode it has when the
    /// fallback is logged. Session 1 starts the virtual display and leaves,
    /// and its start fails late: neither the gap before session 2 nor a
    /// session 3 that mirrors gets that. Session 5 falls back, then the user
    /// picks Mirror; session 6 mirrors through a failure, then picks Second
    /// screen.
    #[test]
    fn a_late_fallback_of_an_ended_session_is_not_the_next_ones() {
        use tracing_subscriber::prelude::*;
        let _slot = FALLBACK_SLOT.lock();
        let summaries = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let subscriber = tracing_subscriber::registry()
            .with(FallbackWatch)
            .with(SummaryCapture(summaries.clone()));
        let shared = SharedControl::new(9876, 15_000_000);
        let ended = |id| SessionEnd {
            session_id: Some(id),
            ..end(EndReason::Goodbye(ByeReason::UserDisconnect), None, 1_000)
        };
        tracing::subscriber::with_default(subscriber, || {
            *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
            note_session_start(1, &shared);
            log_session_end(&ended(1), &shared);
            tracing::warn!(second_screen_fallback = "enable_failed", "in the gap");
            note_session_start(2, &shared);
            log_session_end(&ended(2), &shared);
            note_session_start(3, &shared);
            *shared.capture_target.lock() = CaptureTarget::PrimaryAuto;
            tracing::warn!(
                second_screen_fallback = "enable_failed",
                "session 1's start"
            );
            log_session_end(&ended(3), &shared);
            // The same reason for a session in Second screen mode is its own:
            // it mirrors because the start it waited on failed.
            *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
            note_session_start(4, &shared);
            tracing::warn!(
                second_screen_fallback = "enable_failed",
                "the start it waited on"
            );
            log_session_end(&ended(4), &shared);
            note_session_start(5, &shared);
            tracing::warn!(second_screen_fallback = "attach_timeout", "fell back");
            *shared.capture_target.lock() = CaptureTarget::PrimaryAuto;
            tracing::warn!(second_screen_fallback = "no_frames", "while mirroring");
            log_session_end(&ended(5), &shared);
            note_session_start(6, &shared);
            tracing::info!(second_screen_fallback = "remembered", "while mirroring");
            *shared.capture_target.lock() = CaptureTarget::VirtualExtended;
            log_session_end(&ended(6), &shared);
        });
        let modes: Vec<_> = summaries
            .lock()
            .iter()
            .map(|line| {
                assert_contract(line);
                ["mode=", "fallback="]
                    .map(|key| {
                        line.split(' ')
                            .find_map(|word| word.strip_prefix(key))
                            .unwrap()
                    })
                    .join(" ")
            })
            .collect();
        assert_eq!(
            modes,
            [
                "second_screen none",
                "second_screen none",
                "mirror none",
                "second_screen enable_failed",
                "mirror attach_timeout",
                "second_screen none",
            ]
        );
    }

    #[test]
    fn windows_11_is_named_from_its_build_not_its_registry_label() {
        // The customer's PC: Windows 11 26H2 with the legacy label.
        assert_eq!(windows_name("Windows 10 Home", "26300"), "Windows 11 Home");
        assert_eq!(windows_name("Windows 10 Pro", "22000"), "Windows 11 Pro");
        assert_eq!(
            windows_name("Windows 10 Home China", "26200"),
            "Windows 11 Home China"
        );
        assert_eq!(windows_name("Windows 10 Pro", "19045"), "Windows 10 Pro");
        assert_eq!(windows_name("Windows 10 Pro", "21996"), "Windows 10 Pro");
        // Already right, or unreadable: left as Windows reports it.
        assert_eq!(windows_name("Windows 11 Pro", "26100"), "Windows 11 Pro");
        assert_eq!(
            windows_name("Windows 10 Home", "unavailable(2)"),
            "Windows 10 Home"
        );
    }
}
