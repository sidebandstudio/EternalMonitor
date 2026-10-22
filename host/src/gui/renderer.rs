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
//! A device lost while the window runs (a driver reset, 0x887A0005) is
//! recorded the same way, and the window is rebuilt with a new device.
//! egui-wgpu keeps drawing with the device it was given, and panics on the
//! next frame once that device is gone, so a lost device stops the window's
//! drawing first and closes it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
use eframe::wgpu::{self, Backend, DeviceType};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::startup::ProcessId;

/// Clean launches, per recorded failure, before an avoided backend is retried.
const RETRY_AFTER: u32 = 5;
/// A window rebuilt this many times within [`REBUILD_WINDOW`] is given up,
/// and the failure is reported.
const MAX_REBUILDS: usize = 3;
const REBUILD_WINDOW: Duration = Duration::from_secs(10 * 60);
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
/// Set when the window's device is lost, until the window is rebuilt.
static LOST: AtomicBool = AtomicBool::new(false);
/// The window's device, for the simulated loss.
static DEVICE: Mutex<Option<wgpu::Device>> = Mutex::new(None);
/// What the window draws with. Unlike [`STARTED`], kept once the window
/// works, so a later loss is recorded against it.
static IN_USE: Mutex<Option<Marker>> = Mutex::new(None);
/// Where the window was when it closed for a rebuild.
static PLACEMENT: Mutex<Option<Placement>> = Mutex::new(None);
static REBUILDS: Mutex<Vec<Instant>> = Mutex::new(Vec::new());

/// The lost window's position and size, so the new one opens in its place.
#[derive(Clone, Copy, Debug)]
struct Placement {
    outer: Option<egui::Rect>,
    inner: Option<egui::Rect>,
    maximized: bool,
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

/// Install the adapter selector, after recording failed earlier starts.
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
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
            select(adapters, surface, &history)
        }));
    }
    let default = config.on_surface_status.clone();
    config.on_surface_status = Arc::new(move |status| {
        FAILED_ACQUIRES.fetch_add(1, Ordering::SeqCst);
        // What egui-wgpu gets once the device is gone (seen before both
        // field panics); its next frame would panic.
        if matches!(status, wgpu::CurrentSurfaceTexture::Validation) {
            lose("the window surface reported a validation error");
        }
        default(status)
    });
}

/// Watch the window's new device: its loss, or an error wgpu would panic
/// on, marks the window for a rebuild instead.
pub(super) fn watch(device: &wgpu::Device) {
    device.set_device_lost_callback(|reason, message| {
        lose(&format!("the device was lost ({reason:?}) {message}"))
    });
    device.on_uncaptured_error(Arc::new(|error| lose(&error.to_string())));
    if let Ok(mut current) = DEVICE.lock() {
        *current = Some(device.clone());
    }
}

/// Whether the window's device is lost. A real loss reports itself inside
/// the wgpu call that found it, before egui-wgpu can panic on it.
pub(crate) fn device_lost() -> bool {
    LOST.load(Ordering::SeqCst)
}

fn lose(reason: &str) {
    if !LOST.swap(true, Ordering::SeqCst) {
        let in_use = IN_USE.lock().ok().and_then(|marker| marker.clone());
        warn!(
            reason,
            gpu = in_use.as_ref().map(|m| m.gpu.as_str()),
            backend = in_use.as_ref().map(|m| m.backend.as_str()),
            "The window's graphics device was lost; rebuilding the window"
        );
    }
}

/// Called at the start of each UI pass. Clears this launch's marker once the
/// window has kept drawing without a surface error. False when the device is
/// lost: the pass must draw nothing, since egui-wgpu panics on a lost device
/// with anything to upload, and the window closes to be rebuilt.
pub(super) fn frame_started(ctx: &egui::Context) -> bool {
    crate::startup::simulate_surface_error(&FAILED_ACQUIRES);
    let simulated = PRESENT
        .lock()
        .ok()
        .and_then(|tracker| crate::startup::simulate_device_loss(tracker.drawing_for()));
    if simulated == Some(false) {
        destroy_device(false);
    }
    if device_lost() {
        close_for_rebuild(ctx);
        return false;
    }
    if simulated == Some(true) {
        destroy_device(true);
    }
    let visible = ctx.input(|input| input.viewport().minimized != Some(true));
    let Ok(mut tracker) = PRESENT.lock() else {
        return true;
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
    true
}

/// The simulated loss. Between frames, the loss is delivered as wgpu does
/// after `destroy`. Inside a paint, it is marked the way a driver reset
/// reports itself inside the failing call, and egui-wgpu panics on it.
fn destroy_device(in_paint: bool) {
    let Some(device) = DEVICE.lock().ok().and_then(|device| device.clone()) else {
        return;
    };
    warn!(
        in_paint,
        "Simulating the loss of the window's graphics device"
    );
    device.destroy();
    if in_paint {
        lose("simulated inside a paint");
    } else {
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(2)),
        });
    }
}

/// Close the window once, keeping where it was.
fn close_for_rebuild(ctx: &egui::Context) {
    let Ok(mut placement) = PLACEMENT.lock() else {
        return;
    };
    if placement.is_some() {
        return;
    }
    *placement = Some(ctx.input(|input| {
        let viewport = input.viewport();
        Placement {
            outer: viewport.outer_rect,
            inner: viewport.inner_rect,
            maximized: viewport.maximized == Some(true),
        }
    }));
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

/// Open a rebuilt window where the lost one was.
pub(super) fn place(viewport: egui::ViewportBuilder) -> egui::ViewportBuilder {
    let Some(placement) = PLACEMENT.lock().ok().and_then(|mut p| p.take()) else {
        return viewport;
    };
    let mut viewport = viewport.with_maximized(placement.maximized);
    if let Some(outer) = placement.outer {
        viewport = viewport.with_position(outer.min);
    }
    if let Some(inner) = placement.inner {
        viewport = viewport.with_inner_size(inner.size());
    }
    viewport
}

/// After the window's event loop ends: whether its device was lost, so a new
/// window should open. Records the loss against the backend in use, so the
/// new window, and the next launches, try another. An error when the window
/// keeps losing its device.
pub(super) fn rebuild_after_loss() -> Result<bool, String> {
    if !LOST.swap(false, Ordering::SeqCst) {
        return Ok(false);
    }
    if let Ok(mut device) = DEVICE.lock() {
        device.take();
    }
    let marker = IN_USE.lock().ok().and_then(|mut marker| marker.take());
    if let (Some(directory), Some(marker)) = (STATE_DIR.get(), marker) {
        record_loss(directory, &marker, ProcessId::current());
    }
    if let Ok(mut started) = STARTED.lock() {
        started.take();
    }
    if let Ok(mut tracker) = PRESENT.lock() {
        *tracker = PresentTracker::new();
    }
    let mut rebuilds = REBUILDS.lock().map_err(|error| error.to_string())?;
    allow_rebuild(&mut rebuilds, Instant::now())
}

fn allow_rebuild(rebuilds: &mut Vec<Instant>, now: Instant) -> Result<bool, String> {
    rebuilds.retain(|at| now.duration_since(*at) < REBUILD_WINDOW);
    if rebuilds.len() >= MAX_REBUILDS {
        return Err(format!(
            "The window's graphics device was lost {} times in {} minutes",
            rebuilds.len() + 1,
            REBUILD_WINDOW.as_secs() / 60
        ));
    }
    rebuilds.push(now);
    Ok(true)
}

/// A device lost mid-run counts as a failed start of its backend. The
/// history may be busy with an instance starting; this waits for it briefly,
/// since no window is open meanwhile.
fn record_loss(directory: &Path, marker: &Marker, owner: ProcessId) {
    for _ in 0..20 {
        if update_history(directory, |history| record_failure(history, marker)).is_some() {
            let _ = fs::remove_file(marker_path(directory, owner));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    warn!("Could not record the lost graphics device in the renderer history");
}

/// The window closed normally. A launch that quits before its window is
/// confirmed did not fail, so its marker goes without counting either way.
pub(super) fn window_closed() {
    clear_marker(false);
    if let Ok(mut device) = DEVICE.lock() {
        device.take();
    }
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
    confirmed_at: Option<Instant>,
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
            confirmed_at: None,
        }
    }

    /// How long the window has worked.
    fn drawing_for(&self) -> Option<Duration> {
        self.confirmed_at.map(|at| at.elapsed())
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
                    self.confirmed_at = Some(now);
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
) -> Result<wgpu::Adapter, String> {
    let usable: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
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
    if let Some(directory) = STATE_DIR.get() {
        if let Ok(json) = serde_json::to_vec(&marker) {
            let _ = fs::write(marker_path(directory, ProcessId::current()), json);
        }
    }
    if let Ok(mut in_use) = IN_USE.lock() {
        *in_use = Some(marker.clone());
    }
    if let Ok(mut started) = STARTED.lock() {
        *started = Some(marker);
    }
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
#[cfg(windows)]
fn driver_version(vendor: u32, device: u32) -> Option<String> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    (0..)
        .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
        .find_map(|adapter| {
            let desc = unsafe { adapter.GetDesc1() }.ok()?;
            if desc.VendorId != vendor || desc.DeviceId != device {
                return None;
            }
            let version = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.ok()? as u64;
            Some(format!(
                "{}.{}.{}.{}",
                version >> 48,
                (version >> 32) & 0xffff,
                (version >> 16) & 0xffff,
                version & 0xffff
            ))
        })
}

#[cfg(not(windows))]
fn driver_version(_vendor: u32, _device: u32) -> Option<String> {
    None
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

    /// The adapters wgpu listed on the UHD 620 host from issue #59.
    fn uhd_620() -> Vec<Candidate> {
        vec![
            candidate(Backend::Vulkan, DeviceType::IntegratedGpu, false),
            candidate(Backend::Dx12, DeviceType::IntegratedGpu, true),
            candidate(Backend::Dx12, DeviceType::Cpu, true),
            candidate(Backend::Gl, DeviceType::IntegratedGpu, false),
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
        assert_eq!(
            pick(&avoiding(uhd_620(), &[Backend::Vulkan, Backend::Dx12])),
            Some(3)
        );
        // With everything avoided, the usual order applies again.
        let all = [Backend::Dx12, Backend::Gl, Backend::Vulkan];
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

    /// Serializes the tests that share this module's lost-device state.
    static DEVICE_TESTS: Mutex<()> = Mutex::new(());

    /// A device without a window, or `None` on a machine without an adapter.
    fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let device = runtime.block_on(async {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .ok()?;
            adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .ok()
        });
        if device.is_none() {
            eprintln!("No graphics adapter here; skipping");
        }
        device
    }

    /// One frame as eframe runs it: the UI pass, then egui-wgpu uploading
    /// what it drew. True when the pass asked the window to close.
    fn frame(
        ctx: &egui::Context,
        renderer: &mut eframe::egui_wgpu::Renderer,
        (device, queue): &(wgpu::Device, wgpu::Queue),
        guarded: bool,
    ) -> bool {
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            if guarded && !frame_started(ui.ctx()) {
                return;
            }
            ui.label("EternalMonitor is ready for your iPad");
        });
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut textures = output.textures_delta;
        for (id, deltas) in &textures.set {
            for delta in deltas {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        textures.clear();
        renderer.update_buffers(
            device,
            queue,
            &mut encoder,
            &primitives,
            &eframe::egui_wgpu::ScreenDescriptor {
                size_in_pixels: [1000, 640],
                pixels_per_point: 1.0,
            },
        );
        queue.submit([encoder.finish()]);
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::Close)
    }

    /// Draws one frame, loses the device as a driver reset does, and draws
    /// the next.
    fn lose_device_between_frames(guarded: bool) -> Option<std::thread::Result<bool>> {
        let gpu = headless_device()?;
        watch(&gpu.0);
        let ctx = egui::Context::default();
        let mut renderer = eframe::egui_wgpu::Renderer::new(
            &gpu.0,
            wgpu::TextureFormat::Rgba8Unorm,
            Default::default(),
        );
        assert!(!frame(&ctx, &mut renderer, &gpu, guarded));
        gpu.0.destroy();
        let _ = gpu.0.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(5)),
        });
        assert!(device_lost(), "the lost callback reports the loss");
        Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || frame(&ctx, &mut renderer, &gpu, guarded),
        )))
    }

    /// The field crash (problem 9): after the device is lost, egui-wgpu
    /// panics on the next frame that has anything to draw.
    #[test]
    fn drawing_on_a_lost_device_panics_in_egui_wgpu() {
        let _serial = DEVICE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(next) = lose_device_between_frames(false) else {
            return;
        };
        let panic = next.expect_err("egui-wgpu draws on the lost device");
        let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(
            message.contains("Failed to create staging buffer"),
            "{message}"
        );
        assert_eq!(rebuild_after_loss(), Ok(true));
        let _ = place(egui::ViewportBuilder::default());
        REBUILDS.lock().unwrap().clear();
    }

    #[test]
    fn a_lost_device_closes_the_window_for_a_rebuild_without_drawing() {
        let _serial = DEVICE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(next) = lose_device_between_frames(true) else {
            return;
        };
        assert!(next.expect("no panic on the lost device"), "asks to close");
        assert!(PLACEMENT.lock().unwrap().is_some(), "keeps where it was");
        assert_eq!(rebuild_after_loss(), Ok(true));
        assert!(!device_lost(), "the new window starts clean");
        assert!(!PRESENT.lock().unwrap().confirmed, "and must prove itself");
        let _ = place(egui::ViewportBuilder::default());
        // A normal close afterwards opens nothing.
        assert_eq!(rebuild_after_loss(), Ok(false));
        REBUILDS.lock().unwrap().clear();
    }

    #[test]
    fn a_device_lost_mid_run_is_avoided_next_time() {
        let directory = temp_dir("renderer-lost");
        let launch = ProcessId::current();
        let vulkan = marker("10de:28e0 32.0.15.6094", "vulkan");
        fs::write(
            marker_path(&directory, launch),
            serde_json::to_vec(&vulkan).unwrap(),
        )
        .unwrap();
        // The launch had drawn, so the marker was already cleared once.
        record_loss(&directory, &vulkan, launch);
        let history = read_history(&directory);
        assert!(avoided(&history, &vulkan.gpu, "vulkan"));
        assert!(!avoided(&history, &vulkan.gpu, "dx12"));
        assert!(!marker_path(&directory, launch).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_window_that_keeps_losing_its_device_is_given_up() {
        let start = Instant::now();
        let mut rebuilds = Vec::new();
        for minute in 0..MAX_REBUILDS as u64 {
            let at = start + Duration::from_secs(60 * minute);
            assert_eq!(allow_rebuild(&mut rebuilds, at), Ok(true));
        }
        let at = start + Duration::from_secs(60 * MAX_REBUILDS as u64);
        assert!(allow_rebuild(&mut rebuilds, at).is_err());
        // Losses far apart are each recovered.
        assert_eq!(allow_rebuild(&mut rebuilds, at + REBUILD_WINDOW), Ok(true));
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
