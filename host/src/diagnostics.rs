//! Low-frequency support evidence. Never collect credentials or user documents.
use std::ffi::CStr;
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use crate::control::SharedControl;
use crate::transport::link::PeerId;
use eternal_wire::v2::control::{parse_control, ControlMessage, Hello2, HelloStatus};
use tracing::{info, warn};

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

/// How the host answered an iPad's HELLO2.
pub fn hello_outcome(status: HelloStatus) -> &'static str {
    match status {
        HelloStatus::Ok => "accepted",
        HelloStatus::Unauthorized | HelloStatus::RateLimited => "rejected-pairing",
        HelloStatus::VersionUnsupported => "wrong-version",
        HelloStatus::Busy => "busy",
        HelloStatus::Error => "error",
    }
}

/// One line for each HELLO2 the host answers, from the ACK in `replies`, so
/// support can tell a PC that never heard the iPad (no line) from one that
/// turned it away, and why. For a pairing refusal it says whether the iPad
/// sent a code or a saved pairing, never the code or token. Retries of one
/// attempt log the same line, which the repeat collapser counts.
pub fn log_hello_answer(peer: PeerId, hello: &Hello2, replies: &[(PeerId, Vec<u8>)]) {
    let Some(status) = replies
        .iter()
        .filter(|(to, _)| *to == peer)
        .find_map(|(_, bytes)| match parse_control(bytes) {
            Ok((_, ControlMessage::HelloAck(ack))) => Some(ack.status),
            _ => None,
        })
    else {
        return;
    };
    let outcome = hello_outcome(status);
    let ipad_protocol = format!("{}-{}", hello.proto_min, hello.proto_max);
    let code_sent = hello.pairing_code != 0;
    let saved_pairing = hello.auth_token != [0; 16];
    match status {
        HelloStatus::Ok | HelloStatus::Busy | HelloStatus::Unauthorized => info!(
            %peer, %outcome, ?status, %ipad_protocol, code_sent, saved_pairing,
            "HELLO answered"
        ),
        _ => warn!(
            %peer, %outcome, ?status, %ipad_protocol, code_sent, saved_pairing,
            "HELLO answered"
        ),
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
        if level <= ffmpeg_next::ffi::AV_LOG_ERROR {
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

#[cfg(windows)]
fn windows_system_report() -> Vec<String> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let os_key = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let cpu_key = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0";
    let mut lines = vec![format!(
        // ProductName can retain "Windows 10" on Windows 11. Preserve its source
        // instead of presenting that legacy registry label as the detected OS.
        "Windows registry_product={} installation_type={} display_version={} build={}.{}",
        registry_string(os_key, "ProductName"),
        registry_string(os_key, "InstallationType"),
        registry_string(os_key, "DisplayVersion"),
        registry_string(os_key, "CurrentBuildNumber"),
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

#[cfg(windows)]
fn registry_string(key: &str, name: &str) -> String {
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
        return format!("unavailable({})", result.0);
    }
    String::from_utf16_lossy(&value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())])
        .trim()
        .to_string()
}

#[cfg(windows)]
fn registry_dword(key: &str, name: &str) -> String {
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
