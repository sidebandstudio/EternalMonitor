//! Chooses the graphics adapter for the window. wgpu takes the first adapter
//! of the preferred GPU type, which is Vulkan on Windows. Old Intel drivers
//! (UHD 620 with 23.20.16.x, issue #59) offer a Vulkan adapter that lacks
//! WebGPU features, and the process dies inside the driver while creating the
//! device, with no error to catch. Prefer a fully capable adapter of the same
//! GPU type, usually DX12 there.
//!
//! Each launch also leaves a marker naming its GPU, driver and backend until
//! the window is confirmed working. A marker whose process has exited means
//! that launch never got there, so that backend is avoided on that GPU and
//! driver, and tried again after some clean launches or a driver change.
//!
//! On Windows the instance is created without OpenGL. Listing GL adapters
//! makes a context on the display GPU, and AMD's OpenGL driver can crash the
//! process there before any adapter is chosen. Vulkan, DX12 and WARP cover
//! every Windows 10 PC. A launch that dies while wgpu lists adapters leaves a
//! marker for that stage too, and the next launch narrows the instance to
//! DX12, then to WARP.
//!
//! Settings > Troubleshooting > Window drawing picks DX12, Vulkan or WARP for
//! the next start, in place of the WGPU_BACKEND variable (which still wins).
//! A chosen backend leaves the same markers; one that dies is set aside and
//! the automatic choice draws the window, so a bad choice can't lock a user
//! out of the setting.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
use eframe::wgpu::{self, Backend, Backends, DeviceType};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::startup::ProcessId;
use crate::troubleshooting::WindowDrawing;

/// Clean launches, per recorded failure, before an avoided backend is retried.
const RETRY_AFTER: u32 = 5;
const HISTORY_FILE: &str = "history.json";
/// Held while one instance reads, changes and saves the history.
const HISTORY_LOCK: &str = "history.lock";
/// The window counts as working after this long and this many frames with no
/// surface error. A driver that cannot draw fails while creating the device or
/// at the first present, within milliseconds.
const CONFIRM_AFTER: Duration = Duration::from_secs(3);
const CONFIRM_FRAMES: u64 = 10;

static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();
static STARTED: Mutex<Option<Marker>> = Mutex::new(None);
static PRESENT: Mutex<PresentTracker> = Mutex::new(PresentTracker::new());
/// Frames whose surface texture could not be acquired, so nothing was presented.
static FAILED_ACQUIRES: AtomicU64 = AtomicU64::new(0);
static STATUS: Mutex<Option<Status>> = Mutex::new(None);

/// How this launch draws its window, for Settings > Troubleshooting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The saved Window drawing choice.
    pub choice: WindowDrawing,
    /// WGPU_BACKEND decides, so the choice does not apply.
    pub from_env: bool,
    /// The chosen backend failed to start the window before, so the
    /// automatic choice draws it.
    pub chosen_failed: bool,
    /// Crash avoidance set a backend aside after a launch that never showed
    /// its window.
    pub avoiding: bool,
    /// What draws the window now, in words.
    pub backend: Option<String>,
}

/// How this launch draws its window. `None` before the window starts.
pub fn status() -> Option<Status> {
    STATUS.lock().ok().and_then(|status| status.clone())
}

fn update_status(change: impl FnOnce(&mut Status)) {
    if let Ok(mut status) = STATUS.lock() {
        change(status.get_or_insert_with(Status::default));
    }
}

/// Forget every backend crash avoidance set aside; the next start tries
/// them all again.
pub fn reset_avoidance() {
    if let Some(directory) = STATE_DIR.get() {
        update_history(directory, Vec::clear);
    }
    update_status(|status| {
        status.avoiding = false;
        status.chosen_failed = false;
    });
    info!("Window renderer crash avoidance reset; applies at the next start");
}

/// The backends the instance is created with: the automatic plans, from
/// widest to safest, then the one only a user's choice picks.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Plan {
    Full,
    Dx12,
    Warp,
    Vulkan,
}

impl Plan {
    const ALL: [Plan; 3] = [Plan::Full, Plan::Dx12, Plan::Warp];

    /// The marker backend of a launch still listing adapters with this plan.
    fn stage(self) -> &'static str {
        match self {
            Plan::Full => "enumerate",
            Plan::Dx12 => "enumerate dx12",
            Plan::Warp => "enumerate warp",
            Plan::Vulkan => "enumerate vulkan",
        }
    }

    /// Windows only; elsewhere the instance keeps egui's backends.
    fn backends(self) -> Backends {
        match self {
            Plan::Full => Backends::DX12 | Backends::VULKAN,
            Plan::Dx12 | Plan::Warp => Backends::DX12,
            Plan::Vulkan => Backends::VULKAN,
        }
    }

    /// The plan a Window drawing choice asks for.
    fn chosen(choice: WindowDrawing) -> Option<Plan> {
        match choice {
            WindowDrawing::Automatic => None,
            WindowDrawing::Dx12 => Some(Plan::Dx12),
            WindowDrawing::Vulkan => Some(Plan::Vulkan),
            WindowDrawing::Warp => Some(Plan::Warp),
        }
    }

    /// Whether a failure recorded while drawing (a backend on one adapter)
    /// was this plan's. Microsoft's software adapter (1414:008c) is WARP.
    fn drew_with(self, failure: &Failure) -> bool {
        let warp = failure.gpu.starts_with("1414:");
        match self {
            Plan::Dx12 => failure.backend == "dx12" && !warp,
            Plan::Warp => failure.backend == "dx12" && warp,
            Plan::Vulkan => failure.backend == "vulkan",
            Plan::Full => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Candidate {
    backend: Backend,
    device_type: DeviceType,
    compliant: bool,
    avoided: bool,
}

/// What one launch is trying to start with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Marker {
    gpu: String,
    backend: String,
}

/// A backend that failed to draw on one GPU and driver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Failure {
    gpu: String,
    backend: String,
    failures: u32,
    clean_launches: u32,
}

/// Choose the instance backends and install the adapter selector, after
/// recording failed earlier starts.
pub(super) fn configure(config: &mut WgpuConfiguration) {
    let history = match crate::settings::app_data_dir() {
        Some(base) => {
            let directory = base.join("renderer");
            let _ = fs::create_dir_all(&directory);
            let history = sweep(&directory, ProcessId::current());
            let _ = STATE_DIR.set(directory);
            history
        }
        None => Vec::new(),
    };
    let choice = crate::settings::SettingsFile::load().window_drawing;
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        let plan = choose_backends(&mut setup.instance_descriptor, &history, choice);
        setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
            select(adapters, surface, &history, plan)
        }));
    }
    let default = config.on_surface_status.clone();
    config.on_surface_status = Arc::new(move |status| {
        FAILED_ACQUIRES.fetch_add(1, Ordering::SeqCst);
        default(status)
    });
}

/// `WGPU_BACKEND` decides when set, as in wgpu, and nothing is narrowed or
/// recorded then. Otherwise the saved choice, unless it failed before.
fn choose_backends(
    descriptor: &mut wgpu::InstanceDescriptor,
    history: &[Failure],
    choice: WindowDrawing,
) -> Plan {
    let forced = Backends::from_env();
    let mut plan = Plan::Full;
    let mut chosen_failed = false;
    if cfg!(windows) {
        if forced.is_none() {
            let gpus = system_gpus();
            (plan, chosen_failed) = chosen_plan(history, &gpus, choice);
            start(Marker {
                gpu: gpus,
                backend: plan.stage().to_string(),
            });
        }
        descriptor.backends = instance_backends(forced, plan);
    }
    update_status(|status| {
        status.choice = choice;
        status.from_env = forced.is_some();
        status.chosen_failed = chosen_failed;
        status.avoiding = history.iter().any(|f| avoided(history, &f.gpu, &f.backend));
    });
    info!(
        backends = ?descriptor.backends,
        from_env = forced.is_some(),
        choice = ?choice,
        chosen_failed,
        narrowed = plan != Plan::Full && Plan::chosen(choice) != Some(plan),
        "Window renderer instance"
    );
    plan
}

/// The plan for a saved choice, or the automatic one when the choice is
/// Automatic or its backend failed to start the window before (`true`).
fn chosen_plan(history: &[Failure], gpus: &str, choice: WindowDrawing) -> (Plan, bool) {
    match Plan::chosen(choice) {
        Some(plan)
            if avoided(history, gpus, plan.stage())
                || history
                    .iter()
                    .any(|f| plan.drew_with(f) && avoided(history, &f.gpu, &f.backend)) =>
        {
            (instance_plan(history, gpus), true)
        }
        Some(plan) => (plan, false),
        None => (instance_plan(history, gpus), false),
    }
}

fn instance_backends(forced: Option<Backends>, plan: Plan) -> Backends {
    forced.unwrap_or(plan.backends())
}

/// The widest plan that has not died while listing adapters on these GPUs and
/// drivers.
fn instance_plan(history: &[Failure], gpus: &str) -> Plan {
    Plan::ALL
        .into_iter()
        .find(|plan| !avoided(history, gpus, plan.stage()))
        .unwrap_or(Plan::Warp)
}

/// Leave this launch's marker until the window is confirmed working.
fn start(marker: Marker) {
    if let Some(directory) = STATE_DIR.get() {
        if let Ok(json) = serde_json::to_vec(&marker) {
            let _ = fs::write(marker_path(directory, ProcessId::current()), json);
        }
    }
    if let Ok(mut started) = STARTED.lock() {
        *started = Some(marker);
    }
}

/// Called at the start of each UI pass; clears this launch's marker once the
/// window has kept drawing without a surface error.
pub(super) fn frame_started(ctx: &egui::Context) {
    crate::startup::simulate_surface_error(&FAILED_ACQUIRES);
    let visible = ctx.input(|input| input.viewport().minimized != Some(true));
    let Ok(mut tracker) = PRESENT.lock() else {
        return;
    };
    let now_drawing = tracker.observe(
        ctx.cumulative_frame_nr(),
        FAILED_ACQUIRES.load(Ordering::SeqCst),
        visible,
        Instant::now(),
    );
    let drawing = tracker.confirmed;
    drop(tracker);
    if now_drawing {
        crate::startup::mark_window_shown();
        info!("Window renderer is drawing");
    }
    if drawing {
        clear_marker(true);
    }
}

/// The window closed normally. A launch that quits before its window is
/// confirmed did not fail, so its marker goes without counting either way.
pub(super) fn window_closed() {
    clear_marker(false);
}

/// Clear this launch's marker, after recording that its backend drew when
/// `drew`. The history may be busy with another instance; the next frame
/// tries again, so the GUI never waits for it.
fn clear_marker(drew: bool) {
    let (Some(directory), Ok(mut started)) = (STATE_DIR.get(), STARTED.lock()) else {
        return;
    };
    if let Some(marker) = started.as_ref() {
        if finish_launch(directory, marker, ProcessId::current(), drew) {
            started.take();
        }
    }
}

fn finish_launch(directory: &Path, marker: &Marker, owner: ProcessId, drew: bool) -> bool {
    if drew && update_history(directory, |history| record_success(history, marker)).is_none() {
        return false;
    }
    let _ = fs::remove_file(marker_path(directory, owner));
    true
}

/// Confirms a working window from the frame counter, the time and surface
/// errors. egui-wgpu presents inside its painter and reports nothing after a
/// present; a surface it fails to recreate is not reported at all. So the
/// window must keep drawing for a while after the last reported error, which
/// takes it past device creation and the first presents, where the drivers of
/// issue #59 fail.
struct PresentTracker {
    span: Option<Span>,
    confirmed: bool,
}

/// Visible frames since the last surface error.
#[derive(Clone, Copy)]
struct Span {
    since: Instant,
    first_frame: u64,
    failures: u64,
}

impl PresentTracker {
    const fn new() -> Self {
        Self {
            span: None,
            confirmed: false,
        }
    }

    /// True once: after [`CONFIRM_AFTER`] and [`CONFIRM_FRAMES`] of visible
    /// frames with no new surface error. A surface error or a minimized
    /// window starts the span again.
    fn observe(&mut self, frame: u64, failures: u64, visible: bool, now: Instant) -> bool {
        if self.confirmed {
            return false;
        }
        match self.span {
            Some(span) if visible && span.failures == failures => {
                if frame >= span.first_frame + CONFIRM_FRAMES
                    && now.duration_since(span.since) >= CONFIRM_AFTER
                {
                    self.confirmed = true;
                    return true;
                }
            }
            _ => {
                self.span = visible.then_some(Span {
                    since: now,
                    first_frame: frame,
                    failures,
                });
            }
        }
        false
    }
}

fn select(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    history: &[Failure],
    plan: Plan,
) -> Result<wgpu::Adapter, String> {
    let usable: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .filter(|adapter| plan != Plan::Warp || adapter.get_info().device_type == DeviceType::Cpu)
        .collect();
    let gpus: Vec<String> = usable.iter().map(|a| gpu_key(&a.get_info())).collect();
    let candidates: Vec<Candidate> = usable
        .iter()
        .zip(&gpus)
        .map(|(adapter, gpu)| {
            let info = adapter.get_info();
            Candidate {
                backend: info.backend,
                device_type: info.device_type,
                compliant: adapter.get_downlevel_capabilities().is_webgpu_compliant(),
                avoided: avoided(history, gpu, info.backend.to_str()),
            }
        })
        .collect();
    let index = pick(&candidates)
        .ok_or_else(|| "No graphics adapter can draw the EternalMonitor window".to_string())?;
    let adapter = usable[index].clone();
    let info = adapter.get_info();
    let marker = Marker {
        gpu: gpus[index].clone(),
        backend: info.backend.to_str().to_string(),
    };
    info!(
        backend = %info.backend,
        adapter = %info.name,
        gpu = %marker.gpu,
        compliant = candidates[index].compliant,
        avoided = ?history.iter().filter(|f| avoided(history, &f.gpu, &f.backend)).map(|f| (&f.gpu, &f.backend)).collect::<Vec<_>>(),
        "Window renderer selected"
    );
    let drawing = match info.backend {
        _ if info.device_type == DeviceType::Cpu => "the software renderer (WARP)",
        Backend::Dx12 => "DirectX 12",
        Backend::Vulkan => "Vulkan",
        Backend::Metal => "Metal",
        Backend::Gl => "OpenGL",
        other => other.to_str(),
    };
    update_status(|status| status.backend = Some(drawing.to_string()));
    start(marker);
    crate::startup::simulate_renderer_failure()?;
    Ok(adapter)
}

/// The best candidate: not avoided, then wgpu's high-performance GPU type order,
/// then fully capable, then wgpu's own backend order.
fn pick(candidates: &[Candidate]) -> Option<usize> {
    let type_rank = |device_type| match device_type {
        DeviceType::DiscreteGpu => 0,
        DeviceType::IntegratedGpu => 1,
        DeviceType::Other => 2,
        DeviceType::VirtualGpu => 3,
        DeviceType::Cpu => 4,
    };
    let backend_rank = |backend| match backend {
        Backend::Vulkan | Backend::Metal => 0,
        Backend::Dx12 => 1,
        Backend::Gl => 2,
        _ => 3,
    };
    candidates
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| {
            (
                c.avoided,
                type_rank(c.device_type),
                !c.compliant,
                backend_rank(c.backend),
            )
        })
        .map(|(index, _)| index)
}

/// The GPU and its driver version, so a driver update gives every backend a
/// fresh start.
fn gpu_key(info: &wgpu::AdapterInfo) -> String {
    let driver = driver_version(info.vendor, info.device).unwrap_or_else(|| {
        format!("{} {}", info.driver, info.driver_info)
            .trim()
            .to_string()
    });
    format!("{:04x}:{:04x} {driver}", info.vendor, info.device)
}

/// The driver version DXGI reports for the adapter wgpu describes.
fn driver_version(vendor: u32, device: u32) -> Option<String> {
    dxgi_adapters()
        .into_iter()
        .find(|(v, d, _)| *v == vendor && *d == device)
        .map(|(_, _, driver)| driver)
}

/// Every GPU and driver DXGI lists, as wgpu lists them before choosing one.
fn system_gpus() -> String {
    dxgi_adapters()
        .iter()
        .map(|(vendor, device, driver)| format!("{vendor:04x}:{device:04x} {driver}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Vendor, device and driver version of each DXGI adapter. Creates no device.
#[cfg(windows)]
fn dxgi_adapters() -> Vec<(u32, u32, String)> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    (0..)
        .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
        .filter_map(|adapter| {
            let desc = unsafe { adapter.GetDesc1() }.ok()?;
            let version = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.ok()? as u64;
            let driver = format!(
                "{}.{}.{}.{}",
                version >> 48,
                (version >> 32) & 0xffff,
                (version >> 16) & 0xffff,
                version & 0xffff
            );
            Some((desc.VendorId, desc.DeviceId, driver))
        })
        .collect()
}

#[cfg(not(windows))]
fn dxgi_adapters() -> Vec<(u32, u32, String)> {
    Vec::new()
}

fn avoided(history: &[Failure], gpu: &str, backend: &str) -> bool {
    history.iter().any(|f| {
        f.gpu == gpu && f.backend == backend && f.clean_launches < RETRY_AFTER * f.failures
    })
}

fn record_failure(history: &mut Vec<Failure>, marker: &Marker) {
    match history
        .iter_mut()
        .find(|f| f.gpu == marker.gpu && f.backend == marker.backend)
    {
        Some(failure) => {
            failure.failures += 1;
            failure.clean_launches = 0;
        }
        None => history.push(Failure {
            gpu: marker.gpu.clone(),
            backend: marker.backend.clone(),
            failures: 1,
            clean_launches: 0,
        }),
    }
}

/// A backend that drew is cleared; every other failure moves toward a retry,
/// also when this launch drew on another GPU or on WARP, so a fallback never
/// keeps the failed backend avoided for good.
fn record_success(history: &mut Vec<Failure>, marker: &Marker) {
    history.retain(|f| !(f.gpu == marker.gpu && f.backend == marker.backend));
    for failure in history.iter_mut() {
        failure.clean_launches += 1;
    }
}

fn marker_path(directory: &Path, owner: ProcessId) -> PathBuf {
    directory.join(format!("{owner}.start"))
}

fn read_history(directory: &Path) -> Vec<Failure> {
    fs::read(directory.join(HISTORY_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Read, change and save the history as one step, so instances starting
/// together do not lose each other's updates. Returns the changed history, or
/// `None` without waiting when another instance is updating it.
fn update_history(
    directory: &Path,
    change: impl FnOnce(&mut Vec<Failure>),
) -> Option<Vec<Failure>> {
    let _lock = match crate::startup::lock::try_lock(&directory.join(HISTORY_LOCK)) {
        Ok(lock) => lock,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::WouldBlock {
                warn!(%error, "Could not lock the renderer history");
            }
            return None;
        }
    };
    let mut history = read_history(directory);
    let before = history.clone();
    change(&mut history);
    if history != before {
        write_history(directory, &history);
    }
    Some(history)
}

fn write_history(directory: &Path, history: &[Failure]) {
    let path = directory.join(HISTORY_FILE);
    let temp = directory.join(format!("history.{}.tmp", ProcessId::current()));
    let saved = serde_json::to_vec(history)
        .map_err(std::io::Error::other)
        .and_then(|json| fs::write(&temp, json))
        .and_then(|()| fs::rename(&temp, &path));
    if let Err(error) = saved {
        warn!(%error, "Could not save the renderer history");
    }
}

/// Count markers left by exited launches as failures and remove them. Markers
/// of running instances, including this one, are theirs to clear. When another
/// instance is updating the history, it sweeps them; use the history as saved.
fn sweep(directory: &Path, current: ProcessId) -> Vec<Failure> {
    update_history(directory, |history| {
        for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(owner) = name.strip_suffix(".start").and_then(ProcessId::from_name) else {
                continue;
            };
            if owner == current || owner.is_running() {
                continue;
            }
            if let Some(marker) = fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
            {
                warn!(
                    gpu = %marker.gpu,
                    backend = %marker.backend,
                    "A previous launch closed before its window appeared; avoiding that renderer for now"
                );
                record_failure(history, &marker);
            }
            let _ = fs::remove_file(entry.path());
        }
    })
    .unwrap_or_else(|| read_history(directory))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(backend: Backend, device_type: DeviceType, compliant: bool) -> Candidate {
        Candidate {
            backend,
            device_type,
            compliant,
            avoided: false,
        }
    }

    /// The adapters wgpu listed on the UHD 620 host from issue #59, less the
    /// OpenGL one the instance no longer lists.
    fn uhd_620() -> Vec<Candidate> {
        vec![
            candidate(Backend::Vulkan, DeviceType::IntegratedGpu, false),
            candidate(Backend::Dx12, DeviceType::IntegratedGpu, true),
            candidate(Backend::Dx12, DeviceType::Cpu, true),
        ]
    }

    fn avoiding(mut candidates: Vec<Candidate>, backends: &[Backend]) -> Vec<Candidate> {
        for c in &mut candidates {
            c.avoided = backends.contains(&c.backend);
        }
        candidates
    }

    fn temp_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "eternal-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn exited_process() -> ProcessId {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/c", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let id = ProcessId {
            pid: child.id(),
            start: 1,
        };
        child.wait().unwrap();
        id
    }

    fn marker(gpu: &str, backend: &str) -> Marker {
        Marker {
            gpu: gpu.into(),
            backend: backend.into(),
        }
    }

    #[test]
    fn an_incomplete_vulkan_driver_yields_to_dx12_on_the_same_gpu() {
        assert_eq!(pick(&uhd_620()), Some(1));
    }

    #[test]
    fn capable_vulkan_stays_the_default() {
        let candidates = [
            candidate(Backend::Vulkan, DeviceType::IntegratedGpu, true),
            candidate(Backend::Dx12, DeviceType::IntegratedGpu, true),
        ];
        assert_eq!(pick(&candidates), Some(0));
    }

    #[test]
    fn gpu_type_outranks_capability() {
        let candidates = [
            candidate(Backend::Dx12, DeviceType::IntegratedGpu, true),
            candidate(Backend::Vulkan, DeviceType::DiscreteGpu, false),
        ];
        assert_eq!(pick(&candidates), Some(1));
    }

    #[test]
    fn a_backend_that_failed_before_is_tried_last() {
        assert_eq!(pick(&avoiding(uhd_620(), &[Backend::Dx12])), Some(0));
        // Both backends of the GPU failed: WARP draws.
        let mut candidates = uhd_620();
        candidates[0].avoided = true;
        candidates[1].avoided = true;
        assert_eq!(pick(&candidates), Some(2));
        // With everything avoided, the usual order applies again.
        let all = [Backend::Dx12, Backend::Vulkan];
        assert_eq!(pick(&avoiding(uhd_620(), &all)), Some(1));
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn a_failed_backend_is_retried_after_clean_launches_or_a_driver_update() {
        let old = marker("8086:5917 23.20.16.5038", "vulkan");
        let fallback = marker("8086:5917 23.20.16.5038", "dx12");
        let mut history = Vec::new();
        record_failure(&mut history, &old);
        assert!(avoided(&history, &old.gpu, "vulkan"));
        assert!(!avoided(&history, &old.gpu, "dx12"));
        // A newer driver for the same GPU starts clean.
        assert!(!avoided(&history, "8086:5917 31.0.101.2127", "vulkan"));
        for _ in 0..RETRY_AFTER - 1 {
            record_success(&mut history, &fallback);
        }
        assert!(avoided(&history, &old.gpu, "vulkan"));
        record_success(&mut history, &fallback);
        assert!(!avoided(&history, &old.gpu, "vulkan"));
        // Failing again waits twice as long.
        record_failure(&mut history, &old);
        for _ in 0..2 * RETRY_AFTER - 1 {
            record_success(&mut history, &fallback);
        }
        assert!(avoided(&history, &old.gpu, "vulkan"));
        // Drawing with it clears the entry.
        record_success(&mut history, &old);
        assert!(history.is_empty());
    }

    #[test]
    fn only_markers_of_exited_launches_count_as_failures() {
        let directory = temp_dir("renderer");
        let gpu = "8086:5917 23.20.16.5038";
        // A second instance still starting: this test process stands in for it.
        let running = ProcessId::current();
        let elsewhere = ProcessId {
            pid: running.pid,
            start: running.start + 1,
        };
        let json = |m: &Marker| serde_json::to_vec(m).unwrap();
        fs::write(
            marker_path(&directory, running),
            json(&marker(gpu, "vulkan")),
        )
        .unwrap();
        let dead = exited_process();
        fs::write(marker_path(&directory, dead), json(&marker(gpu, "dx12"))).unwrap();

        let history = sweep(&directory, elsewhere);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].backend, "dx12");
        assert!(marker_path(&directory, running).exists());
        assert!(!marker_path(&directory, dead).exists());
        // Recorded once, and kept for the next launch.
        assert_eq!(sweep(&directory, elsewhere), history);
        assert_eq!(read_history(&directory), history);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_windows_instance_leaves_out_opengl_unless_wgpu_backend_asks() {
        let full = instance_backends(None, Plan::Full);
        assert_eq!(full, Backends::DX12 | Backends::VULKAN);
        assert!(!full.contains(Backends::GL));
        // WGPU_BACKEND is read as wgpu reads it and wins over any narrowing.
        let dx12 = Backends::from_comma_list("dx12");
        assert_eq!(instance_backends(Some(dx12), Plan::Full), Backends::DX12);
        let gl = Backends::from_comma_list("gl");
        assert_eq!(instance_backends(Some(gl), Plan::Warp), Backends::GL);
    }

    #[test]
    fn a_launch_that_died_listing_adapters_narrows_the_next_one() {
        let directory = temp_dir("renderer-enumerate");
        let gpus = "1002:150e 32.0.21025.1024, 10de:2d59 32.0.15.9186";
        assert_eq!(instance_plan(&[], gpus), Plan::Full);
        let crash = |plan: Plan| {
            let dead = exited_process();
            let json = serde_json::to_vec(&marker(gpus, plan.stage())).unwrap();
            fs::write(marker_path(&directory, dead), json).unwrap();
            sweep(&directory, ProcessId::current())
        };
        let history = crash(Plan::Full);
        assert_eq!(instance_plan(&history, gpus), Plan::Dx12);
        assert_eq!(instance_backends(None, Plan::Dx12), Backends::DX12);
        let history = crash(Plan::Dx12);
        assert_eq!(instance_plan(&history, gpus), Plan::Warp);
        // Other GPUs or a driver update start with every backend.
        assert_eq!(
            instance_plan(&history, "1002:150e 32.0.21030.2001"),
            Plan::Full
        );
        // Everything failed: stay on the safest set.
        let history = crash(Plan::Warp);
        assert_eq!(instance_plan(&history, gpus), Plan::Warp);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_chosen_backend_draws_until_it_fails_then_automatic_takes_over() {
        let gpus = "1002:150e 32.0.21025.1024, 10de:2d59 32.0.15.9186";
        assert_eq!(
            chosen_plan(&[], gpus, WindowDrawing::Automatic),
            (Plan::Full, false)
        );
        assert_eq!(
            chosen_plan(&[], gpus, WindowDrawing::Dx12),
            (Plan::Dx12, false)
        );
        assert_eq!(
            chosen_plan(&[], gpus, WindowDrawing::Vulkan),
            (Plan::Vulkan, false)
        );
        assert_eq!(
            chosen_plan(&[], gpus, WindowDrawing::Warp),
            (Plan::Warp, false)
        );
        assert_eq!(instance_backends(None, Plan::Vulkan), Backends::VULKAN);
        assert_eq!(instance_backends(None, Plan::Warp), Backends::DX12);
        // A Vulkan driver that died creating the device: the next start
        // draws automatically, so the user can still reach the setting.
        let mut history = Vec::new();
        record_failure(&mut history, &marker("10de:2d59 32.0.15.9186", "vulkan"));
        assert_eq!(
            chosen_plan(&history, gpus, WindowDrawing::Vulkan),
            (Plan::Full, true)
        );
        // Other choices are unaffected by it.
        assert_eq!(
            chosen_plan(&history, gpus, WindowDrawing::Dx12),
            (Plan::Dx12, false)
        );
        // Dying while listing adapters with the chosen set counts too, and
        // the automatic plan then still narrows on its own record.
        let mut history = Vec::new();
        record_failure(&mut history, &marker(gpus, Plan::Dx12.stage()));
        record_failure(&mut history, &marker(gpus, Plan::Full.stage()));
        assert_eq!(
            chosen_plan(&history, gpus, WindowDrawing::Dx12),
            (Plan::Warp, true)
        );
        // WARP is told apart from hardware DX12 by Microsoft's adapter.
        let mut history = Vec::new();
        record_failure(&mut history, &marker("1414:008c 10.0.22621.1", "dx12"));
        assert_eq!(
            chosen_plan(&history, gpus, WindowDrawing::Warp),
            (Plan::Full, true)
        );
        assert_eq!(
            chosen_plan(&history, gpus, WindowDrawing::Dx12),
            (Plan::Dx12, false)
        );
    }

    #[test]
    fn clean_launches_bring_the_full_instance_back() {
        let gpus = "1002:150e 32.0.21025.1024";
        let drew = marker("1002:150e 32.0.21025.1024", "dx12");
        let mut history = Vec::new();
        record_failure(&mut history, &marker(gpus, Plan::Full.stage()));
        for _ in 0..RETRY_AFTER - 1 {
            record_success(&mut history, &drew);
        }
        assert_eq!(instance_plan(&history, gpus), Plan::Dx12);
        record_success(&mut history, &drew);
        assert_eq!(instance_plan(&history, gpus), Plan::Full);
    }

    #[test]
    fn a_fallback_to_another_gpu_still_ages_the_failure() {
        let failed = marker("8086:5917 23.20.16.5038", "vulkan");
        let warp = marker("1414:008c 10.0.22621.1", "dx12");
        let mut history = Vec::new();
        record_failure(&mut history, &failed);
        for _ in 0..RETRY_AFTER {
            record_success(&mut history, &warp);
        }
        assert!(!avoided(&history, &failed.gpu, "vulkan"));
    }

    #[test]
    fn instances_updating_together_keep_every_failure() {
        let directory = temp_dir("renderer-race");
        let gpu = "8086:5917 23.20.16.5038";
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let directory = directory.clone();
                std::thread::spawn(move || {
                    let mut done = 0;
                    while done < 20 {
                        // Never waits: an instance that finds it busy retries later.
                        if update_history(&directory, |history| {
                            record_failure(history, &marker(gpu, "vulkan"))
                        })
                        .is_some()
                        {
                            done += 1;
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(read_history(&directory)[0].failures, 8 * 20);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_busy_history_never_holds_the_window() {
        let directory = temp_dir("renderer-busy");
        let busy = crate::startup::lock::try_lock(&directory.join(HISTORY_LOCK)).unwrap();
        // Each call on the GUI thread must return without waiting for the
        // lock. The code this replaced waited up to 2 s per call; each bound
        // stays under one such wait, with room for a loaded PC's disk (a
        // 500 ms bound on the whole sequence failed there).
        let quick = Duration::from_millis(1500);
        let started = Instant::now();
        assert_eq!(update_history(&directory, |_| {}), None);
        assert!(started.elapsed() < quick, "{:?}", started.elapsed());
        let launch = ProcessId::current();
        let drew = marker("10de:2c02 32.0.15.9186", "vulkan");
        fs::write(marker_path(&directory, launch), b"{}").unwrap();
        // Not finished: the next frame tries again.
        let started = Instant::now();
        assert!(!finish_launch(&directory, &drew, launch, true));
        assert!(started.elapsed() < quick, "{:?}", started.elapsed());
        assert!(marker_path(&directory, launch).exists());
        drop(busy);
        assert!(finish_launch(&directory, &drew, launch, true));
        assert!(!marker_path(&directory, launch).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_normal_quit_before_the_window_is_confirmed_is_not_a_failure() {
        let directory = temp_dir("renderer-quit");
        let quit = exited_process();
        let backend = marker("8086:5917 23.20.16.5038", "vulkan");
        fs::write(
            marker_path(&directory, quit),
            serde_json::to_vec(&backend).unwrap(),
        )
        .unwrap();
        assert!(finish_launch(&directory, &backend, quit, false));
        // The next launch finds nothing to count.
        assert!(sweep(&directory, ProcessId::current()).is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    /// Visible frames every 200 ms, as the GUI repaints, counted from `start`.
    fn run(
        tracker: &mut PresentTracker,
        start: Instant,
        frames: std::ops::Range<u64>,
        failures: u64,
    ) -> bool {
        frames.into_iter().any(|frame| {
            tracker.observe(
                frame,
                failures,
                true,
                start + Duration::from_millis(200 * frame),
            )
        })
    }

    #[test]
    fn the_window_works_after_frames_and_time_without_surface_errors() {
        let start = Instant::now();
        let mut tracker = PresentTracker::new();
        // Frames alone are not enough: egui may lay out one frame many times.
        for frame in 0..100 {
            assert!(!tracker.observe(frame, 0, true, start));
        }
        assert!(tracker.observe(100, 0, true, start + CONFIRM_AFTER));
        let mut tracker = PresentTracker::new();
        // Nor is time alone, with one frame laid out repeatedly.
        assert!(!tracker.observe(0, 0, true, start));
        assert!(!tracker.observe(0, 0, true, start + 10 * CONFIRM_AFTER));
        assert!(tracker.observe(CONFIRM_FRAMES, 0, true, start + CONFIRM_AFTER));
        assert!(!tracker.observe(CONFIRM_FRAMES + 1, 0, true, start + CONFIRM_AFTER));
    }

    #[test]
    fn a_lost_surface_that_is_recreated_late_is_not_a_working_window() {
        let start = Instant::now();
        let mut tracker = PresentTracker::new();
        assert!(!tracker.observe(0, 0, true, start));
        // Lost: egui reports it and recreates the surface on the next frame,
        // which can fail without a report and without a present.
        assert!(!tracker.observe(1, 1, true, start + Duration::from_millis(16)));
        assert!(!tracker.observe(2, 1, true, start + Duration::from_millis(32)));
        assert!(!tracker.observe(3, 1, true, start + Duration::from_millis(48)));
        // Confirmed only once the window kept drawing after the error.
        assert!(!run(&mut tracker, start, 4..15, 1));
        let mut tracker = PresentTracker::new();
        assert!(run(&mut tracker, start, 0..20, 0));
    }

    #[test]
    fn a_minimized_window_is_not_confirmed() {
        let start = Instant::now();
        let mut tracker = PresentTracker::new();
        assert!(!tracker.observe(0, 0, true, start));
        assert!(!tracker.observe(CONFIRM_FRAMES, 0, false, start + CONFIRM_AFTER));
        assert!(!tracker.observe(2 * CONFIRM_FRAMES, 0, true, start + CONFIRM_AFTER));
        assert!(tracker.observe(3 * CONFIRM_FRAMES, 0, true, start + 2 * CONFIRM_AFTER));
    }
}
