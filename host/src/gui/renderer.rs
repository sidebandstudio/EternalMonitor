//! Chooses the graphics adapter for the window. wgpu takes the first adapter
//! of the preferred GPU type, which is Vulkan on Windows. Old Intel drivers
//! (UHD 620 with 23.20.16.x, issue #59) offer a Vulkan adapter that lacks
//! WebGPU features, and the process dies inside the driver while creating the
//! device, with no error to catch. Prefer a fully capable adapter of the same
//! GPU type, usually DX12 there.
//!
//! Intel's Vulkan driver also crashes while the window is created on some UHD
//! PCs whose Vulkan adapter looks fully capable, so an Intel GPU draws with
//! DX12 first. The GPU itself is chosen as before; other vendors keep wgpu's
//! backend order.
//!
//! Each launch also leaves a marker naming its GPU, driver and backend until
//! the window is confirmed working. A marker whose process has exited means
//! that launch never got there, so that backend is avoided on that GPU. The
//! failure reporter notes beside the marker what ended the launch. A crash
//! inside a graphics driver or its loader, on the thread that started the
//! window's renderer, keeps the backend aside until the driver changes.
//! Capture and encoding run in the same drivers on other threads meanwhile,
//! so a crash there says nothing about the window. Any other end, or one on a
//! GPU whose driver version is unknown, is tried again after some clean
//! launches: the cause may be elsewhere, and a working backend must not stay
//! aside for it.
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
//!
//! The update manifest's `renderer_rules` (`docs/remote-config.md`) can name
//! DX12 or Vulkan for the GPU [`pick`] chose, by vendor and driver, from the
//! next launch. WGPU_BACKEND and any Window drawing choice but Automatic win
//! over a rule, also a choice that failed, and a rule never draws with a
//! backend that failed on that GPU and driver before, even one due for a
//! retry. A rule draws only when this launch's marker is saved and the
//! history can be, so a crash under it is remembered: it costs at most one
//! crash per driver.

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

use crate::remote_config::{RendererBackend, RendererRule};
use crate::startup::ProcessId;
use crate::troubleshooting::WindowDrawing;

/// Clean launches, per recorded failure, before an avoided backend is retried,
/// unless it crashed inside a known driver.
const RETRY_AFTER: u32 = 5;
/// What ended a launch that crashed in no module the reporter could name, or
/// that no reporter described.
pub(crate) const UNKNOWN: &str = "unknown";
/// What ended a launch that was ended without a crash, as by Task Manager.
#[cfg(any(windows, test))]
pub(crate) const TERMINATED: &str = "terminated";
/// Modules whose crash is the graphics driver's or its loader's, by the start
/// of the lowercase file name. Kept narrow: a crash anywhere else is retried
/// after clean launches.
const DRIVER_MODULES: [&str; 18] = [
    // Intel: the Vulkan driver (igvk64, the 0.3.3 crash), the Direct3D drivers
    // and their memory manager (igd10iumd64, igd12umd64, igdgmm64), the shader
    // compiler (igc64) and the Xe OpenGL driver (igxelpicd64).
    "igvk",
    "igd",
    "igc",
    "igxelpicd",
    // AMD: the Vulkan driver (amdvlk64), the OpenGL driver (atio6axx), the
    // Direct3D drivers (atidxx64, atiumd64, and amdxx64 for D3D12 and amdxn64
    // for D3D11 in newer drivers) and the DX12 shader compiler (amdxc64).
    "amdvlk",
    "atio6axx",
    "atidxx",
    "atiumd",
    "amdxx",
    "amdxn",
    "amdxc",
    // NVIDIA: the OpenGL and Vulkan driver (nvoglv64), the Direct3D drivers
    // (nvwgf2umx, nvd3dumx), their loader (nvldumdx) and the shader compiler
    // (nvgpucomp64).
    "nvoglv",
    "nvwgf2um",
    "nvd3dum",
    "nvldumd",
    "nvgpucomp",
    // The Vulkan and OpenGL loaders, which fail on the driver they load.
    "vulkan-1.dll",
    "opengl32.dll",
];
/// PCI vendor id of Intel GPUs.
const INTEL: u32 = 0x8086;
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
    /// The user reset crash avoidance; it applies at the next start.
    pub reset_pending: bool,
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
        status.reset_pending = true;
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
    vendor: u32,
    device: u32,
    device_type: DeviceType,
    compliant: bool,
    avoided: bool,
    /// A failure of this backend on this GPU and driver is recorded, also one
    /// due for a retry. A remote rule never draws with it.
    failed_before: bool,
}

/// What one launch is trying to start with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Marker {
    gpu: String,
    backend: String,
    /// The thread that lists adapters, creates the device and draws. 0.3.3
    /// saved none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread: Option<u32>,
}

/// A backend that failed to draw on one GPU and driver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Failure {
    gpu: String,
    backend: String,
    failures: u32,
    clean_launches: u32,
    /// What ended the launch: the module it crashed in, [`UNKNOWN`] or
    /// [`TERMINATED`]. A driver crash off the window's thread names its
    /// thread first (see [`attribute`]). 0.3.3 saved none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fault: Option<String>,
}

impl Failure {
    /// Whether the launch crashed inside a graphics driver. 0.3.3 noted no
    /// module, and its crash records were removed once reported, so its
    /// entries cannot tell; they are retried after clean launches, as 0.3.3
    /// itself did.
    fn in_driver(&self) -> bool {
        self.fault.as_deref().is_some_and(driver_module)
    }
}

fn driver_module(module: &str) -> bool {
    let module = module.to_ascii_lowercase();
    DRIVER_MODULES.iter().any(|name| module.starts_with(name))
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
    // Saved by the last update check; this launch's choice only.
    let rules = if cfg!(windows) {
        crate::remote_config::current().renderer_rules().to_vec()
    } else {
        Vec::new()
    };
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        let (plan, overruled) = choose_backends(&mut setup.instance_descriptor, &history, choice);
        setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
            select(adapters, surface, &history, plan, &rules, overruled)
        }));
    }
    let default = config.on_surface_status.clone();
    config.on_surface_status = Arc::new(move |status| {
        FAILED_ACQUIRES.fetch_add(1, Ordering::SeqCst);
        default(status)
    });
}

/// `WGPU_BACKEND` decides when set, as in wgpu, and nothing is narrowed or
/// recorded then. Otherwise the saved choice, unless it failed before. Also
/// returns why a remote rule can't apply, if one of them decides.
fn choose_backends(
    descriptor: &mut wgpu::InstanceDescriptor,
    history: &[Failure],
    choice: WindowDrawing,
) -> (Plan, Option<&'static str>) {
    let forced = Backends::from_env();
    let mut plan = Plan::Full;
    let mut chosen_failed = false;
    let gpus = if cfg!(windows) && forced.is_none() {
        system_gpus()
    } else {
        String::new()
    };
    if cfg!(windows) {
        if forced.is_none() {
            (plan, chosen_failed) = chosen_plan(history, &gpus, choice);
            let _ = start(Marker {
                gpu: gpus.clone(),
                backend: plan.stage().to_string(),
                thread: current_thread(),
            });
        }
        descriptor.backends = instance_backends(forced, plan);
    }
    update_status(|status| {
        status.choice = choice;
        status.from_env = forced.is_some();
        status.chosen_failed = chosen_failed;
        status.avoiding = history
            .iter()
            .any(|f| applies_now(f, &gpus) && avoided(history, &f.gpu, &f.backend));
    });
    info!(
        backends = ?descriptor.backends,
        from_env = forced.is_some(),
        choice = ?choice,
        chosen_failed,
        narrowed = plan != Plan::Full && Plan::chosen(choice) != Some(plan),
        "Window renderer instance"
    );
    (plan, overruled(forced.is_some(), choice))
}

/// Why a remote rule can't choose the backend: `WGPU_BACKEND` is set, or the
/// user chose how the window draws. A choice that failed before is set aside
/// and the automatic choice draws, but without the rule: the user chose not
/// to leave it to the PC.
fn overruled(from_env: bool, choice: WindowDrawing) -> Option<&'static str> {
    if from_env {
        Some("env")
    } else if choice != WindowDrawing::Automatic {
        Some("choice")
    } else {
        None
    }
}

/// The plan for a saved choice, or the automatic one when the choice is
/// Automatic or its backend failed to start the window before (`true`) on
/// these GPUs and drivers.
fn chosen_plan(history: &[Failure], gpus: &str, choice: WindowDrawing) -> (Plan, bool) {
    match Plan::chosen(choice) {
        Some(plan)
            if avoided(history, gpus, plan.stage())
                || history.iter().any(|f| {
                    plan.drew_with(f)
                        && applies_now(f, gpus)
                        && avoided(history, &f.gpu, &f.backend)
                }) =>
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

/// The id of this thread, as a crash record names the thread that crashed.
/// egui-wgpu lists adapters, creates the device and draws on the thread that
/// runs the window. Only Windows has a reporter to compare it with.
#[cfg(windows)]
fn current_thread() -> Option<u32> {
    Some(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() })
}

#[cfg(not(windows))]
fn current_thread() -> Option<u32> {
    None
}

/// Leave this launch's marker until the window is confirmed working. False
/// when it could not be saved.
fn start(marker: Marker) -> bool {
    let saved = STATE_DIR
        .get()
        .is_some_and(|directory| save_marker(directory, ProcessId::current(), &marker));
    if let Ok(mut started) = STARTED.lock() {
        *started = Some(marker);
    }
    saved
}

fn save_marker(directory: &Path, owner: ProcessId, marker: &Marker) -> bool {
    serde_json::to_vec(marker)
        .is_ok_and(|json| fs::write(marker_path(directory, owner), json).is_ok())
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
    rules: &[RendererRule],
    overruled: Option<&'static str>,
) -> Result<wgpu::Adapter, String> {
    let usable: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|adapter| surface.is_none_or(|surface| adapter.is_surface_supported(surface)))
        .filter(|adapter| plan != Plan::Warp || adapter.get_info().device_type == DeviceType::Cpu)
        .collect();
    let (candidates, gpus) = describe(&usable, history);
    let picked = pick(&candidates)
        .ok_or_else(|| "No graphics adapter can draw the EternalMonitor window".to_string())?;
    let marker = |index: usize| Marker {
        gpu: gpus[index].clone(),
        backend: candidates[index].backend.to_str().to_string(),
        thread: current_thread(),
    };
    let (index, ruled) = guard_rule(
        apply_rule(&candidates, &gpus, picked, rules, overruled),
        picked,
        STATE_DIR.get().map(PathBuf::as_path),
        |index| start(marker(index)),
    );
    if let Some((rule, outcome)) = ruled {
        info!("{}", rule_line(rule, outcome, &gpus[picked]));
    }
    let adapter = usable[index].clone();
    let info = adapter.get_info();
    info!(
        backend = %info.backend,
        adapter = %info.name,
        gpu = %gpus[index],
        compliant = candidates[index].compliant,
        reason = if index == picked {
            selection_reason(&candidates, index)
        } else {
            "remote rule"
        },
        avoided = ?history.iter().filter(|f| gpus.contains(&f.gpu) && avoided(history, &f.gpu, &f.backend)).map(|f| (&f.gpu, &f.backend, &f.fault)).collect::<Vec<_>>(),
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
    crate::startup::simulate_renderer_failure()?;
    Ok(adapter)
}

/// Each adapter as [`pick`] sees it, with its GPU key.
fn describe(adapters: &[&wgpu::Adapter], history: &[Failure]) -> (Vec<Candidate>, Vec<String>) {
    let gpus: Vec<String> = adapters.iter().map(|a| gpu_key(&a.get_info())).collect();
    let candidates = adapters
        .iter()
        .zip(&gpus)
        .map(|(adapter, gpu)| {
            let info = adapter.get_info();
            Candidate {
                backend: info.backend,
                vendor: info.vendor,
                device: info.device,
                device_type: info.device_type,
                compliant: adapter.get_downlevel_capabilities().is_webgpu_compliant(),
                avoided: avoided(history, gpu, info.backend.to_str()),
                failed_before: failed_before(history, gpu, info.backend.to_str()),
            }
        })
        .collect();
    (candidates, gpus)
}

/// The best candidate: not avoided, then wgpu's high-performance GPU type order,
/// then fully capable, then wgpu's own backend order. That chooses the GPU; an
/// Intel GPU then draws with DX12 first, unless a second card of its model
/// makes the same GPU impossible to find on DX12.
fn pick(candidates: &[Candidate]) -> Option<usize> {
    pick_with(candidates, true)
}

fn pick_with(candidates: &[Candidate], intel_dx12: bool) -> Option<usize> {
    let type_rank = |device_type| match device_type {
        DeviceType::DiscreteGpu => 0,
        DeviceType::IntegratedGpu => 1,
        DeviceType::Other => 2,
        DeviceType::VirtualGpu => 3,
        DeviceType::Cpu => 4,
    };
    let backend_rank = |c: &Candidate, dx12_first: bool| match c.backend {
        Backend::Dx12 if dx12_first => 0,
        Backend::Vulkan | Backend::Metal => 1,
        Backend::Dx12 => 2,
        Backend::Gl => 3,
        _ => 4,
    };
    let best = |gpu: Option<&Candidate>| {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| gpu.is_none_or(|g| (c.vendor, c.device) == (g.vendor, g.device)))
            .min_by_key(|(_, c)| {
                (
                    c.avoided,
                    type_rank(c.device_type),
                    !c.compliant,
                    backend_rank(c, gpu.is_some() && c.vendor == INTEL),
                )
            })
            .map(|(index, _)| index)
    };
    let index = best(None)?;
    if intel_dx12 && candidates[index].vendor == INTEL && !twins(candidates, &candidates[index]) {
        best(Some(&candidates[index]))
    } else {
        Some(index)
    }
}

/// Whether another card of `gpu`'s model is listed. Backends list identical
/// cards in their own order, so the DX12 adapter of the same model may be the
/// other card.
fn twins(candidates: &[Candidate], gpu: &Candidate) -> bool {
    let model = |c: &Candidate| (c.vendor, c.device) == (gpu.vendor, gpu.device);
    candidates.iter().filter(|c| model(c)).any(|c| {
        candidates
            .iter()
            .filter(|o| model(o) && o.backend == c.backend)
            .count()
            > 1
    })
}

/// A rule that named the window's GPU, and the candidate it draws with or why
/// it can't.
type Ruled<'a> = Option<(&'a RendererRule, Result<usize, &'static str>)>;

/// The first remote rule that names the GPU [`pick`] chose (`picked`), by
/// vendor and driver, and the candidate it draws with, or why it can't. A
/// rule only chooses among the listed candidates: the backend on that GPU.
/// It never takes one the crash memory holds a failure of, one that lacks
/// WebGPU features (which crashes, issue #59), or one of two identical
/// cards, which backends list in their own order.
fn apply_rule<'a>(
    candidates: &[Candidate],
    gpus: &[String],
    picked: usize,
    rules: &'a [RendererRule],
    overruled: Option<&'static str>,
) -> Ruled<'a> {
    let gpu = &candidates[picked];
    let driver = gpus[picked].split_once(' ').map_or("", |(_, d)| d.trim());
    let rule = rules.iter().find(|r| r.matches(gpu.vendor, driver))?;
    if let Some(reason) = overruled {
        return Some((rule, Err(reason)));
    }
    let model = |c: &Candidate| (c.vendor, c.device) == (gpu.vendor, gpu.device);
    let drawn_with = |c: &Candidate| match rule.backend {
        RendererBackend::Dx12 => {
            c.backend == Backend::Dx12 && c.device_type != DeviceType::Cpu && model(c)
        }
        RendererBackend::Vulkan => c.backend == Backend::Vulkan && model(c),
    };
    let target = if drawn_with(gpu) {
        Some(picked)
    } else if twins(candidates, gpu) {
        return Some((rule, Err("twin_gpus")));
    } else {
        candidates.iter().position(drawn_with)
    };
    let outcome = match target {
        None => Err("unavailable"),
        Some(i) if candidates[i].avoided || candidates[i].failed_before => Err("set_aside"),
        Some(i) if !candidates[i].compliant => Err("incapable"),
        Some(i) => Ok(i),
    };
    Some((rule, outcome))
}

/// A rule draws only when a crash under it will be remembered: this launch's
/// marker naming its candidate is saved (`start`), and the next launch can
/// save the history in `directory` it counts the crash in. Otherwise
/// `picked` draws and the rule is ignored (`no_marker`), so a PC that can't
/// remember crashes never crashes under a rule on every launch. Returns the
/// candidate drawn with, whose marker is started.
fn guard_rule<'a>(
    ruled: Ruled<'a>,
    picked: usize,
    directory: Option<&Path>,
    mut start: impl FnMut(usize) -> bool,
) -> (usize, Ruled<'a>) {
    match ruled {
        Some((rule, Ok(index))) => {
            if start(index) && directory.is_some_and(history_saves) {
                return (index, ruled);
            }
            if index != picked {
                start(picked);
            }
            (picked, Some((rule, Err("no_marker"))))
        }
        _ => {
            start(picked);
            (picked, ruled)
        }
    }
}

/// The launch's one line about a rule that named the window's GPU (`gpu`, a
/// GPU key): its vendor, the driver and its backend, nothing else.
fn rule_line(rule: &RendererRule, outcome: Result<usize, &str>, gpu: &str) -> String {
    let driver = match gpu.split_once(' ') {
        Some((_, driver)) if is_version(driver.trim()) => driver.trim(),
        _ => "unknown",
    };
    let named = format!("{}:{driver}->{}", rule.vendor(), rule.backend.name());
    match outcome {
        Ok(_) => format!("renderer_rule applied={named}"),
        Err(reason) => format!("renderer_rule ignored reason={reason} rule={named}"),
    }
}

/// Why [`pick`] chose the candidate at `index`: the rule without which it
/// would have chosen another, for the selection log line.
fn selection_reason(candidates: &[Candidate], index: usize) -> &'static str {
    let without: Vec<Candidate> = candidates
        .iter()
        .map(|c| Candidate {
            avoided: false,
            ..*c
        })
        .collect();
    if pick(&without) != Some(index) {
        return "avoiding crashed backend";
    }
    if pick_with(&without, false) != Some(index) {
        "intel default"
    } else {
        "default"
    }
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

/// A crash inside a graphics driver stays avoided until the driver changes,
/// which changes the GPU key. Every other failure, and one on a GPU whose
/// driver version is unknown, is retried after clean launches.
fn avoided(history: &[Failure], gpu: &str, backend: &str) -> bool {
    history.iter().any(|f| {
        f.gpu == gpu
            && f.backend == backend
            && ((f.in_driver() && knows_driver(&f.gpu))
                || f.clean_launches < RETRY_AFTER * f.failures)
    })
}

/// Whether `backend` failed on `gpu` (with its driver) before, whether or not
/// it is avoided now. Only drawing with it or Troubleshooting's reset forgets
/// that; a driver update is another GPU key.
fn failed_before(history: &[Failure], gpu: &str, backend: &str) -> bool {
    history.iter().any(|f| f.gpu == gpu && f.backend == backend)
}

/// Whether a failure was recorded on these GPUs with the drivers they have
/// now. A GPU listed with another driver was updated since, so its failure no
/// longer applies; one DXGI does not list cannot show its driver, so it does.
fn applies_now(failure: &Failure, gpus: &str) -> bool {
    let device = |gpu: &str| gpu.split(' ').next().unwrap_or_default().to_string();
    let now: Vec<&str> = gpus.split(", ").collect();
    failure
        .gpu
        .split(", ")
        .all(|gpu| now.contains(&gpu) || !now.iter().any(|n| device(n) == device(gpu)))
}

/// Whether every GPU in a key names its driver version, so a driver update
/// shows as a new key. A GPU DXGI does not list is keyed by wgpu's driver
/// text, which can be a vendor name that stays the same across updates.
fn knows_driver(gpu: &str) -> bool {
    gpu.split(", ").all(|gpu| {
        gpu.split_once(' ')
            .is_some_and(|(_, driver)| is_version(driver.trim()))
    })
}

/// Digits separated by dots, as `31.0.101.2111`.
fn is_version(text: &str) -> bool {
    let mut parts = text.split('.');
    parts.clone().count() >= 2
        && parts.all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Count a failure and what ended it. A driver crash stays noted when a later
/// failure of the same backend is something else.
fn record_failure(history: &mut Vec<Failure>, marker: &Marker, fault: &str) {
    match history
        .iter_mut()
        .find(|f| f.gpu == marker.gpu && f.backend == marker.backend)
    {
        Some(failure) => {
            failure.failures += 1;
            failure.clean_launches = 0;
            if !failure.in_driver() {
                failure.fault = Some(fault.to_string());
            }
        }
        None => history.push(Failure {
            gpu: marker.gpu.clone(),
            backend: marker.backend.clone(),
            failures: 1,
            clean_launches: 0,
            fault: Some(fault.to_string()),
        }),
    }
}

/// A backend that drew is cleared; every other failure counts the clean
/// launch, also when this launch drew on another GPU or on WARP. The count
/// retries every failure but a driver crash on a GPU with a known driver, and
/// keeps the file as earlier versions read it.
fn record_success(history: &mut Vec<Failure>, marker: &Marker) {
    history.retain(|f| !(f.gpu == marker.gpu && f.backend == marker.backend));
    for failure in history.iter_mut() {
        failure.clean_launches += 1;
    }
}

fn marker_path(directory: &Path, owner: ProcessId) -> PathBuf {
    directory.join(format!("{owner}.start"))
}

/// What ended the launch of the marker beside it, as the failure reporter saw.
fn fault_path(directory: &Path, owner: ProcessId) -> PathBuf {
    directory.join(format!("{owner}.fault"))
}

/// Called by the failure reporter once it knows how `owner` ended: the module
/// it crashed in and the crash record's thread, or [`UNKNOWN`] or
/// [`TERMINATED`] without a thread. Noted only while that launch's marker is
/// there, for the next launch's sweep.
#[cfg(windows)]
pub(crate) fn note_fault(owner: ProcessId, fault: &str, thread: Option<u32>) {
    if let Some(base) = crate::settings::app_data_dir() {
        write_fault(&base.join("renderer"), owner, fault, thread);
    }
}

#[cfg(any(windows, test))]
fn write_fault(directory: &Path, owner: ProcessId, fault: &str, thread: Option<u32>) {
    let Ok(bytes) = fs::read(marker_path(directory, owner)) else {
        return;
    };
    let window = serde_json::from_slice::<Marker>(&bytes)
        .ok()
        .and_then(|marker| marker.thread);
    let _ = fs::write(
        fault_path(directory, owner),
        attribute(fault, thread, window),
    );
}

/// The fault to note for a crash in `module` on `thread`. A driver crash is
/// the window's only on the `window` thread, which started its renderer.
/// Elsewhere, as in capture or encoding, or on a thread the record or marker
/// does not name, it is noted after its thread, which no driver name starts
/// with, so it is retried after clean launches.
#[cfg(any(windows, test))]
fn attribute(module: &str, thread: Option<u32>, window: Option<u32>) -> String {
    match thread {
        _ if !driver_module(module) => module.to_string(),
        Some(thread) if window == Some(thread) => module.to_string(),
        Some(thread) => format!("thread {thread}: {module}"),
        None => format!("unnamed thread: {module}"),
    }
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
    let _lock = lock_history(directory)?;
    let mut history = read_history(directory);
    let before = history.clone();
    change(&mut history);
    if history != before {
        write_history(directory, &history);
    }
    Some(history)
}

/// `None` without waiting when another instance holds the history.
fn lock_history(directory: &Path) -> Option<crate::startup::lock::Locked> {
    match crate::startup::lock::try_lock(&directory.join(HISTORY_LOCK)) {
        Ok(lock) => Some(lock),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::WouldBlock {
                warn!(%error, "Could not lock the renderer history");
            }
            None
        }
    }
}

/// False when it could not be saved.
fn write_history(directory: &Path, history: &[Failure]) -> bool {
    let path = directory.join(HISTORY_FILE);
    let temp = directory.join(format!("history.{}.tmp", ProcessId::current()));
    let saved = serde_json::to_vec(history)
        .map_err(std::io::Error::other)
        .and_then(|json| fs::write(&temp, json))
        .and_then(|()| fs::rename(&temp, &path));
    if let Err(error) = &saved {
        warn!(%error, "Could not save the renderer history");
    }
    saved.is_ok()
}

/// Whether the history saves now, as the next launch must save it to count a
/// crash of this one. Saves it unchanged; false while another instance holds
/// it.
fn history_saves(directory: &Path) -> bool {
    lock_history(directory).is_some_and(|_lock| write_history(directory, &read_history(directory)))
}

/// Count markers left by exited launches as failures, with what the failure
/// reporter saw end them, and remove them once the history is saved. Until
/// it saves, they stay, and a later launch counts them. A launch the reporter
/// did not describe, as 0.3.3's, counts as [`UNKNOWN`]. Markers of running
/// instances, including this one, are theirs to clear. When another instance
/// is updating the history, it sweeps them; use the history as saved.
fn sweep(directory: &Path, current: ProcessId) -> Vec<Failure> {
    let Some(_lock) = lock_history(directory) else {
        return read_history(directory);
    };
    let mut history = read_history(directory);
    let before = history.clone();
    let mut swept = Vec::new();
    for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let (owner, is_marker) = match (name.strip_suffix(".start"), name.strip_suffix(".fault")) {
            (Some(owner), _) => (owner, true),
            (_, Some(owner)) => (owner, false),
            _ => continue,
        };
        let Some(owner) = ProcessId::from_name(owner) else {
            continue;
        };
        if owner == current || owner.is_running() {
            continue;
        }
        // A note whose marker went with a working window.
        if !is_marker {
            if !marker_path(directory, owner).exists() {
                let _ = fs::remove_file(entry.path());
            }
            continue;
        }
        let fault = fs::read_to_string(fault_path(directory, owner))
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| UNKNOWN.to_string());
        if let Some(marker) = fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
        {
            record_failure(&mut history, &marker, &fault);
            let until_driver_changes = knows_driver(&marker.gpu)
                && history
                    .iter()
                    .any(|f| f.gpu == marker.gpu && f.backend == marker.backend && f.in_driver());
            warn!(
                gpu = %marker.gpu,
                backend = %marker.backend,
                fault = %fault,
                retry = if until_driver_changes { "after a driver update" } else { "after clean launches" },
                "A previous launch closed before its window appeared; avoiding that renderer"
            );
        }
        swept.push(owner);
    }
    if history != before && !write_history(directory, &history) {
        return history;
    }
    for owner in swept {
        let _ = fs::remove_file(marker_path(directory, owner));
        let _ = fs::remove_file(fault_path(directory, owner));
    }
    history
}

#[cfg(test)]
mod tests {
    use super::*;

    const NVIDIA: u32 = 0x10de;
    const AMD: u32 = 0x1002;
    const MICROSOFT: u32 = 0x1414;

    fn candidate(backend: Backend, device_type: DeviceType, compliant: bool) -> Candidate {
        on(NVIDIA, backend, device_type, compliant)
    }

    fn on(vendor: u32, backend: Backend, device_type: DeviceType, compliant: bool) -> Candidate {
        Candidate {
            backend,
            vendor,
            device: 0,
            device_type,
            compliant,
            avoided: false,
            failed_before: false,
        }
    }

    /// Intel's Vulkan driver, where 0.3.3 crashed in the field.
    const IGVK: &str = "igvk64.dll";

    /// The adapters wgpu listed on the UHD 620 host from issue #59, less the
    /// OpenGL one the instance no longer lists.
    fn uhd_620() -> Vec<Candidate> {
        vec![
            on(INTEL, Backend::Vulkan, DeviceType::IntegratedGpu, false),
            on(INTEL, Backend::Dx12, DeviceType::IntegratedGpu, true),
            on(MICROSOFT, Backend::Dx12, DeviceType::Cpu, true),
        ]
    }

    /// An Intel UHD PC whose Vulkan adapter is fully capable, as on the PCs
    /// where igvk64.dll crashed while the window was created.
    fn uhd_capable() -> Vec<Candidate> {
        vec![
            on(INTEL, Backend::Vulkan, DeviceType::IntegratedGpu, true),
            on(INTEL, Backend::Dx12, DeviceType::IntegratedGpu, true),
            on(MICROSOFT, Backend::Dx12, DeviceType::Cpu, true),
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
            thread: None,
        }
    }

    /// The thread a test launch draws its window on.
    const WINDOW: u32 = 0x1a2b;

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
        assert_eq!(selection_reason(&candidates, 0), "default");
    }

    #[test]
    fn intel_draws_with_dx12_even_with_capable_vulkan() {
        let candidates = uhd_capable();
        assert_eq!(pick(&candidates), Some(1));
        assert_eq!(selection_reason(&candidates, 1), "intel default");
        // Listed in the other order too.
        let reversed: Vec<Candidate> = candidates.iter().rev().copied().collect();
        assert_eq!(pick(&reversed), Some(1));
        // #59's driver chose DX12 for being incomplete before; it still does.
        assert_eq!(selection_reason(&uhd_620(), 1), "default");
    }

    #[test]
    fn nvidia_and_amd_keep_vulkan() {
        for vendor in [NVIDIA, AMD] {
            for device_type in [DeviceType::DiscreteGpu, DeviceType::IntegratedGpu] {
                let candidates = [
                    on(vendor, Backend::Dx12, device_type, true),
                    on(vendor, Backend::Vulkan, device_type, true),
                    on(MICROSOFT, Backend::Dx12, DeviceType::Cpu, true),
                ];
                assert_eq!(pick(&candidates), Some(1), "{vendor:04x} {device_type:?}");
                assert_eq!(selection_reason(&candidates, 1), "default");
            }
        }
        // A hybrid laptop still draws on its discrete GPU's Vulkan.
        let mut hybrid = uhd_capable();
        hybrid.push(on(NVIDIA, Backend::Dx12, DeviceType::DiscreteGpu, true));
        hybrid.push(on(NVIDIA, Backend::Vulkan, DeviceType::DiscreteGpu, true));
        assert_eq!(pick(&hybrid), Some(4));
        assert_eq!(selection_reason(&hybrid, 4), "default");
    }

    #[test]
    fn the_intel_default_never_changes_the_gpu() {
        // Two capable discrete GPUs: NVIDIA lists Vulkan, the Intel Arc only
        // DX12. wgpu's order chooses NVIDIA, on Vulkan, as before.
        let arc = |backend| Candidate {
            device: 0x56a0,
            ..on(INTEL, backend, DeviceType::DiscreteGpu, true)
        };
        let mixed = [
            arc(Backend::Dx12),
            on(NVIDIA, Backend::Dx12, DeviceType::DiscreteGpu, true),
            on(NVIDIA, Backend::Vulkan, DeviceType::DiscreteGpu, true),
            on(MICROSOFT, Backend::Dx12, DeviceType::Cpu, true),
        ];
        assert_eq!(pick(&mixed), Some(2));
        assert_eq!(selection_reason(&mixed, 2), "default");
        let reversed: Vec<Candidate> = mixed.iter().rev().copied().collect();
        assert_eq!(pick(&reversed), Some(1));
        // The Arc listing Vulkan too, after NVIDIA: still NVIDIA Vulkan.
        let mut both = vec![mixed[2], mixed[1], arc(Backend::Vulkan), arc(Backend::Dx12)];
        assert_eq!(pick(&both), Some(0));
        // Listed first, the Arc was wgpu's GPU before; it now draws with DX12.
        both.rotate_left(2);
        assert_eq!(pick(&both), Some(1));
        assert_eq!(selection_reason(&both, 1), "intel default");
        // An Intel iGPU beside an Arc: the Arc, with DX12.
        let pair = [
            on(INTEL, Backend::Vulkan, DeviceType::IntegratedGpu, true),
            on(INTEL, Backend::Dx12, DeviceType::IntegratedGpu, true),
            arc(Backend::Vulkan),
            arc(Backend::Dx12),
        ];
        assert_eq!(pick(&pair), Some(3));
        // Two Arc A770s, cards A and B. Each backend lists them in its own
        // order, and nothing in a candidate tells which DX12 adapter is the
        // chosen card, so the choice stays as before in every order.
        for vulkan_first in [true, false] {
            for dx12_order in [["A", "B"], ["B", "A"]] {
                let vulkan = ["A", "B"].map(|card| (arc(Backend::Vulkan), card));
                let dx12 = dx12_order.map(|card| (arc(Backend::Dx12), card));
                let listed: Vec<(Candidate, &str)> = if vulkan_first {
                    vulkan.into_iter().chain(dx12).collect()
                } else {
                    dx12.into_iter().chain(vulkan).collect()
                };
                let two: Vec<Candidate> = listed.iter().map(|(c, _)| *c).collect();
                let before = pick_with(&two, false).unwrap();
                let case = format!("vulkan first {vulkan_first}, dx12 {dx12_order:?}");
                assert_eq!(pick(&two), Some(before), "{case}");
                assert_eq!(listed[before], (arc(Backend::Vulkan), "A"), "{case}");
                assert_eq!(selection_reason(&two, before), "default", "{case}");
            }
        }
        // A second card that lists only DX12 still makes the model ambiguous.
        let lopsided = [arc(Backend::Vulkan), arc(Backend::Dx12), arc(Backend::Dx12)];
        assert_eq!(pick(&lopsided), Some(0));
        // An Arc beside a different Intel model keeps the DX12 preference.
        let other_model = [
            arc(Backend::Vulkan),
            arc(Backend::Dx12),
            Candidate {
                device: 0x56a1,
                ..arc(Backend::Vulkan)
            },
        ];
        assert_eq!(pick(&other_model), Some(1));
    }

    #[test]
    fn avoidance_outranks_the_intel_default() {
        let candidates = avoiding(uhd_capable(), &[Backend::Dx12]);
        assert_eq!(pick(&candidates), Some(0));
        assert_eq!(selection_reason(&candidates, 0), "avoiding crashed backend");
    }

    #[test]
    fn a_chosen_backend_or_wgpu_backend_wins_over_the_intel_default() {
        let gpus = "8086:9bc4 31.0.101.2111";
        // Vulkan chosen in Troubleshooting, or WGPU_BACKEND=vulkan: the
        // instance lists only Vulkan adapters, so Vulkan draws.
        assert_eq!(
            chosen_plan(&[], gpus, WindowDrawing::Vulkan),
            (Plan::Vulkan, false)
        );
        assert_eq!(instance_backends(None, Plan::Vulkan), Backends::VULKAN);
        let vulkan = Backends::from_comma_list("vulkan");
        assert_eq!(
            instance_backends(Some(vulkan), Plan::Full),
            Backends::VULKAN
        );
        let listed: Vec<Candidate> = uhd_capable()
            .into_iter()
            .filter(|c| c.backend == Backend::Vulkan)
            .collect();
        assert_eq!(pick(&listed), Some(0));
        assert_eq!(selection_reason(&listed, 0), "default");
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
    fn graphics_driver_modules_are_told_apart() {
        for module in [
            IGVK,
            "IGVK64.DLL",
            "igd10iumd64.dll",
            "igd12umd64.dll",
            "igxelpicd64.dll",
            "igc64.dll",
            "amdvlk64.dll",
            "atio6axx.dll",
            "atidxx64.dll",
            "amdxx64.dll",
            "amdxn64.dll",
            "amdxc64.dll",
            "nvoglv64.dll",
            "nvwgf2umx.dll",
            "nvldumdx.dll",
            "nvgpucomp64.dll",
            "vulkan-1.dll",
            "opengl32.dll",
        ] {
            assert!(driver_module(module), "{module}");
        }
        for fault in [
            UNKNOWN,
            TERMINATED,
            "EternalMonitor.exe",
            "ntdll.dll",
            "KERNELBASE.dll",
            "d3d12core.dll",
            "GameOverlayVulkanLayer64.dll",
            "graphics-hook64.dll",
            "nvspcap64.dll",
            "amdihk64.dll",
            "thread 9: igvk64.dll",
            "unnamed thread: igvk64.dll",
            "",
        ] {
            assert!(!driver_module(fault), "{fault}");
        }
    }

    #[test]
    fn only_a_driver_crash_on_the_window_thread_is_the_windows() {
        assert_eq!(attribute(IGVK, Some(WINDOW), Some(WINDOW)), IGVK);
        // Capture's D3D11 duplication or an encoder in the same driver.
        let capture = attribute("igd10iumd64.dll", Some(7), Some(WINDOW));
        assert_eq!(capture, "thread 7: igd10iumd64.dll");
        // A record without a thread.
        let unnamed = attribute(IGVK, None, Some(WINDOW));
        assert_eq!(unnamed, "unnamed thread: igvk64.dll");
        // A marker without one, as 0.3.3's.
        assert_eq!(
            attribute(IGVK, Some(WINDOW), None),
            format!("thread {WINDOW}: igvk64.dll")
        );
        for fault in [capture, unnamed] {
            assert!(!driver_module(&fault), "{fault}");
        }
        // Anything else is noted as it was.
        assert_eq!(
            attribute("EternalMonitor.exe", Some(7), Some(WINDOW)),
            "EternalMonitor.exe"
        );
        assert_eq!(attribute(UNKNOWN, None, Some(WINDOW)), UNKNOWN);
        assert_eq!(attribute(TERMINATED, None, Some(WINDOW)), TERMINATED);
    }

    #[test]
    fn a_driver_crash_is_retried_only_after_a_driver_update() {
        let old = marker("8086:5917 23.20.16.5038", "vulkan");
        let fallback = marker("8086:5917 23.20.16.5038", "dx12");
        let mut history = Vec::new();
        record_failure(&mut history, &old, IGVK);
        assert!(avoided(&history, &old.gpu, "vulkan"));
        assert!(!avoided(&history, &old.gpu, "dx12"));
        // Clean launches on the fallback never bring the crash back.
        for _ in 0..100 * RETRY_AFTER {
            record_success(&mut history, &fallback);
        }
        assert!(avoided(&history, &old.gpu, "vulkan"));
        // A later end of another kind does not undo it.
        record_failure(&mut history, &old, TERMINATED);
        for _ in 0..100 * RETRY_AFTER {
            record_success(&mut history, &fallback);
        }
        assert!(avoided(&history, &old.gpu, "vulkan"));
        // A newer driver for the same GPU starts clean.
        assert!(!avoided(&history, "8086:5917 31.0.101.2127", "vulkan"));
        // Drawing with it clears the entry.
        record_success(&mut history, &old);
        assert!(history.is_empty());
    }

    #[test]
    fn any_other_end_is_retried_after_clean_launches() {
        let old = marker("8086:5917 23.20.16.5038", "vulkan");
        let fallback = marker("8086:5917 23.20.16.5038", "dx12");
        // Our own module, an overlay's, no module, no report, Task Manager.
        for fault in [
            "EternalMonitor.exe",
            "GameOverlayVulkanLayer64.dll",
            UNKNOWN,
            TERMINATED,
        ] {
            let mut history = Vec::new();
            record_failure(&mut history, &old, fault);
            for _ in 0..RETRY_AFTER - 1 {
                record_success(&mut history, &fallback);
            }
            assert!(avoided(&history, &old.gpu, "vulkan"), "{fault}");
            record_success(&mut history, &fallback);
            assert!(!avoided(&history, &old.gpu, "vulkan"), "{fault}");
        }
    }

    #[test]
    fn without_a_driver_version_a_failure_is_retried_after_clean_launches() {
        // DXGI did not list the GPU and wgpu named no driver.
        let gpu = "8086:5917 ";
        assert!(!knows_driver(gpu));
        assert!(!knows_driver(""));
        assert!(!knows_driver("8086:5917 23.20.16.5038, 10de:1c8d "));
        assert!(knows_driver(
            "8086:5917 23.20.16.5038, 10de:1c8d 31.0.15.3623"
        ));
        // wgpu's driver text, when it is no version.
        assert!(!knows_driver("8086:5917 Intel Corporation Intel driver"));
        assert!(!knows_driver("1002:150e AMD proprietary driver 24.10.1"));
        assert!(!knows_driver("8086:5917 31"));
        assert!(!knows_driver("8086:5917 31..101"));
        // Even a driver crash: a driver update could not be seen.
        for gpu in [gpu, "8086:5917 Intel Corporation Intel driver"] {
            let old = marker(gpu, "vulkan");
            let fallback = marker(gpu, "dx12");
            let mut history = Vec::new();
            record_failure(&mut history, &old, IGVK);
            for _ in 0..RETRY_AFTER - 1 {
                record_success(&mut history, &fallback);
            }
            assert!(avoided(&history, gpu, "vulkan"), "{gpu}");
            record_success(&mut history, &fallback);
            assert!(!avoided(&history, gpu, "vulkan"), "{gpu}");
            // Failing again waits twice as long.
            record_failure(&mut history, &old, IGVK);
            for _ in 0..2 * RETRY_AFTER - 1 {
                record_success(&mut history, &fallback);
            }
            assert!(avoided(&history, gpu, "vulkan"), "{gpu}");
        }
    }

    #[test]
    fn files_from_earlier_versions_load_and_stay_readable() {
        let directory = temp_dir("renderer-old");
        // 0.3.3's history: Vulkan crashed once, then 40 clean launches on
        // DX12, so 0.3.3 would retry it now. It does not say where the
        // launch crashed, so it is retried as 0.3.3 would.
        let gpu = "8086:9bc4 31.0.101.2111";
        fs::write(
            directory.join(HISTORY_FILE),
            format!(r#"[{{"gpu":"{gpu}","backend":"vulkan","failures":1,"clean_launches":40}}]"#),
        )
        .unwrap();
        // And a marker an exited 0.3.3 launch left, which no reporter
        // described.
        let dead = exited_process();
        fs::write(
            marker_path(&directory, dead),
            format!(r#"{{"gpu":"{gpu}","backend":"dx12"}}"#),
        )
        .unwrap();
        let mut history = sweep(&directory, ProcessId::current());
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].fault, None);
        assert_eq!(history[1].fault.as_deref(), Some(UNKNOWN));
        assert!(!avoided(&history, gpu, "vulkan"));
        assert!(avoided(&history, gpu, "dx12"));
        for _ in 0..RETRY_AFTER {
            record_success(&mut history, &marker("1414:008c 10.0.22621.1", "dx12"));
        }
        assert!(!avoided(&history, gpu, "dx12"));
        // The saved file keeps the fields earlier versions need, and 0.3.3's
        // reading of it ignores the new one.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Failure033 {
            gpu: String,
            backend: String,
            failures: u32,
            clean_launches: u32,
        }
        let saved = fs::read(directory.join(HISTORY_FILE)).unwrap();
        assert!(String::from_utf8_lossy(&saved).contains(r#""fault":"unknown""#));
        assert_eq!(
            serde_json::from_slice::<Vec<Failure033>>(&saved)
                .unwrap()
                .len(),
            2
        );
        // Troubleshooting's reset forgets them all.
        update_history(&directory, Vec::clear);
        assert!(sweep(&directory, ProcessId::current()).is_empty());
        fs::remove_dir_all(directory).unwrap();
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
        // The reporter noted both; the running one's note stays with it.
        write_fault(&directory, running, "EternalMonitor.exe", Some(WINDOW));
        write_fault(&directory, dead, "EternalMonitor.exe", Some(WINDOW));

        let history = sweep(&directory, elsewhere);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].backend, "dx12");
        assert_eq!(history[0].fault.as_deref(), Some("EternalMonitor.exe"));
        assert!(marker_path(&directory, running).exists());
        assert!(fault_path(&directory, running).exists());
        assert!(!marker_path(&directory, dead).exists());
        assert!(!fault_path(&directory, dead).exists());
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
        record_failure(
            &mut history,
            &marker("10de:2d59 32.0.15.9186", "vulkan"),
            "nvoglv64.dll",
        );
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
        record_failure(&mut history, &marker(gpus, Plan::Dx12.stage()), UNKNOWN);
        record_failure(&mut history, &marker(gpus, Plan::Full.stage()), UNKNOWN);
        assert_eq!(
            chosen_plan(&history, gpus, WindowDrawing::Dx12),
            (Plan::Warp, true)
        );
        // WARP is told apart from hardware DX12 by Microsoft's adapter.
        let mut history = Vec::new();
        record_failure(
            &mut history,
            &marker("1414:008c 10.0.22621.1", "dx12"),
            UNKNOWN,
        );
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
    fn a_saved_choice_works_again_after_a_driver_update() {
        let gpus = "8086:9bc4 31.0.101.2111, 10de:2d59 32.0.15.9186";
        let mut history = Vec::new();
        record_failure(
            &mut history,
            &marker("8086:9bc4 31.0.101.2111", "vulkan"),
            IGVK,
        );
        assert_eq!(
            chosen_plan(&history, gpus, WindowDrawing::Vulkan),
            (Plan::Full, true)
        );
        // Intel's driver was updated: Vulkan draws as chosen.
        let updated = "8086:9bc4 32.0.101.6078, 10de:2d59 32.0.15.9186";
        assert_eq!(
            chosen_plan(&history, updated, WindowDrawing::Vulkan),
            (Plan::Vulkan, false)
        );
        let failure = &history[0];
        assert!(applies_now(failure, gpus));
        assert!(!applies_now(failure, updated));
        // A GPU DXGI does not list cannot show its driver: still avoided.
        assert!(applies_now(failure, ""));
        assert!(applies_now(failure, "10de:2d59 32.0.15.9186"));
        assert_eq!(
            chosen_plan(&history, "", WindowDrawing::Vulkan),
            (Plan::Full, true)
        );
    }

    #[test]
    fn a_driver_update_brings_the_full_instance_back() {
        let gpus = "1002:150e 32.0.21025.1024";
        let drew = marker("1002:150e 32.0.21025.1024", "dx12");
        let mut history = Vec::new();
        record_failure(
            &mut history,
            &marker(gpus, Plan::Full.stage()),
            "amdvlk64.dll",
        );
        for _ in 0..10 * RETRY_AFTER {
            record_success(&mut history, &drew);
        }
        assert_eq!(instance_plan(&history, gpus), Plan::Dx12);
        assert_eq!(
            instance_plan(&history, "1002:150e 32.0.21030.2001"),
            Plan::Full
        );
    }

    #[test]
    fn clean_launches_bring_the_full_instance_back_after_other_ends() {
        let gpus = "1002:150e 32.0.21025.1024";
        let drew = marker("1002:150e 32.0.21025.1024", "dx12");
        let mut history = Vec::new();
        record_failure(&mut history, &marker(gpus, Plan::Full.stage()), UNKNOWN);
        for _ in 0..RETRY_AFTER - 1 {
            record_success(&mut history, &drew);
        }
        assert_eq!(instance_plan(&history, gpus), Plan::Dx12);
        record_success(&mut history, &drew);
        assert_eq!(instance_plan(&history, gpus), Plan::Full);
    }

    #[test]
    fn the_reporter_note_decides_how_a_crashed_launch_is_retried() {
        let directory = temp_dir("renderer-fault");
        let gpu = "8086:9bc4 31.0.101.2111";
        let launch = |backend: &str, fault: Option<(&str, Option<u32>)>| {
            let dead = exited_process();
            let drawing = Marker {
                thread: Some(WINDOW),
                ..marker(gpu, backend)
            };
            fs::write(
                marker_path(&directory, dead),
                serde_json::to_vec(&drawing).unwrap(),
            )
            .unwrap();
            if let Some((fault, thread)) = fault {
                write_fault(&directory, dead, fault, thread);
            }
        };
        // Intel's Vulkan driver crashed on the window's thread; DX12 crashed
        // in our own code; GL was ended by Task Manager; Metal's launch has
        // no note at all.
        launch("vulkan", Some((IGVK, Some(WINDOW))));
        launch("dx12", Some(("EternalMonitor.exe", Some(WINDOW))));
        launch("gl", Some((TERMINATED, None)));
        launch("metal", None);
        // Capture crashed in Intel's D3D11 driver on its own thread, and a
        // record named no thread.
        launch("enumerate", Some(("igd10iumd64.dll", Some(WINDOW + 1))));
        launch("enumerate dx12", Some((IGVK, None)));
        // A note without a marker: that window worked; it goes.
        let shown = exited_process();
        fs::write(fault_path(&directory, shown), IGVK).unwrap();
        // And the reporter notes nothing for a launch without a marker.
        write_fault(&directory, exited_process(), IGVK, Some(WINDOW));
        let mut history = sweep(&directory, ProcessId::current());
        assert_eq!(history.len(), 6);
        assert!(!fault_path(&directory, shown).exists());
        let left = fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|x| x == "start" || x == "fault")
            })
            .count();
        assert_eq!(left, 0);
        let fault = |backend: &str| {
            history
                .iter()
                .find(|f| f.backend == backend)
                .and_then(|f| f.fault.clone())
        };
        assert_eq!(fault("vulkan").as_deref(), Some(IGVK));
        assert_eq!(fault("metal").as_deref(), Some(UNKNOWN));
        assert_eq!(fault("gl").as_deref(), Some(TERMINATED));
        assert_eq!(
            fault("enumerate").as_deref(),
            Some(format!("thread {}: igd10iumd64.dll", WINDOW + 1).as_str())
        );
        assert_eq!(
            fault("enumerate dx12").as_deref(),
            Some("unnamed thread: igvk64.dll")
        );
        for _ in 0..RETRY_AFTER - 1 {
            record_success(&mut history, &marker("1414:008c 10.0.22621.1", "dx12"));
        }
        for backend in ["dx12", "gl", "metal", "enumerate", "enumerate dx12"] {
            assert!(avoided(&history, gpu, backend), "{backend}");
        }
        record_success(&mut history, &marker("1414:008c 10.0.22621.1", "dx12"));
        // Only the window's driver crash stays aside, until the driver changes.
        for _ in 0..100 * RETRY_AFTER {
            record_success(&mut history, &marker("1414:008c 10.0.22621.1", "dx12"));
        }
        assert!(avoided(&history, gpu, "vulkan"));
        assert!(!avoided(&history, "8086:9bc4 32.0.101.6078", "vulkan"));
        for backend in ["dx12", "gl", "metal", "enumerate", "enumerate dx12"] {
            assert!(!avoided(&history, gpu, backend), "{backend}");
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_fallback_to_another_gpu_still_ages_the_failure() {
        let failed = marker("8086:5917 23.20.16.5038", "vulkan");
        let warp = marker("1414:008c 10.0.22621.1", "dx12");
        let mut history = Vec::new();
        record_failure(&mut history, &failed, UNKNOWN);
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
                            record_failure(history, &marker(gpu, "vulkan"), UNKNOWN)
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

    // ── Remote renderer rules ───────────────────────────────────────────────

    /// The rules a saved `windows.config` holds.
    fn rules(rules: serde_json::Value) -> Vec<RendererRule> {
        crate::remote_config::parse_config(Some(
            &serde_json::json!({ "schema": 1, "renderer_rules": rules }),
        ))
        .unwrap()
        .renderer_rules
    }

    const NV_DRIVER: &str = "32.0.15.9186";
    const AMD_DRIVER: &str = "32.0.21025.1024";

    /// mainpc as wgpu lists it: an RTX card beside a Radeon iGPU, with their
    /// GPU keys, and the crash memory applied as [`select`] applies it.
    fn mainpc(history: &[Failure]) -> (Vec<Candidate>, Vec<String>) {
        let nvidia = |backend| Candidate {
            device: 0x2d59,
            ..on(NVIDIA, backend, DeviceType::DiscreteGpu, true)
        };
        let amd = |backend| Candidate {
            device: 0x150e,
            ..on(AMD, backend, DeviceType::IntegratedGpu, true)
        };
        let listed = [
            (nvidia(Backend::Vulkan), format!("10de:2d59 {NV_DRIVER}")),
            (amd(Backend::Vulkan), format!("1002:150e {AMD_DRIVER}")),
            (nvidia(Backend::Dx12), format!("10de:2d59 {NV_DRIVER}")),
            (amd(Backend::Dx12), format!("1002:150e {AMD_DRIVER}")),
            (
                on(MICROSOFT, Backend::Dx12, DeviceType::Cpu, true),
                "1414:008c 10.0.26100.1".to_string(),
            ),
        ];
        remembered(listed.to_vec(), history)
    }

    fn remembered(
        listed: Vec<(Candidate, String)>,
        history: &[Failure],
    ) -> (Vec<Candidate>, Vec<String>) {
        listed
            .into_iter()
            .map(|(c, gpu)| {
                let backend = c.backend.to_str();
                let c = Candidate {
                    avoided: avoided(history, &gpu, backend),
                    failed_before: failed_before(history, &gpu, backend),
                    ..c
                };
                (c, gpu)
            })
            .unzip()
    }

    /// The candidate the window draws with, as [`select`] decides it.
    fn drawn(
        candidates: &[Candidate],
        gpus: &[String],
        rules: &[RendererRule],
        overruled: Option<&'static str>,
    ) -> (usize, Option<Result<usize, &'static str>>) {
        let picked = pick(candidates).unwrap();
        let ruled = apply_rule(candidates, gpus, picked, rules, overruled);
        let index = match ruled {
            Some((_, Ok(index))) => index,
            _ => picked,
        };
        (index, ruled.map(|(_, outcome)| outcome))
    }

    #[test]
    fn a_rule_applies_to_the_gpu_pick_chose_by_vendor_and_driver() {
        let (candidates, gpus) = mainpc(&[]);
        // Without a rule: the RTX card on Vulkan.
        assert_eq!(drawn(&candidates, &gpus, &[], None), (0, None));
        let dx12 = |driver: serde_json::Value| {
            let mut rule = serde_json::json!({ "vendor": "nvidia", "backend": "dx12" });
            if !driver.is_null() {
                rule["driver"] = driver;
            }
            rules(serde_json::json!([rule]))
        };
        let in_range = serde_json::json!({ "min": "32.0.15.9000", "max": "32.0.15.9199" });
        assert_eq!(
            drawn(&candidates, &gpus, &dx12(in_range), None),
            (2, Some(Ok(2)))
        );
        // Either end alone, or no range at all.
        for driver in [
            serde_json::json!({ "min": NV_DRIVER }),
            serde_json::json!({ "max": NV_DRIVER }),
            serde_json::Value::Null,
        ] {
            assert_eq!(
                drawn(&candidates, &gpus, &dx12(driver.clone()), None),
                (2, Some(Ok(2))),
                "{driver}"
            );
        }
        // Out of range on either side: no rule.
        for driver in [
            serde_json::json!({ "min": "32.0.15.9187" }),
            serde_json::json!({ "max": "32.0.15.9185" }),
            serde_json::json!({ "min": "31.0.15.0", "max": "31.0.15.9999" }),
        ] {
            assert_eq!(
                drawn(&candidates, &gpus, &dx12(driver.clone()), None),
                (0, None),
                "{driver}"
            );
        }
        // A rule for the iGPU the window doesn't use, or for Intel: nothing.
        for vendor in ["amd", "intel"] {
            let other = rules(serde_json::json!([{ "vendor": vendor, "backend": "dx12" }]));
            assert_eq!(
                drawn(&candidates, &gpus, &other, None),
                (0, None),
                "{vendor}"
            );
        }
        // The first rule naming the GPU decides.
        let two = rules(serde_json::json!([
            { "vendor": "amd", "backend": "dx12" },
            { "vendor": "nvidia", "backend": "vulkan" },
            { "vendor": "nvidia", "backend": "dx12" },
        ]));
        assert_eq!(drawn(&candidates, &gpus, &two, None), (0, Some(Ok(0))));
        // Naming the backend pick chose anyway applies and changes nothing.
        let vulkan = rules(serde_json::json!([{ "vendor": "nvidia", "backend": "vulkan" }]));
        assert_eq!(drawn(&candidates, &gpus, &vulkan, None), (0, Some(Ok(0))));
        // Unless it is in the crash memory, where pick() only fell back on it.
        let mut all_failed = candidates.clone();
        for c in &mut all_failed {
            c.avoided = true;
            c.failed_before = true;
        }
        assert_eq!(
            drawn(&all_failed, &gpus, &vulkan, None),
            (0, Some(Err("set_aside")))
        );
        // An unknown driver matches only a rule without a range.
        let mut unknown = gpus.clone();
        unknown[0] = "10de:2d59 NVIDIA 591.86".to_string();
        let ranged = dx12(serde_json::json!({ "min": "32.0.15.0" }));
        assert_eq!(drawn(&candidates, &unknown, &ranged, None), (0, None));
        // The log line names the vendor, the driver version and the backend.
        let any = dx12(serde_json::Value::Null);
        assert_eq!(
            rule_line(&any[0], Ok(2), &gpus[0]),
            format!("renderer_rule applied=nvidia:{NV_DRIVER}->dx12")
        );
        assert_eq!(
            rule_line(&any[0], Err("set_aside"), &unknown[0]),
            "renderer_rule ignored reason=set_aside rule=nvidia:unknown->dx12"
        );
    }

    #[test]
    fn a_rule_chooses_only_among_what_pick_would_consider() {
        let (candidates, gpus) = mainpc(&[]);
        let rule =
            |backend: &str| rules(serde_json::json!([{ "vendor": "nvidia", "backend": backend }]));
        // The instance was narrowed to DX12 after a launch died listing
        // adapters: Vulkan isn't listed, so a Vulkan rule can't apply.
        let dx12_only: Vec<Candidate> = candidates
            .iter()
            .filter(|c| c.backend == Backend::Dx12)
            .copied()
            .collect();
        let dx12_gpus: Vec<String> = [&gpus[2], &gpus[3], &gpus[4]].map(String::clone).to_vec();
        assert_eq!(
            drawn(&dx12_only, &dx12_gpus, &rule("dx12"), None),
            (0, Some(Ok(0)))
        );
        let mut nvidia_dx12_first = dx12_only.clone();
        nvidia_dx12_first.retain(|c| c.vendor == NVIDIA);
        assert_eq!(
            drawn(&nvidia_dx12_first, &dx12_gpus[..1], &rule("vulkan"), None),
            (0, Some(Err("unavailable")))
        );
        // #59's UHD 620: Vulkan lacks WebGPU features and would crash.
        let uhd: Vec<String> = ["8086:5917 23.20.16.5038"; 2]
            .into_iter()
            .map(String::from)
            .chain(["1414:008c 10.0.26100.1".to_string()])
            .collect();
        let intel_vulkan = rules(serde_json::json!([{ "vendor": "intel", "backend": "vulkan" }]));
        assert_eq!(
            drawn(&uhd_620(), &uhd, &intel_vulkan, None),
            (1, Some(Err("incapable")))
        );
        // A capable UHD's Vulkan is allowed.
        assert_eq!(
            drawn(&uhd_capable(), &uhd, &intel_vulkan, None),
            (0, Some(Ok(0)))
        );
        // Two cards of one model: which DX12 adapter is which isn't known.
        let arc = |backend| Candidate {
            device: 0x56a0,
            ..on(INTEL, backend, DeviceType::DiscreteGpu, true)
        };
        let twins = [
            arc(Backend::Vulkan),
            arc(Backend::Vulkan),
            arc(Backend::Dx12),
            arc(Backend::Dx12),
        ];
        let twin_gpus = vec!["8086:56a0 32.0.101.6078".to_string(); 4];
        let intel_dx12 = rules(serde_json::json!([{ "vendor": "intel", "backend": "dx12" }]));
        assert_eq!(
            drawn(&twins, &twin_gpus, &intel_dx12, None),
            (0, Some(Err("twin_gpus")))
        );
    }

    #[test]
    fn env_beats_the_choice_beats_crash_memory_beats_a_rule_beats_pick() {
        let gpus_now = format!("1002:150e {AMD_DRIVER}, 10de:2d59 {NV_DRIVER}");
        let rule = rules(serde_json::json!([{ "vendor": "nvidia", "backend": "dx12" }]));
        let nv_dx12 = marker(&format!("10de:2d59 {NV_DRIVER}"), "dx12");
        let nv_vulkan = marker(&format!("10de:2d59 {NV_DRIVER}"), "vulkan");

        // 5. pick(): the RTX card on Vulkan.
        let (candidates, gpus) = mainpc(&[]);
        assert_eq!(drawn(&candidates, &gpus, &[], None).0, 0);
        // 4. A rule beats it: DX12.
        let automatic = overruled(false, WindowDrawing::Automatic);
        assert_eq!(automatic, None);
        assert_eq!(
            drawn(&candidates, &gpus, &rule, automatic),
            (2, Some(Ok(2)))
        );

        // 3. The crash memory beats the rule: DX12 crashed on this driver.
        let mut history = Vec::new();
        record_failure(&mut history, &nv_dx12, "nvwgf2umx.dll");
        let (candidates, gpus) = mainpc(&history);
        assert_eq!(
            drawn(&candidates, &gpus, &rule, automatic),
            (0, Some(Err("set_aside")))
        );

        // 2. The Window drawing choice beats the rule, and the memory of
        // another backend: Vulkan chosen, so only Vulkan is listed and the
        // rule is overruled.
        let history = Vec::new();
        assert_eq!(
            chosen_plan(&history, &gpus_now, WindowDrawing::Vulkan),
            (Plan::Vulkan, false)
        );
        let chosen = overruled(false, WindowDrawing::Vulkan);
        assert_eq!(chosen, Some("choice"));
        let (all, gpus) = mainpc(&history);
        let vulkan: Vec<Candidate> = all
            .iter()
            .filter(|c| c.backend == Backend::Vulkan)
            .copied()
            .collect();
        assert_eq!(
            drawn(&vulkan, &gpus[..2], &rule, chosen),
            (0, Some(Err("choice")))
        );
        // DX12 chosen while the memory holds Vulkan: the choice draws.
        let mut history = Vec::new();
        record_failure(&mut history, &nv_vulkan, "nvoglv64.dll");
        assert_eq!(
            chosen_plan(&history, &gpus_now, WindowDrawing::Dx12),
            (Plan::Dx12, false)
        );
        assert_eq!(overruled(false, WindowDrawing::Dx12), Some("choice"));
        // A choice that itself crashed is set aside (#117) and the automatic
        // choice draws, still without the rule.
        assert_eq!(
            chosen_plan(&history, &gpus_now, WindowDrawing::Vulkan),
            (Plan::Full, true)
        );
        let (candidates, gpus) = mainpc(&history);
        assert_eq!(
            drawn(
                &candidates,
                &gpus,
                &rule,
                overruled(false, WindowDrawing::Vulkan)
            ),
            (2, Some(Err("choice")))
        );

        // 1. WGPU_BACKEND beats everything: the instance lists its backends
        // and neither the choice nor a rule applies.
        for choice in [
            WindowDrawing::Automatic,
            WindowDrawing::Dx12,
            WindowDrawing::Vulkan,
        ] {
            assert_eq!(overruled(true, choice), Some("env"));
        }
        let vulkan_env = Backends::from_comma_list("vulkan");
        assert_eq!(
            instance_backends(Some(vulkan_env), Plan::Dx12),
            Backends::VULKAN
        );
        let (candidates, gpus) = mainpc(&[]);
        assert_eq!(
            drawn(&candidates, &gpus, &rule, Some("env")),
            (0, Some(Err("env")))
        );
    }

    /// The user chose how the window draws, so no rule decides, even after
    /// that choice crashed and the automatic choice took over (#117).
    #[test]
    fn a_saved_choice_that_crashed_still_blocks_a_rule() {
        let gpus_now = format!("1002:150e {AMD_DRIVER}, 10de:2d59 {NV_DRIVER}");
        let rule = rules(serde_json::json!([{ "vendor": "nvidia", "backend": "dx12" }]));
        let mut history = Vec::new();
        record_failure(
            &mut history,
            &marker("1414:008c 10.0.26100.1", "dx12"),
            UNKNOWN,
        );
        assert_eq!(
            chosen_plan(&history, &gpus_now, WindowDrawing::Warp),
            (Plan::Full, true)
        );
        let (candidates, gpus) = mainpc(&history);
        assert_eq!(
            drawn(
                &candidates,
                &gpus,
                &rule,
                overruled(false, WindowDrawing::Warp)
            ),
            (0, Some(Err("choice")))
        );
        for choice in [
            WindowDrawing::Dx12,
            WindowDrawing::Vulkan,
            WindowDrawing::Warp,
        ] {
            assert_eq!(overruled(false, choice), Some("choice"), "{choice:?}");
        }
    }

    /// A rule draws only when this launch's marker is saved and the history
    /// can be, so a crash under it is counted at the next launch.
    #[test]
    fn a_rule_draws_only_when_its_crash_would_be_remembered() {
        let (candidates, gpus) = mainpc(&[]);
        let rule = rules(serde_json::json!([{ "vendor": "nvidia", "backend": "dx12" }]));
        let picked = pick(&candidates).unwrap();
        let owner = ProcessId::current();
        let marker_of = |index: usize| marker(&gpus[index], candidates[index].backend.to_str());
        let guarded = |directory: Option<&Path>| {
            let (index, ruled) = guard_rule(
                apply_rule(&candidates, &gpus, picked, &rule, None),
                picked,
                directory,
                |index| directory.is_some_and(|d| save_marker(d, owner, &marker_of(index))),
            );
            (index, ruled.map(|(_, outcome)| outcome))
        };
        let started = |directory: &Path| -> Option<Marker> {
            serde_json::from_slice(&fs::read(marker_path(directory, owner)).ok()?).ok()
        };

        // Both saved: the rule draws, and its marker names DX12.
        let directory = temp_dir("renderer-rule-guard");
        assert_eq!(guarded(Some(&directory)), (2, Some(Ok(2))));
        assert_eq!(started(&directory), Some(marker_of(2)));

        // No app data folder, or one where nothing can be saved: pick() draws.
        assert_eq!(guarded(None), (0, Some(Err("no_marker"))));
        let not_a_folder = directory.join("not-a-folder");
        fs::write(&not_a_folder, b"").unwrap();
        assert_eq!(guarded(Some(&not_a_folder)), (0, Some(Err("no_marker"))));

        // The marker saves but the history can't: pick() draws, and the
        // marker names its backend.
        let unsaved = temp_dir("renderer-rule-unsaved");
        let blocker = unsaved.join(format!("history.{}.tmp", ProcessId::current()));
        fs::create_dir(&blocker).unwrap();
        assert_eq!(guarded(Some(&unsaved)), (0, Some(Err("no_marker"))));
        assert_eq!(started(&unsaved), Some(marker_of(0)));
        fs::remove_dir(&blocker).unwrap();
        // Nor while another instance holds the history.
        let busy = crate::startup::lock::try_lock(&unsaved.join(HISTORY_LOCK)).unwrap();
        assert_eq!(guarded(Some(&unsaved)), (0, Some(Err("no_marker"))));
        drop(busy);
        assert_eq!(guarded(Some(&unsaved)), (2, Some(Ok(2))));

        // Without a rule, pick()'s marker is saved as before.
        let (index, ruled) = guard_rule(
            apply_rule(&candidates, &gpus, picked, &[], None),
            picked,
            Some(&directory),
            |index| save_marker(&directory, owner, &marker_of(index)),
        );
        assert_eq!((index, ruled), (0, None));
        assert_eq!(started(&directory), Some(marker_of(0)));
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(unsaved).unwrap();
    }

    /// A crash is never forgotten because the history couldn't be saved: the
    /// marker stays and the next launch that can save counts it, once.
    #[test]
    fn a_history_that_cannot_be_saved_keeps_the_marker() {
        let directory = temp_dir("renderer-sweep-unsaved");
        let gpu = format!("10de:2d59 {NV_DRIVER}");
        let dead = exited_process();
        let crashed = Marker {
            thread: Some(WINDOW),
            ..marker(&gpu, "dx12")
        };
        fs::write(
            marker_path(&directory, dead),
            serde_json::to_vec(&crashed).unwrap(),
        )
        .unwrap();
        write_fault(&directory, dead, "nvwgf2umx.dll", Some(WINDOW));
        let blocker = directory.join(format!("history.{}.tmp", ProcessId::current()));
        fs::create_dir(&blocker).unwrap();

        // This launch avoids the backend, but nothing is saved, so the
        // marker and its note stay.
        let unsaved = sweep(&directory, ProcessId::current());
        assert!(avoided(&unsaved, &gpu, "dx12"));
        assert!(read_history(&directory).is_empty());
        assert!(marker_path(&directory, dead).exists());
        assert!(fault_path(&directory, dead).exists());

        // The next launch saves it, counted once, and then removes them.
        fs::remove_dir(&blocker).unwrap();
        let saved = sweep(&directory, ProcessId::current());
        assert_eq!(saved, unsaved);
        assert_eq!(saved[0].failures, 1);
        assert_eq!(saved[0].fault.as_deref(), Some("nvwgf2umx.dll"));
        assert_eq!(read_history(&directory), saved);
        assert!(!marker_path(&directory, dead).exists());
        assert!(!fault_path(&directory, dead).exists());
        assert_eq!(sweep(&directory, ProcessId::current()), saved);
        fs::remove_dir_all(directory).unwrap();
    }

    /// The worst a bad rule can do: one launch that never shows its window,
    /// then the crash memory holds that backend and the rule is ignored, even
    /// when the crash was outside a driver and pick() would retry it.
    #[test]
    fn a_rule_that_crashes_the_window_costs_one_launch() {
        let directory = temp_dir("renderer-rule-crash");
        let rule = rules(serde_json::json!([{ "vendor": "nvidia", "backend": "dx12" }]));
        for fault in ["nvwgf2umx.dll", UNKNOWN, TERMINATED] {
            update_history(&directory, Vec::clear);
            // Launch 1: the rule draws with DX12, and the launch dies before
            // its window appears, leaving the marker select() wrote.
            let (candidates, gpus) = mainpc(&read_history(&directory));
            let (index, outcome) = drawn(&candidates, &gpus, &rule, None);
            assert_eq!((index, outcome), (2, Some(Ok(2))), "{fault}");
            let dead = exited_process();
            let drawing = Marker {
                thread: Some(WINDOW),
                ..marker(&gpus[index], candidates[index].backend.to_str())
            };
            fs::write(
                marker_path(&directory, dead),
                serde_json::to_vec(&drawing).unwrap(),
            )
            .unwrap();
            write_fault(&directory, dead, fault, Some(WINDOW));
            // Launch 2 sweeps it: the rule is ignored and pick() draws.
            let mut history = sweep(&directory, ProcessId::current());
            let (candidates, gpus) = mainpc(&history);
            assert_eq!(
                drawn(&candidates, &gpus, &rule, None),
                (0, Some(Err("set_aside"))),
                "{fault}"
            );
            // And it stays ignored however long the PC draws on Vulkan.
            let vulkan = marker(&gpus[0], "vulkan");
            for _ in 0..100 * RETRY_AFTER {
                record_success(&mut history, &vulkan);
            }
            let (candidates, gpus) = mainpc(&history);
            assert_eq!(
                drawn(&candidates, &gpus, &rule, None),
                (0, Some(Err("set_aside"))),
                "{fault}"
            );
            // A new driver is another GPU key: one more try there.
            let updated: Vec<String> = gpus
                .iter()
                .map(|g| g.replace(NV_DRIVER, "32.0.15.9200"))
                .collect();
            let (candidates, _) = remembered(
                candidates
                    .iter()
                    .copied()
                    .zip(updated.iter().cloned())
                    .collect(),
                &history,
            );
            assert_eq!(
                drawn(&candidates, &updated, &rule, None),
                (2, Some(Ok(2))),
                "{fault}"
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_malformed_or_missing_rule_list_is_todays_choice() {
        let (candidates, gpus) = mainpc(&[]);
        let today = drawn(&candidates, &gpus, &[], None);
        for config in [
            serde_json::json!({ "schema": 1 }),
            serde_json::json!({ "schema": 1, "renderer_rules": "nvidia dx12" }),
            serde_json::json!({ "schema": 1, "renderer_rules": { "vendor": "nvidia", "backend": "dx12" } }),
            serde_json::json!({ "schema": 1, "renderer_rules": [{ "vendor": "nvidia", "backend": "dx11" }] }),
            serde_json::json!({ "schema": 1, "renderer_rules": [{ "vendor": "nvidia", "backend": "dx12", "beta": true }] }),
            // WARP would move this PC off its RTX card for good.
            serde_json::json!({ "schema": 1, "renderer_rules": [{ "vendor": "nvidia", "backend": "warp" }] }),
            serde_json::json!({ "schema": 1, "renderer_rules": vec![serde_json::json!({ "vendor": "nvidia", "backend": "dx12" }); 9] }),
        ] {
            let parsed = crate::remote_config::parse_config(Some(&config)).unwrap();
            assert_eq!(
                drawn(&candidates, &gpus, &parsed.renderer_rules, None),
                today,
                "{config}"
            );
        }
        for bad in [
            None,
            Some(
                serde_json::json!({ "schema": 2, "renderer_rules": [{ "vendor": "nvidia", "backend": "dx12" }] }),
            ),
            Some(serde_json::json!("garbage")),
        ] {
            let applied = crate::remote_config::Applied {
                rollout: None,
                config: crate::remote_config::parse_config(bad.as_ref()),
            };
            assert!(applied.renderer_rules().is_empty(), "{bad:?}");
        }
    }

    /// How this PC's window would draw under the rules in `EM_RENDERER_RULES`
    /// (a `renderer_rules` list), from the adapters wgpu lists here with the
    /// automatic instance. Creates no device and opens no window:
    /// `EM_RENDERER_RULES='[{"vendor":"nvidia","backend":"dx12"}]' cargo test
    /// -p eternal-host --lib renderer_decision_on_this_pc -- --ignored --nocapture`
    #[test]
    #[ignore = "lists this PC's graphics adapters"]
    fn renderer_decision_on_this_pc() {
        let rules = rules(
            serde_json::from_str(&std::env::var("EM_RENDERER_RULES").unwrap_or("[]".into()))
                .unwrap(),
        );
        println!("rules: {rules:?}");
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: Plan::Full.backends(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapters = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(instance.enumerate_adapters(Plan::Full.backends()));
        let usable: Vec<&wgpu::Adapter> = adapters.iter().collect();
        let (candidates, gpus) = describe(&usable, &[]);
        for (index, (c, gpu)) in candidates.iter().zip(&gpus).enumerate() {
            println!(
                "{index}: {} {gpu} {:?} compliant={}",
                c.backend.to_str(),
                c.device_type,
                c.compliant
            );
        }
        if candidates.is_empty() {
            println!("no DX12 or Vulkan adapters here");
            return;
        }
        for (case, overruled) in [
            ("automatic", None),
            ("window drawing chosen", Some("choice")),
            ("WGPU_BACKEND set", Some("env")),
        ] {
            let (index, _) = drawn(&candidates, &gpus, &rules, overruled);
            let picked = pick(&candidates).unwrap();
            let line = apply_rule(&candidates, &gpus, picked, &rules, overruled)
                .map(|(rule, outcome)| rule_line(rule, outcome, &gpus[picked]));
            println!(
                "{case}: pick()={picked} ({} {}), draws {index} ({} {}), log: {}",
                candidates[picked].backend.to_str(),
                gpus[picked],
                candidates[index].backend.to_str(),
                gpus[index],
                line.as_deref().unwrap_or("none")
            );
        }
    }
}
