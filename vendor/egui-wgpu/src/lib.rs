//! This crates provides bindings between [`egui`](https://github.com/emilk/egui) and [wgpu](https://crates.io/crates/wgpu).
//!
//! If you're targeting WebGL you also need to turn on the
//! `webgl` feature of the `wgpu` crate:
//!
//! ```toml
//! # Enable both WebGL and WebGPU backends on web.
//! wgpu = { version = "*", features = ["webgpu", "webgl"] }
//! ```
//!
//! You can control whether WebGL or WebGPU will be picked at runtime by configuring
//! [`WgpuConfiguration::wgpu_setup`].
//! The default is to prefer WebGPU and fall back on WebGL.
//!
//! ## Feature flags
#![doc = document_features::document_features!()]
//!

pub use wgpu;

/// Low-level painting of [`egui`](https://github.com/emilk/egui) on [`wgpu`].
mod renderer;

mod setup;

pub use renderer::*;
pub use setup::{
    EguiDisplayHandle, NativeAdapterSelectorMethod, WgpuSetup, WgpuSetupCreateNew,
    WgpuSetupExisting,
};

/// Helpers for capturing screenshots of the UI.
#[cfg(feature = "capture")]
pub mod capture;

/// Module for painting [`egui`](https://github.com/emilk/egui) with [`wgpu`] on [`winit`].
#[cfg(feature = "winit")]
pub mod winit;

use std::sync::Arc;

use epaint::mutex::RwLock;

/// An error produced by egui-wgpu.
#[derive(thiserror::Error, Debug)]
pub enum WgpuError {
    #[error(transparent)]
    RequestAdapterError(#[from] wgpu::RequestAdapterError),

    #[error("Adapter selection failed: {0}")]
    CustomNativeAdapterSelectionError(String),

    #[error("There was no valid format for the surface at all.")]
    NoSurfaceFormatsAvailable,

    #[error(transparent)]
    RequestDeviceError(#[from] wgpu::RequestDeviceError),

    #[error(transparent)]
    CreateSurfaceError(#[from] wgpu::CreateSurfaceError),

    #[cfg(feature = "winit")]
    #[error(transparent)]
    HandleError(#[from] ::winit::raw_window_handle::HandleError),
}

/// Runtime-mutable subset of [`WgpuConfiguration`].
///
/// Edit any field to have the surface reconfigured on the next paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceConfig {
    /// Present mode used for the primary surface.
    pub present_mode: wgpu::PresentMode,

    /// Desired maximum number of frames that the presentation engine should queue in advance.
    ///
    /// Use `1` for low-latency, and `2` for high-throughput.
    ///
    /// See [`wgpu::SurfaceConfiguration::desired_maximum_frame_latency`] for details.
    ///
    /// `None` => Let `wgpu` pick a default (currently `2`).
    pub desired_maximum_frame_latency: Option<u32>,
}

impl SurfaceConfig {
    /// Good default for GUIs with very little (or no) extra GPU work.
    pub const LOW_LATENCY: Self = Self {
        present_mode: wgpu::PresentMode::AutoVsync,

        desired_maximum_frame_latency: if cfg!(target_os = "ios") {
            None // The default is good on iOS, while `Some(1)` cuts FPS in half
        } else {
            Some(1)
        },
    };

    /// Good default for GUIs with a lot of extra GPU work,
    /// or that want to prioritize smoothness over latency.
    pub const HIGH_THROUGHPUT: Self = Self {
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: Some(2), // High-throughput.
    };
}

/// Access to the render state for egui.
#[derive(Clone)]
pub struct RenderState {
    /// Wgpu adapter used for rendering.
    pub adapter: wgpu::Adapter,

    /// All the available adapters.
    ///
    /// This is not available on web.
    /// On web, we always select WebGPU is available, then fall back to WebGL if not.
    #[cfg(not(target_arch = "wasm32"))]
    pub available_adapters: Vec<wgpu::Adapter>,

    /// Wgpu instance used for creating surfaces and adapters.
    pub instance: wgpu::Instance,

    /// Wgpu device used for rendering, created from the adapter.
    pub device: wgpu::Device,

    /// Wgpu queue used for rendering, created from the adapter.
    pub queue: wgpu::Queue,

    /// The target texture format used for presenting to the window.
    pub target_format: wgpu::TextureFormat,

    /// Egui renderer responsible for drawing the UI.
    pub renderer: Arc<RwLock<Renderer>>,

    /// Runtime-mutable subset of the wgpu configuration.
    ///
    /// Update this to have the surface reconfigured on the next paint.
    pub surface_config: SurfaceConfig,
}

async fn request_adapter(
    instance: &wgpu::Instance,
    power_preference: wgpu::PowerPreference,
    compatible_surface: Option<&wgpu::Surface<'_>>,
    available_adapters: &[wgpu::Adapter],
) -> Result<wgpu::Adapter, WgpuError> {
    profiling::function_scope!();

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface,
            // We don't expose this as an option right now since it's fairly rarely useful:
            // * only has an effect on native
            // * fails if there's no software rasterizer available
            // * can achieve the same with `native_adapter_selector`
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        })
        .await
        .inspect_err(|_err| {
            if cfg!(target_arch = "wasm32") {
                // Nothing to add here
            } else if available_adapters.is_empty() {
                if std::env::var("DYLD_LIBRARY_PATH").is_ok() {
                    // DYLD_LIBRARY_PATH can sometimes lead to loading dylibs that cause
                    // us to find zero adapters. Very strange.
                    // I don't want to debug this again.
                    // See https://github.com/rerun-io/rerun/issues/11351 for more
                    log::warn!(
                        "No wgpu adapter found. This could be because DYLD_LIBRARY_PATH causes dylibs to be loaded that interfere with Metal device creation. Try restarting with DYLD_LIBRARY_PATH=''"
                    );
                } else {
                    log::info!("No wgpu adapter found");
                }
            } else if available_adapters.len() == 1 {
                log::info!(
                    "The only available wgpu adapter was not suitable: {}",
                    adapter_info_summary(&available_adapters[0].get_info())
                );
            } else {
                log::info!(
                    "No suitable wgpu adapter found out of the {} available ones: {}",
                    available_adapters.len(),
                    describe_adapters(available_adapters)
                );
            }
        })?;

    if 1 < available_adapters.len() {
        log::info!(
            "There are {} available wgpu adapters: {}",
            available_adapters.len(),
            describe_adapters(available_adapters)
        );
    }

    Ok(adapter)
}

impl RenderState {
    /// Creates a new [`RenderState`], containing everything needed for drawing egui with wgpu.
    ///
    /// # Errors
    /// Wgpu initialization may fail due to incompatible hardware or driver for a given config.
    pub async fn create(
        config: &WgpuConfiguration,
        instance: &wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
        options: RendererOptions,
    ) -> Result<Self, WgpuError> {
        profiling::scope!("RenderState::create"); // async yield give bad names using `profile_function`

        // This is always an empty list on web.
        #[cfg(not(target_arch = "wasm32"))]
        let available_adapters = {
            let backends = if let WgpuSetup::CreateNew(create_new) = &config.wgpu_setup {
                create_new.instance_descriptor.backends
            } else {
                wgpu::Backends::all()
            };

            instance.enumerate_adapters(backends).await
        };

        let (instance, adapter, device, queue) = match config.wgpu_setup.clone() {
            WgpuSetup::CreateNew(WgpuSetupCreateNew {
                instance_descriptor: _,
                display_handle: _,
                power_preference,
                native_adapter_selector: _native_adapter_selector,
                device_descriptor,
            }) => {
                let adapter = {
                    #[cfg(target_arch = "wasm32")]
                    {
                        request_adapter(instance, power_preference, compatible_surface, &[]).await
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(native_adapter_selector) = _native_adapter_selector {
                        native_adapter_selector(&available_adapters, compatible_surface)
                            .map_err(WgpuError::CustomNativeAdapterSelectionError)
                    } else {
                        request_adapter(
                            instance,
                            power_preference,
                            compatible_surface,
                            &available_adapters,
                        )
                        .await
                    }
                }?;

                let (device, queue) = {
                    profiling::scope!("request_device");
                    adapter
                        .request_device(&(*device_descriptor)(&adapter))
                        .await?
                };

                (instance.clone(), adapter, device, queue)
            }
            WgpuSetup::Existing(WgpuSetupExisting {
                instance,
                adapter,
                device,
                queue,
            }) => (instance, adapter, device, queue),
        };

        log_adapter_info(&adapter.get_info());

        let surface_formats = {
            profiling::scope!("get_capabilities");
            compatible_surface.map_or_else(
                || vec![wgpu::TextureFormat::Rgba8Unorm],
                |s| s.get_capabilities(&adapter).formats,
            )
        };
        let target_format = crate::preferred_framebuffer_format(&surface_formats)?;

        let renderer = Renderer::new(&device, target_format, options);

        // On wasm, depending on feature flags, wgpu objects may or may not implement sync.
        // It doesn't make sense to switch to Rc for that special usecase, so simply disable the lint.
        #[allow(clippy::allow_attributes, clippy::arc_with_non_send_sync)] // For wasm
        Ok(Self {
            instance,
            adapter,
            #[cfg(not(target_arch = "wasm32"))]
            available_adapters,
            device,
            queue,
            target_format,
            renderer: Arc::new(RwLock::new(renderer)),
            surface_config: config.surface,
        })
    }
}

/// EternalMonitor patch: which adapter one is, across listings. Its LUID where the
/// backend gives one (DX12 always, Vulkan 1.1+ on Windows), which tells identical cards
/// apart. Without one, the backend, the PCI ids, the type, the name and the PCI bus: wgpu's
/// DX12 bus comes from the PCI ids alone, so identical cards there would share it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterKey {
    pub luid: Option<u64>,
    pub backend: wgpu::Backend,
    pub vendor: u32,
    pub device: u32,
    pub device_type: wgpu::DeviceType,
    pub name: String,
    pub pci_bus: String,
}

impl AdapterKey {
    pub fn of(adapter: &wgpu::Adapter) -> Self {
        let info = adapter.get_info();
        Self {
            luid: luid(adapter),
            backend: info.backend,
            vendor: info.vendor,
            device: info.device,
            device_type: info.device_type,
            name: info.name,
            pci_bus: info.device_pci_bus_id,
        }
    }
}

/// EternalMonitor patch: the adapter's LUID, from DXGI on DX12, or from
/// `VkPhysicalDeviceIDProperties` on Vulkan 1.1+.
#[cfg(windows)]
fn luid(adapter: &wgpu::Adapter) -> Option<u64> {
    // SAFETY: the backend objects are only read, while `adapter` holds them.
    if let Some(hal) = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() } {
        let desc = unsafe { hal.raw_adapter().GetDesc1() }.ok()?;
        let luid = desc.AdapterLuid;
        return Some((u64::from(luid.HighPart as u32) << 32) | u64::from(luid.LowPart));
    }
    if let Some(hal) = unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() } {
        let shared = hal.shared_instance();
        if shared.instance_api_version() < ash::vk::API_VERSION_1_1 {
            return None;
        }
        let mut id = ash::vk::PhysicalDeviceIDProperties::default();
        {
            let mut properties = ash::vk::PhysicalDeviceProperties2::default().push_next(&mut id);
            unsafe {
                shared
                    .raw_instance()
                    .get_physical_device_properties2(hal.raw_physical_device(), &mut properties);
            }
        }
        return (id.device_luid_valid == ash::vk::TRUE).then(|| u64::from_le_bytes(id.device_luid));
    }
    None
}

#[cfg(not(windows))]
fn luid(_adapter: &wgpu::Adapter) -> Option<u64> {
    None
}

/// EternalMonitor patch: the adapters, by index into `keys`, that a replacement may choose
/// from: only `preferred` (the last one that drew on the window) while it is listed and
/// not `excluded`, else every one not `excluded` (ones that couldn't draw on the window).
pub fn replacement_candidates(
    keys: &[AdapterKey],
    preferred: Option<&AdapterKey>,
    excluded: &[AdapterKey],
) -> Vec<usize> {
    let allowed = |key: &AdapterKey| !excluded.contains(key);
    if let Some(preferred) = preferred.filter(|preferred| allowed(preferred))
        && let Some(index) = keys.iter().position(|key| key == preferred)
    {
        return vec![index];
    }
    (0..keys.len())
        .filter(|&index| allowed(&keys[index]))
        .collect()
}

/// EternalMonitor patch: an adapter and device to replace a lost one, made without the
/// window's surface: the thread that makes them, where a driver can stall, never touches
/// the window. The window's thread then checks the surface ([`RenderState::surface_format`]).
#[cfg(not(target_arch = "wasm32"))]
pub struct NewDevice {
    adapter: wgpu::Adapter,
    available_adapters: Vec<wgpu::Adapter>,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

#[cfg(not(target_arch = "wasm32"))]
impl NewDevice {
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn adapter_key(&self) -> AdapterKey {
        AdapterKey::of(&self.adapter)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl RenderState {
    /// EternalMonitor patch: list adapters, choose among [`replacement_candidates`] as
    /// [`Self::create`] does but with no surface to check against, and make the device.
    /// `watch` gets the device before anything is made on it. Takes no window or surface,
    /// so it can run on a thread that may outlive the window.
    pub async fn new_device(
        config: &WgpuConfiguration,
        instance: &wgpu::Instance,
        preferred: Option<&AdapterKey>,
        excluded: &[AdapterKey],
        watch: impl FnOnce(&wgpu::Device),
    ) -> Result<NewDevice, WgpuError> {
        let WgpuSetup::CreateNew(create_new) = &config.wgpu_setup else {
            return Err(WgpuError::CustomNativeAdapterSelectionError(
                "a device the app made can't be replaced".to_owned(),
            ));
        };
        let available_adapters = instance
            .enumerate_adapters(create_new.instance_descriptor.backends)
            .await;
        let keys: Vec<AdapterKey> = available_adapters.iter().map(AdapterKey::of).collect();
        let candidates: Vec<wgpu::Adapter> = replacement_candidates(&keys, preferred, excluded)
            .into_iter()
            .map(|index| available_adapters[index].clone())
            .collect();
        if candidates.is_empty() {
            return Err(WgpuError::CustomNativeAdapterSelectionError(
                "no adapter left that could draw on the window".to_owned(),
            ));
        }
        let adapter = if let Some(selector) = &create_new.native_adapter_selector {
            selector(&candidates, None).map_err(WgpuError::CustomNativeAdapterSelectionError)?
        } else {
            // wgpu chooses among all adapters, not these: keep its choice only if it is one.
            let keys: Vec<AdapterKey> = candidates.iter().map(AdapterKey::of).collect();
            request_adapter(instance, create_new.power_preference, None, &candidates)
                .await
                .ok()
                .filter(|adapter| keys.contains(&AdapterKey::of(adapter)))
                .unwrap_or_else(|| candidates[0].clone())
        };
        let (device, queue) = adapter
            .request_device(&(*create_new.device_descriptor)(&adapter))
            .await?;
        watch(&device);
        Ok(NewDevice {
            adapter,
            available_adapters,
            device,
            queue,
        })
    }

    /// EternalMonitor patch: on the window's thread, the format to draw on `surface` with
    /// `new`'s adapter, or an error when that adapter can't draw on it.
    pub fn surface_format(
        new: &NewDevice,
        surface: &wgpu::Surface<'_>,
    ) -> Result<wgpu::TextureFormat, WgpuError> {
        if !new.adapter.is_surface_supported(surface) {
            return Err(WgpuError::CustomNativeAdapterSelectionError(
                "the new adapter can't draw on the window".to_owned(),
            ));
        }
        preferred_framebuffer_format(&surface.get_capabilities(&new.adapter).formats)
    }

    /// EternalMonitor patch: the render state of a replacement, from its device and a
    /// renderer made for `target_format`.
    pub fn from_new_device(
        new: NewDevice,
        config: &WgpuConfiguration,
        instance: &wgpu::Instance,
        target_format: wgpu::TextureFormat,
        renderer: Renderer,
    ) -> Self {
        log_adapter_info(&new.adapter.get_info());
        Self {
            instance: instance.clone(),
            adapter: new.adapter,
            available_adapters: new.available_adapters,
            device: new.device,
            queue: new.queue,
            target_format,
            renderer: Arc::new(RwLock::new(renderer)),
            surface_config: config.surface,
        }
    }
}

fn describe_adapters(adapters: &[wgpu::Adapter]) -> String {
    if adapters.is_empty() {
        "(none)".to_owned()
    } else if adapters.len() == 1 {
        adapter_info_summary(&adapters[0].get_info())
    } else {
        adapters
            .iter()
            .map(|a| format!("{{{}}}", adapter_info_summary(&a.get_info())))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Specifies which action should be taken as consequence of a surface error.
pub enum SurfaceErrorAction {
    /// Do nothing and skip the current frame.
    SkipFrame,

    /// Reconfigure the existing surface, then skip the current frame.
    ///
    /// Calls [`wgpu::Surface::configure`] on the current surface object.
    /// Use for [`wgpu::CurrentSurfaceTexture::Outdated`].
    Reconfigure,

    /// Drop the surface, create a new one via [`wgpu::Instance::create_surface`], configure it,
    /// then skip the current frame.
    ///
    /// Use for [`wgpu::CurrentSurfaceTexture::Lost`], where reconfiguring the same surface
    /// object cannot recover.
    RecreateSurface,
}

/// Configuration for using wgpu with eframe or the egui-wgpu winit feature.
#[derive(Clone)]
pub struct WgpuConfiguration {
    /// Runtime-mutable configuration for the surface (present mode, frame latency).
    ///
    /// These are the fields exposed via [`RenderState::surface_config`] for live
    /// reconfiguration at runtime.
    pub surface: SurfaceConfig,

    /// How to create the wgpu adapter & device
    pub wgpu_setup: WgpuSetup,

    /// Callback for surface status changes.
    ///
    /// Called with the [`wgpu::CurrentSurfaceTexture`] result whenever acquiring a frame
    /// does not return [`wgpu::CurrentSurfaceTexture::Success`]. For
    /// [`wgpu::CurrentSurfaceTexture::Suboptimal`], egui uses the frame as-is and
    /// defers surface reconfiguration to the next frame — the callback is not invoked
    /// in that case either.
    pub on_surface_status:
        Arc<dyn Fn(&wgpu::CurrentSurfaceTexture) -> SurfaceErrorAction + Send + Sync>,
}

#[test]
fn wgpu_config_impl_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<WgpuConfiguration>();
}

impl core::fmt::Debug for WgpuConfiguration {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let Self {
            surface,
            wgpu_setup,
            on_surface_status: _,
        } = self;
        f.debug_struct("WgpuConfiguration")
            .field("surface", &surface)
            .field("wgpu_setup", &wgpu_setup)
            .finish_non_exhaustive()
    }
}

impl WgpuConfiguration {
    #[inline]
    pub fn with_surface_config(mut self, surface_config: SurfaceConfig) -> Self {
        self.surface = surface_config;
        self
    }
}

impl Default for WgpuConfiguration {
    fn default() -> Self {
        Self {
            surface: SurfaceConfig::HIGH_THROUGHPUT,

            // No display handle available at this point — callers should replace this with
            // `WgpuSetup::from_display_handle(...)` before creating the instance if one is available.
            wgpu_setup: WgpuSetup::without_display_handle(),
            on_surface_status: Arc::new(|status| match status {
                wgpu::CurrentSurfaceTexture::Outdated => {
                    // The compositor changed the surface (resize, scale, output, …). wgpu
                    // requires us to reconfigure before the next acquire. Skipping would mean
                    // we are stuck in `Outdated` forever.
                    log::trace!("Dropped frame with error: {status:?}");
                    SurfaceErrorAction::Reconfigure
                }
                wgpu::CurrentSurfaceTexture::Lost => {
                    // The underlying surface is gone and we need a fresh one from the `wgpu::Instance`.
                    log::debug!("Dropped frame with error: {status:?}");
                    SurfaceErrorAction::RecreateSurface
                }
                wgpu::CurrentSurfaceTexture::Occluded => {
                    // App is hidden (minimized / behind another window). Skip silently.
                    log::trace!("Skipping frame due to occlusion.");
                    SurfaceErrorAction::SkipFrame
                }
                _ => {
                    log::warn!("Dropped frame with error: {status:?}");
                    SurfaceErrorAction::SkipFrame
                }
            }),
        }
    }
}

/// Find the framebuffer format that egui prefers
///
/// # Errors
/// Returns [`WgpuError::NoSurfaceFormatsAvailable`] if the given list of formats is empty.
pub fn preferred_framebuffer_format(
    formats: &[wgpu::TextureFormat],
) -> Result<wgpu::TextureFormat, WgpuError> {
    for &format in formats {
        if matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
        ) {
            return Ok(format);
        }
    }

    formats
        .first()
        .copied()
        .ok_or(WgpuError::NoSurfaceFormatsAvailable)
}

/// Take's epi's depth/stencil bits and returns the corresponding wgpu format.
pub fn depth_format_from_bits(depth_buffer: u8, stencil_buffer: u8) -> Option<wgpu::TextureFormat> {
    match (depth_buffer, stencil_buffer) {
        (0, 8) => Some(wgpu::TextureFormat::Stencil8),
        (16, 0) => Some(wgpu::TextureFormat::Depth16Unorm),
        (24, 0) => Some(wgpu::TextureFormat::Depth24Plus),
        (24, 8) => Some(wgpu::TextureFormat::Depth24PlusStencil8),
        (32, 0) => Some(wgpu::TextureFormat::Depth32Float),
        (32, 8) => Some(wgpu::TextureFormat::Depth32FloatStencil8),
        _ => None,
    }
}

// ---------------------------------------------------------------------------

fn log_adapter_info(info: &wgpu::AdapterInfo) {
    let summary = adapter_info_summary(info);

    let is_test = cfg!(test); // Software rasterizers are expected (and preferred) during testing!

    if info.device_type == wgpu::DeviceType::Cpu && !is_test {
        log::warn!("Software rasterizer detected - loss of performance expected. {summary}");
    } else {
        log::debug!("wgpu adapter: {summary}");
    }
}

/// A human-readable summary about an adapter
pub fn adapter_info_summary(info: &wgpu::AdapterInfo) -> String {
    let wgpu::AdapterInfo {
        name,
        vendor,
        device,
        device_type,
        driver,
        driver_info,
        backend,
        device_pci_bus_id,
        subgroup_min_size,
        subgroup_max_size,
        transient_saves_memory,
        limit_bucket,
    } = &info;

    // Example values:
    // > name: "llvmpipe (LLVM 16.0.6, 256 bits)", device_type: Cpu, backend: Vulkan, driver: "llvmpipe", driver_info: "Mesa 23.1.6-arch1.4 (LLVM 16.0.6)"
    // > name: "Apple M1 Pro", device_type: IntegratedGpu, backend: Metal, driver: "", driver_info: ""
    // > name: "ANGLE (Apple, Apple M1 Pro, OpenGL 4.1)", device_type: IntegratedGpu, backend: Gl, driver: "", driver_info: ""

    use core::fmt::Write as _;

    let mut summary = format!("backend: {backend:?}, device_type: {device_type:?}");

    if !name.is_empty() {
        write!(summary, ", name: {name:?}").ok();
    }
    if !driver.is_empty() {
        write!(summary, ", driver: {driver:?}").ok();
    }
    if !driver_info.is_empty() {
        write!(summary, ", driver_info: {driver_info:?}").ok();
    }
    if *vendor != 0 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            write!(
                summary,
                ", vendor: {} (0x{vendor:04X})",
                parse_vendor_id(*vendor)
            )
            .ok();
        }
        #[cfg(target_arch = "wasm32")]
        {
            write!(summary, ", vendor: 0x{vendor:04X}").ok();
        }
    }
    if *device != 0 {
        write!(summary, ", device: 0x{device:02X}").ok();
    }
    if !device_pci_bus_id.is_empty() {
        write!(summary, ", pci_bus_id: {device_pci_bus_id:?}").ok();
    }
    if *subgroup_min_size != 0 || *subgroup_max_size != 0 {
        write!(
            summary,
            ", subgroup_size: {subgroup_min_size}..={subgroup_max_size}"
        )
        .ok();
    }
    write!(
        summary,
        ", transient_saves_memory: {transient_saves_memory:?}"
    )
    .ok();
    write!(summary, ", limit_bucket: {limit_bucket:?}").ok();

    summary
}

/// Tries to parse the adapter's vendor ID to a human-readable string.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_vendor_id(vendor_id: u32) -> &'static str {
    match vendor_id {
        wgpu::hal::auxil::db::amd::VENDOR => "AMD",
        wgpu::hal::auxil::db::apple::VENDOR => "Apple",
        wgpu::hal::auxil::db::arm::VENDOR => "ARM",
        wgpu::hal::auxil::db::broadcom::VENDOR => "Broadcom",
        wgpu::hal::auxil::db::imgtec::VENDOR => "Imagination Technologies",
        wgpu::hal::auxil::db::intel::VENDOR => "Intel",
        wgpu::hal::auxil::db::mesa::VENDOR => "Mesa",
        wgpu::hal::auxil::db::nvidia::VENDOR => "NVIDIA",
        wgpu::hal::auxil::db::qualcomm::VENDOR => "Qualcomm",
        _ => "Unknown",
    }
}
