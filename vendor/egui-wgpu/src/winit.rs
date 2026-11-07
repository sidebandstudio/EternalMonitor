#![expect(clippy::missing_errors_doc)]
#![expect(clippy::undocumented_unsafe_blocks)]
#![expect(clippy::unwrap_used)] // TODO(emilk): avoid unwraps
#![expect(unsafe_code)]

use crate::{RenderState, SurfaceConfig, SurfaceErrorAction, WgpuConfiguration, renderer};
use crate::{
    RendererOptions,
    capture::{CaptureReceiver, CaptureSender, CaptureState, capture_channel},
};
use core::num::NonZeroU32;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use egui::{Context, Event, UserData, ViewportId, ViewportIdMap, ViewportIdSet};
use std::sync::Arc;
use web_time::Instant;

/// EternalMonitor patch: frames the painter skipped because its device was lost or was
/// being replaced. An app that tells a working window by its frames counts these as
/// failures.
pub static SKIPPED_FRAMES: AtomicU64 = AtomicU64::new(0);

/// EternalMonitor patch: devices the painter dropped because they were lost, whether or
/// not a replacement was ever made.
pub static DEVICES_LOST: AtomicU64 = AtomicU64::new(0);

/// EternalMonitor patch: how long a replacement device may take before the try counts as
/// failed. A driver call can stall for much longer, so the window never waits on it.
pub const REPLACEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// EternalMonitor patch: the name of the thread that makes a replacement device. A panic
/// there ends only that try, so an app's panic hook can tell it from others.
pub const REPLACEMENT_THREAD: &str = "wgpu device replacement";

/// EternalMonitor patch: how often the window looks for a replacement device being made.
const REPLACEMENT_POLL: Duration = Duration::from_millis(100);

/// EternalMonitor patch: what [`Answer::poll`] found.
#[derive(Debug)]
pub enum Asked<T> {
    Answered(T),
    Waiting,
    /// No answer by the deadline. The next deadline is a timeout later; a late answer
    /// still arrives.
    TimedOut,
    /// The thread ended without an answer, as by a panic.
    Ended,
}

/// EternalMonitor patch: work done on its own thread, which the window polls without ever
/// waiting for it.
pub struct Answer<T> {
    receiver: std::sync::mpsc::Receiver<T>,
    deadline: Instant,
    timeout: Duration,
}

impl<T: Send + 'static> Answer<T> {
    pub fn ask(
        name: &str,
        now: Instant,
        timeout: Duration,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _ = sender.send(work());
            })?;
        Ok(Self {
            receiver,
            deadline: now + timeout,
            timeout,
        })
    }

    /// Never blocks.
    pub fn poll(&mut self, now: Instant) -> Asked<T> {
        match self.receiver.try_recv() {
            Ok(answer) => Asked::Answered(answer),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Asked::Ended,
            Err(std::sync::mpsc::TryRecvError::Empty) if now >= self.deadline => {
                self.deadline = now + self.timeout;
                Asked::TimedOut
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Asked::Waiting,
        }
    }
}

/// EternalMonitor patch: run `future` on this thread. wgpu's native futures are ready at
/// once; this parks until woken otherwise.
fn block_on<F: core::future::Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = core::pin::pin!(future);
    loop {
        if let core::task::Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        std::thread::park();
    }
}

/// EternalMonitor patch: a replacement device being made on another thread, with the
/// surface it was made for.
type Replacing = Answer<(
    wgpu::Surface<'static>,
    Result<RenderState, crate::WgpuError>,
)>;

/// EternalMonitor patch: when the painter tries to replace a lost device, and when it
/// stops trying.
///
/// The first try is at once. Each failure, a device that could not be made or a new one
/// lost before it drew for [`Self::STABLE_AFTER`], waits twice as long as the last, from
/// 0.5 s up to 30 s. After [`Self::MAX_FAILURES`] the painter gives up and the window
/// stays blank. A device that drew for [`Self::STABLE_AFTER`] starts the count again.
#[derive(Debug, Default)]
pub struct Replacement {
    failures: u32,
    next_try: Option<Instant>,
    drawing_since: Option<Instant>,
    gave_up: bool,
}

impl Replacement {
    pub const MAX_FAILURES: u32 = 10;
    pub const STABLE_AFTER: Duration = Duration::from_secs(10);
    const FIRST_WAIT: Duration = Duration::from_millis(500);
    const LONGEST_WAIT: Duration = Duration::from_secs(30);

    /// The device was lost at `now`. True when the painter just gave up.
    pub fn lost(&mut self, now: Instant) -> bool {
        let drew = self
            .drawing_since
            .is_some_and(|since| now.duration_since(since) >= Self::STABLE_AFTER);
        self.drawing_since = None;
        if drew {
            self.failures = 0;
            self.next_try = Some(now);
            return false;
        }
        if self.next_try.is_none() {
            // The device the window started with: its replacement is the first try.
            self.next_try = Some(now);
            return false;
        }
        self.failed(now)
    }

    /// A replacement could not be made at `now`. True when the painter just gave up.
    pub fn failed(&mut self, now: Instant) -> bool {
        if self.gave_up {
            return false;
        }
        self.failures += 1;
        if self.failures >= Self::MAX_FAILURES {
            self.gave_up = true;
            return true;
        }
        let wait = Self::FIRST_WAIT.saturating_mul(1 << (self.failures - 1).min(16));
        self.next_try = Some(now + wait.min(Self::LONGEST_WAIT));
        false
    }

    /// The device presented a frame at `now`.
    pub fn drew(&mut self, now: Instant) {
        self.drawing_since.get_or_insert(now);
    }

    /// How long until the next try, or `None` once the painter gave up.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        (!self.gave_up).then(|| {
            self.next_try
                .map_or(Duration::ZERO, |at| at.saturating_duration_since(now))
        })
    }
}

struct SurfaceState {
    surface: wgpu::Surface<'static>,
    alpha_mode: wgpu::CompositeAlphaMode,
    width: u32,
    height: u32,
    resizing: bool,
    needs_reconfigure: bool,
    needs_recreate: bool,
}

/// Everything you need to paint egui with [`wgpu`] on [`winit`].
///
/// Alternatively you can use [`crate::Renderer`] directly.
///
/// NOTE: all egui viewports share the same painter.
pub struct Painter {
    context: Context,
    config: WgpuConfiguration,
    options: RendererOptions,
    support_transparent_backbuffer: bool,
    screen_capture_state: Option<CaptureState>,

    instance: wgpu::Instance,
    render_state: Option<RenderState>,

    // Per viewport/window:
    depth_texture_view: ViewportIdMap<wgpu::TextureView>,
    msaa_texture_view: ViewportIdMap<wgpu::TextureView>,
    surfaces: ViewportIdMap<SurfaceState>,
    capture_tx: CaptureSender,
    capture_rx: CaptureReceiver,

    // EternalMonitor patch: device-loss recovery.
    /// Set when the current device is lost: by its lost callback, or when a frame's upload
    /// fails. The next paint drops the device and everything made on it.
    device_lost: Arc<AtomicBool>,
    /// The lost device was dropped; [`Self::set_window`] builds a new one when
    /// [`Self::replacement`] says, and a failure is retried instead of returned.
    rebuilding: bool,
    /// The new device's first paint uploads the font atlas again.
    restore_fonts: bool,
    /// When to try replacing a lost device, and when to stop.
    replacement: Replacement,
    /// The replacement being made, if any. Never more than one: a stuck one is waited
    /// for, not joined by another.
    replacing: Option<Replacing>,
}

impl Painter {
    /// Manages [`wgpu`] state, including surface state, required to render egui.
    ///
    /// Only the [`wgpu::Instance`] is initialized here. Device selection and the initialization
    /// of render + surface state is deferred until the painter is given its first window target
    /// via [`set_window()`](Self::set_window). (Ensuring that a device that's compatible with the
    /// native window is chosen)
    ///
    /// Before calling [`paint_and_update_textures()`](Self::paint_and_update_textures) a
    /// [`wgpu::Surface`] must be initialized (and corresponding render state) by calling
    /// [`set_window()`](Self::set_window) once you have
    /// a [`winit::window::Window`] with a valid `.raw_window_handle()`
    /// associated.
    pub async fn new(
        context: Context,
        config: WgpuConfiguration,
        support_transparent_backbuffer: bool,
        options: RendererOptions,
    ) -> Self {
        let (capture_tx, capture_rx) = capture_channel();
        let instance = config.wgpu_setup.new_instance().await;

        Self {
            context,
            config,
            options,
            support_transparent_backbuffer,
            screen_capture_state: None,

            instance,
            render_state: None,

            depth_texture_view: Default::default(),
            surfaces: Default::default(),
            msaa_texture_view: Default::default(),

            capture_tx,
            capture_rx,

            device_lost: Arc::new(AtomicBool::new(false)),
            rebuilding: false,
            restore_fonts: false,
            replacement: Replacement::default(),
            replacing: None,
        }
    }

    /// Get the [`RenderState`].
    ///
    /// Will return [`None`] if the render state has not been initialized yet.
    pub fn render_state(&self) -> Option<RenderState> {
        self.render_state.clone()
    }

    fn configure_surface(
        surface_state: &SurfaceState,
        render_state: &RenderState,
        config: &SurfaceConfig,
        device_lost: &AtomicBool,
    ) {
        profiling::function_scope!();

        let SurfaceConfig {
            present_mode,
            desired_maximum_frame_latency,
        } = *config;

        // Transaction presentation can hold a drawable during AppKit live resize. Keep the
        // configured low-latency path normally, but use three Metal drawables while resizing.
        #[cfg(all(target_os = "macos", feature = "macos-window-resize-jitter-fix"))]
        let desired_maximum_frame_latency = if surface_state.resizing {
            Some(desired_maximum_frame_latency.unwrap_or(2).max(2))
        } else {
            desired_maximum_frame_latency
        };

        let width = surface_state.width;
        let height = surface_state.height;

        // EternalMonitor patch: a lost device's adapter may support no surface before its
        // lost callback runs. Replace the device instead of panicking.
        let Some(default_config) =
            surface_state
                .surface
                .get_default_config(&render_state.adapter, width, height)
        else {
            log::warn!("The surface isn't supported by this adapter; replacing the device");
            device_lost.store(true, Ordering::SeqCst);
            return;
        };
        let mut surf_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: render_state.target_format,
            present_mode,
            alpha_mode: surface_state.alpha_mode,
            view_formats: vec![render_state.target_format],
            ..default_config
        };

        if let Some(desired_maximum_frame_latency) = desired_maximum_frame_latency {
            surf_config.desired_maximum_frame_latency = desired_maximum_frame_latency;
        }

        surface_state
            .surface
            .configure(&render_state.device, &surf_config);
    }

    /// Drop the existing [`wgpu::Surface`] for `viewport_id` and create a fresh one for the
    /// given window via [`wgpu::Instance::create_surface`], then configure it.
    ///
    /// Used to recover from [`wgpu::CurrentSurfaceTexture::Lost`], where reconfiguring the
    /// existing surface object cannot recover.
    fn recreate_surface(
        &mut self,
        viewport_id: ViewportId,
        window: &Arc<winit::window::Window>,
    ) -> Result<(), crate::WgpuError> {
        profiling::function_scope!();

        let Some(old_state) = self.surfaces.remove(&viewport_id) else {
            return Ok(());
        };

        let surface = self.instance.create_surface(Arc::clone(window))?;
        self.install_surface(
            surface,
            viewport_id,
            old_state.width,
            old_state.height,
            old_state.resizing,
        );
        Ok(())
    }

    /// Updates (or clears) the [`winit::window::Window`] associated with the [`Painter`]
    ///
    /// This creates a [`wgpu::Surface`] for the given Window (as well as initializing render
    /// state if needed) that is used for egui rendering.
    ///
    /// This must be called before trying to render via
    /// [`paint_and_update_textures`](Self::paint_and_update_textures)
    ///
    /// # Portability
    ///
    /// _In particular it's important to note that on Android a it's only possible to create
    /// a window surface between `Resumed` and `Paused` lifecycle events, and Winit will panic on
    /// attempts to query the raw window handle while paused._
    ///
    /// On Android [`set_window`](Self::set_window) should be called with `Some(window)` for each
    /// `Resumed` event and `None` for each `Paused` event. Currently, on all other platforms
    /// [`set_window`](Self::set_window) may be called with `Some(window)` as soon as you have a
    /// valid [`winit::window::Window`].
    ///
    /// # Errors
    /// If the provided wgpu configuration does not match an available device.
    pub async fn set_window(
        &mut self,
        viewport_id: ViewportId,
        window: Option<Arc<winit::window::Window>>,
    ) -> Result<(), crate::WgpuError> {
        profiling::scope!("Painter::set_window"); // profile_function gives bad names for async functions

        if let Some(window) = window {
            let size = window.inner_size();
            if !self.surfaces.contains_key(&viewport_id) {
                // EternalMonitor patch: a lost device is replaced off this thread.
                if self.rebuilding {
                    self.replace_device(viewport_id, window);
                    return Ok(());
                }
                let surface = self.instance.create_surface(window)?;
                self.add_surface(surface, viewport_id, size).await?;
            }
        } else {
            log::warn!("No window - clearing all surfaces");
            self.surfaces.clear();
        }
        Ok(())
    }

    /// Updates (or clears) the [`winit::window::Window`] associated with the [`Painter`] without taking ownership of the window.
    ///
    /// Like [`set_window`](Self::set_window) except:
    ///
    /// # Safety
    /// The user is responsible for ensuring that the window is alive for as long as it is set.
    pub async unsafe fn set_window_unsafe(
        &mut self,
        viewport_id: ViewportId,
        window: Option<&winit::window::Window>,
    ) -> Result<(), crate::WgpuError> {
        profiling::scope!("Painter::set_window_unsafe"); // profile_function gives bad names for async functions

        if let Some(window) = window {
            let size = window.inner_size();
            if !self.surfaces.contains_key(&viewport_id) {
                let surface = unsafe {
                    self.instance
                        .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(&window)?)?
                };
                self.add_surface(surface, viewport_id, size).await?;
            }
        } else {
            log::warn!("No window - clearing all surfaces");
            self.surfaces.clear();
        }
        Ok(())
    }

    async fn add_surface(
        &mut self,
        surface: wgpu::Surface<'static>,
        viewport_id: ViewportId,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> Result<(), crate::WgpuError> {
        if self.render_state.is_none() {
            let render_state =
                RenderState::create(&self.config, &self.instance, Some(&surface), self.options)
                    .await?;
            self.watch_device(&render_state.device);
            self.render_state = Some(render_state);
        }
        self.install_surface(surface, viewport_id, size.width, size.height, false);
        Ok(())
    }

    /// EternalMonitor patch: flag the loss of `device`. An error wgpu reports outside an
    /// error scope flags it too, instead of panicking as wgpu's default handler does: a
    /// window whose device or surface fails is drawn again on a new one.
    fn watch_device(&mut self, device: &wgpu::Device) {
        let lost = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&lost);
        device.set_device_lost_callback(move |reason, message| {
            log::warn!("The wgpu device was lost ({reason:?}): {message}");
            flag.store(true, Ordering::SeqCst);
        });
        let flag = Arc::clone(&lost);
        device.on_uncaptured_error(Arc::new(move |error| {
            if flag.swap(true, Ordering::SeqCst) {
                log::debug!("wgpu error on the lost device: {error}");
            } else {
                log::error!("wgpu error; replacing the window's device: {error}");
            }
        }));
        self.device_lost = lost;
    }

    /// EternalMonitor patch: forget a lost device and everything made on it. eframe calls
    /// [`Self::set_window`] before each paint, which builds a new device and surfaces.
    fn drop_lost_device(&mut self) {
        self.render_state = None;
        self.surfaces.clear();
        self.depth_texture_view.clear();
        self.msaa_texture_view.clear();
        self.screen_capture_state = None;
        self.rebuilding = true;
        self.restore_fonts = false;
        DEVICES_LOST.fetch_add(1, Ordering::SeqCst);
        let now = Instant::now();
        if self.replacement.lost(now) {
            Self::log_gave_up(&"the new device was lost too");
        } else {
            log::warn!("Rebuilding the window renderer after its wgpu device was lost");
            if let Some(wait) = self.replacement.wait(now) {
                self.context.request_repaint_after(wait);
            }
        }
    }

    /// EternalMonitor patch: work towards a new device for a lost one, without blocking.
    ///
    /// The surface is made here, on the window's thread: it is the window's (Metal wants
    /// it on the main thread) and calls no driver for a device. Listing adapters and
    /// making the device, where a driver can stall, run on another thread, which this
    /// polls. One that hasn't answered in [`REPLACEMENT_TIMEOUT`] counts as a failed try,
    /// and no other starts while it is out; its late device is still taken. Configuring
    /// the new surface, which makes the swapchain, stays here too: DXGI sends the window
    /// messages while it does.
    fn replace_device(&mut self, viewport_id: ViewportId, window: Arc<winit::window::Window>) {
        let now = Instant::now();
        if let Some(replacing) = &mut self.replacing {
            match replacing.poll(now) {
                Asked::Waiting => {}
                Asked::TimedOut => self.replacement_failed(now, &"no new device within 10 s"),
                Asked::Ended => {
                    self.replacing = None;
                    self.replacement_failed(now, &"the thread making it ended");
                }
                Asked::Answered((surface, Ok(render_state))) => {
                    self.replacing = None;
                    self.watch_device(&render_state.device);
                    self.render_state = Some(render_state);
                    let size = window.inner_size();
                    self.install_surface(surface, viewport_id, size.width, size.height, false);
                    self.rebuilding = false;
                    self.restore_fonts = true;
                    log::info!("Replaced the lost wgpu device");
                    return;
                }
                Asked::Answered((_, Err(err))) => {
                    self.replacing = None;
                    self.replacement_failed(now, &err);
                }
            }
            if let Some(wait) = self.next_check(now) {
                self.context.request_repaint_after(wait);
            }
            return;
        }
        match self.replacement.wait(now) {
            None => return,
            Some(wait) if !wait.is_zero() => {
                self.context.request_repaint_after(wait);
                return;
            }
            Some(_) => {}
        }
        let surface = match self.instance.create_surface(window) {
            Ok(surface) => surface,
            Err(err) => {
                self.replacement_failed(now, &err);
                return;
            }
        };
        let (config, instance, options) =
            (self.config.clone(), self.instance.clone(), self.options);
        let asked = Answer::ask(REPLACEMENT_THREAD, now, REPLACEMENT_TIMEOUT, move || {
            let made = block_on(RenderState::create(
                &config,
                &instance,
                Some(&surface),
                options,
            ));
            (surface, made)
        });
        match asked {
            Ok(replacing) => {
                self.replacing = Some(replacing);
                self.context.request_repaint_after(REPLACEMENT_POLL);
            }
            Err(err) => self.replacement_failed(now, &err),
        }
    }

    /// EternalMonitor patch: a try at a new device failed at `now`.
    fn replacement_failed(&mut self, now: Instant, why: &dyn core::fmt::Display) {
        if self.replacement.failed(now) {
            Self::log_gave_up(why);
            // A stuck thread may still answer; nothing waits for it.
            self.replacing = None;
        } else {
            log::warn!("Could not replace the lost wgpu device yet: {why}");
        }
    }

    /// EternalMonitor patch: when the window should look again at a device being replaced,
    /// or `None` once the painter gave up.
    fn next_check(&self, now: Instant) -> Option<Duration> {
        if self.replacing.is_some() {
            Some(REPLACEMENT_POLL)
        } else {
            self.replacement.wait(now)
        }
    }

    /// EternalMonitor patch: said once, when [`Replacement`] gives up.
    fn log_gave_up(last: &dyn core::fmt::Display) {
        log::error!(
            "Gave up replacing the window's lost wgpu device after {} failures ({last}); the window stays blank",
            Replacement::MAX_FAILURES
        );
    }

    /// Inserts a freshly created surface into [`Self::surfaces`] and configures it.
    ///
    /// Render state must already be initialised before calling this.
    // NOTE: The same assumption is already required by `resize_and_generate_depth_texture_view_and_msaa_view`.
    fn install_surface(
        &mut self,
        surface: wgpu::Surface<'static>,
        viewport_id: ViewportId,
        width: u32,
        height: u32,
        resizing: bool,
    ) {
        let alpha_mode = {
            // Panic: We use the same failure mode as `resize_and_generate_depth_texture_view_and_msaa_view`
            let render_state = self
                .render_state
                .as_ref()
                .expect("install_surface called before render_state initialization");
            if self.support_transparent_backbuffer {
                let supported_alpha_modes =
                    surface.get_capabilities(&render_state.adapter).alpha_modes;
                // Prefer pre multiplied over post multiplied!
                if supported_alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                    wgpu::CompositeAlphaMode::PreMultiplied
                } else if supported_alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied)
                {
                    wgpu::CompositeAlphaMode::PostMultiplied
                } else {
                    log::warn!(
                        "Transparent window was requested, but the active wgpu surface does not support a `CompositeAlphaMode` with transparency."
                    );
                    wgpu::CompositeAlphaMode::Auto
                }
            } else {
                wgpu::CompositeAlphaMode::Auto
            }
        };
        self.surfaces.insert(
            viewport_id,
            SurfaceState {
                surface,
                width,
                height,
                alpha_mode,
                resizing,
                needs_reconfigure: false,
                needs_recreate: false,
            },
        );
        let Some(width) = NonZeroU32::new(width) else {
            log::debug!("The window width was zero; skipping generate textures");
            return;
        };
        let Some(height) = NonZeroU32::new(height) else {
            log::debug!("The window height was zero; skipping generate textures");
            return;
        };
        self.resize_and_generate_depth_texture_view_and_msaa_view(viewport_id, width, height);
    }

    /// Returns the maximum texture dimension supported if known
    ///
    /// This API will only return a known dimension after `set_window()` has been called
    /// at least once, since the underlying device and render state are initialized lazily
    /// once we have a window (that may determine the choice of adapter/device).
    pub fn max_texture_side(&self) -> Option<usize> {
        self.render_state
            .as_ref()
            .map(|rs| rs.device.limits().max_texture_dimension_2d as usize)
    }

    fn resize_and_generate_depth_texture_view_and_msaa_view(
        &mut self,
        viewport_id: ViewportId,
        width_in_pixels: NonZeroU32,
        height_in_pixels: NonZeroU32,
    ) {
        profiling::function_scope!();

        let width = width_in_pixels.get();
        let height = height_in_pixels.get();

        let render_state = self.render_state.as_ref().unwrap();
        let surface_state = self.surfaces.get_mut(&viewport_id).unwrap();

        surface_state.width = width;
        surface_state.height = height;

        Self::configure_surface(
            surface_state,
            render_state,
            &self.config.surface,
            &self.device_lost,
        );

        if let Some(depth_format) = self.options.depth_stencil_format {
            self.depth_texture_view.insert(
                viewport_id,
                render_state
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("egui_depth_texture"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: self.options.msaa_samples.max(1),
                        dimension: wgpu::TextureDimension::D2,
                        format: depth_format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[depth_format],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
        }

        if let Some(render_state) = (self.options.msaa_samples > 1)
            .then_some(self.render_state.as_ref())
            .flatten()
        {
            let texture_format = render_state.target_format;
            self.msaa_texture_view.insert(
                viewport_id,
                render_state
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("egui_msaa_texture"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: self.options.msaa_samples.max(1),
                        dimension: wgpu::TextureDimension::D2,
                        format: texture_format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[texture_format],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
        }
    }

    /// Handles changes of the resizing state.
    ///
    /// Should be called prior to the first [`Painter::on_window_resized`] call and after the last in
    /// the chain. Used to apply platform-specific logic, e.g. OSX Metal window resize jitter fix.
    pub fn on_window_resize_state_change(&mut self, viewport_id: ViewportId, resizing: bool) {
        profiling::function_scope!();

        let Some(state) = self.surfaces.get_mut(&viewport_id) else {
            return;
        };
        if state.resizing == resizing {
            if resizing {
                log::debug!(
                    "Painter::on_window_resize_state_change() redundant call while resizing"
                );
            } else {
                log::debug!(
                    "Painter::on_window_resize_state_change() redundant call after resizing"
                );
            }
            return;
        }

        // Set before reconfiguring so macOS live resize uses the temporary latency bump above.
        state.resizing = resizing;

        // Resizing is a bit tricky on macOS.
        // It requires enabling ["present_with_transaction"](https://developer.apple.com/documentation/quartzcore/cametallayer/presentswithtransaction)
        // flag to avoid jittering during the resize. Even though resize jittering on macOS
        // is common across rendering backends, the solution for wgpu/metal is known.
        //
        // See https://github.com/emilk/egui/issues/903
        #[cfg(all(target_os = "macos", feature = "macos-window-resize-jitter-fix"))]
        {
            // SAFETY: `as_hal::<Metal>()` returns `None` unless this surface is backed by wgpu's
            // Metal backend.
            unsafe {
                if let (Some(render_state), Some(hal_surface)) = (
                    self.render_state.as_ref(),
                    state.surface.as_hal::<wgpu::hal::api::Metal>(),
                ) {
                    hal_surface
                        .render_layer()
                        .lock()
                        .setPresentsWithTransaction(resizing);

                    Self::configure_surface(
                        state,
                        render_state,
                        &self.config.surface,
                        &self.device_lost,
                    );
                }
            }
        }
    }

    pub fn on_window_resized(
        &mut self,
        viewport_id: ViewportId,
        width_in_pixels: NonZeroU32,
        height_in_pixels: NonZeroU32,
    ) {
        profiling::function_scope!();

        if self.surfaces.contains_key(&viewport_id) {
            self.resize_and_generate_depth_texture_view_and_msaa_view(
                viewport_id,
                width_in_pixels,
                height_in_pixels,
            );
        } else {
            log::warn!(
                "Ignoring window resize notification with no surface created via Painter::set_window()"
            );
        }
    }

    /// Returns two things:
    ///
    /// The approximate number of seconds spent on vsync-waiting (if any),
    /// and the captures captured screenshot if it was requested.
    ///
    /// If `capture_data` isn't empty, a screenshot will be captured.
    #[expect(clippy::too_many_arguments)]
    pub fn paint_and_update_textures(
        &mut self,
        viewport_id: ViewportId,
        pixels_per_point: f32,
        clear_color: [f32; 4],
        clipped_primitives: &[epaint::ClippedPrimitive],
        textures_delta: &mut epaint::textures::TexturesDelta,
        capture_data: Vec<UserData>,
        window: &Arc<winit::window::Window>,
    ) -> f32 {
        profiling::function_scope!();

        /// Guard to ensure that commands are always submitted to the renderer queue
        /// so that calls to [`write_buffer()`](https://docs.rs/wgpu/latest/wgpu/struct.Queue.html#method.write_buffer)
        /// are completed even if we take a codepath which doesn't submit commands and avoids
        /// internal buffers growing indefinitely.
        ///
        /// This may happen, for example, if no output frame is resolved.
        /// See <https://github.com/emilk/egui/pull/7928> for full context.
        struct RendererQueueGuard<'q> {
            queue: &'q wgpu::Queue,
            commands_submitted: bool,
        }

        impl Drop for RendererQueueGuard<'_> {
            fn drop(&mut self) {
                // Only submit an empty command buffer array if no commands were
                // explicitly submitted.
                if !self.commands_submitted {
                    self.queue.submit([]);
                }
            }
        }

        let capture = !capture_data.is_empty();
        let mut vsync_sec = 0.0;

        // EternalMonitor patch: while a lost device is replaced there is nothing to draw
        // with; set_window makes the next one when `replacement` says. A device lost since
        // the last frame draws nothing again, so it goes.
        if self.rebuilding {
            SKIPPED_FRAMES.fetch_add(1, Ordering::SeqCst);
            if let Some(wait) = self.next_check(Instant::now()) {
                self.context.request_repaint_after(wait);
            }
            return vsync_sec;
        }
        if self.device_lost.load(Ordering::SeqCst) {
            SKIPPED_FRAMES.fetch_add(1, Ordering::SeqCst);
            self.drop_lost_device();
            return vsync_sec;
        }

        // If the previous frame produced `CurrentSurfaceTexture::Lost`, the action match
        // below set `needs_recreate`. Recreate the surface now, before re-borrowing
        // `self.render_state` / `self.surfaces` for the rest of the paint.
        if self
            .surfaces
            .get(&viewport_id)
            .is_some_and(|s| s.needs_recreate)
            && let Err(err) = self.recreate_surface(viewport_id, window)
        {
            log::error!("Failed to recreate surface for {viewport_id:?}: {err}");
            return vsync_sec;
        }

        // Apply any runtime changes requested via `RenderState::surface_config`.
        // We diff against the already-applied values in `self.config.surface`
        // and, if anything differs, mark every surface as needing reconfiguration so
        // the existing `needs_reconfigure` pathway below picks them up.
        if let Some(render_state) = self.render_state.as_ref()
            && render_state.surface_config != self.config.surface
        {
            self.config.surface = render_state.surface_config;
            #[expect(clippy::iter_over_hash_type)]
            for surface in self.surfaces.values_mut() {
                surface.needs_reconfigure = true;
            }
        }

        let Some(render_state) = self.render_state.as_mut() else {
            return vsync_sec;
        };

        let mut render_queue_guard = RendererQueueGuard {
            queue: &render_state.queue,
            commands_submitted: false,
        };

        let Some(surface_state) = self.surfaces.get_mut(&viewport_id) else {
            return vsync_sec;
        };

        let mut encoder =
            render_state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("encoder"),
                });

        // Upload all resources for the GPU.
        let screen_descriptor = renderer::ScreenDescriptor {
            size_in_pixels: [surface_state.width, surface_state.height],
            pixels_per_point,
        };

        let user_cmd_bufs = {
            let mut renderer = render_state.renderer.write();
            // EternalMonitor patch: the font atlas went with the lost device. Other textures
            // are the app's to load again.
            if core::mem::take(&mut self.restore_fonts) {
                let atlas = self.context.fonts(|fonts| fonts.image());
                renderer.update_texture(
                    &render_state.device,
                    &render_state.queue,
                    epaint::TextureId::default(),
                    &epaint::ImageDelta::full(atlas, epaint::TextureAtlas::texture_options()),
                );
            }
            #[expect(clippy::iter_over_hash_type)] // Order doesn't matter here
            for (id, image_deltas) in textures_delta.set.drain() {
                for image_delta in image_deltas {
                    renderer.update_texture(
                        &render_state.device,
                        &render_state.queue,
                        id,
                        &image_delta,
                    );
                }
            }

            renderer.update_buffers(
                &render_state.device,
                &render_state.queue,
                &mut encoder,
                clipped_primitives,
                &screen_descriptor,
            )
        };

        // EternalMonitor patch: skip a frame whose upload failed; the next one replaces the
        // device.
        if render_state.renderer.read().upload_failed() || self.device_lost.load(Ordering::SeqCst) {
            SKIPPED_FRAMES.fetch_add(1, Ordering::SeqCst);
            self.device_lost.store(true, Ordering::SeqCst);
            self.context.request_repaint_of(viewport_id);
            return vsync_sec;
        }

        if surface_state.needs_reconfigure {
            Self::configure_surface(
                surface_state,
                render_state,
                &self.config.surface,
                &self.device_lost,
            );
            surface_state.needs_reconfigure = false;
        }

        let output_frame = {
            profiling::scope!("get_current_texture");
            // This is what vsync-waiting happens on my Mac.
            let start = web_time::Instant::now();
            let output_frame = surface_state.surface.get_current_texture();
            vsync_sec += start.elapsed().as_secs_f32();
            output_frame
        };

        let output_frame = match output_frame {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                surface_state.needs_reconfigure = true;
                frame
            }
            other => {
                match (*self.config.on_surface_status)(&other) {
                    SurfaceErrorAction::Reconfigure => {
                        Self::configure_surface(
                            surface_state,
                            render_state,
                            &self.config.surface,
                            &self.device_lost,
                        );
                        self.context.request_repaint_of(viewport_id);
                    }
                    SurfaceErrorAction::RecreateSurface => {
                        // Because of ownership, I could not find an easy way to do a full recovery here,
                        // as that would involve dropping the old surface and creating a new one.
                        // For now, we defer the recreation to the beginning of the next frame (which
                        // we ensure to arrive via `request_repaint_of`). A cleaner solution would be
                        // to untangle the ownership of `RenderState`.
                        surface_state.needs_recreate = true;
                        self.context.request_repaint_of(viewport_id);
                    }
                    SurfaceErrorAction::SkipFrame => {}
                }
                return vsync_sec;
            }
        };

        let mut capture_buffer = None;
        {
            let renderer = render_state.renderer.read();

            let target_texture = if capture {
                let capture_state = self.screen_capture_state.get_or_insert_with(|| {
                    CaptureState::new(&render_state.device, &output_frame.texture)
                });
                capture_state.update(&render_state.device, &output_frame.texture);

                &capture_state.texture
            } else {
                &output_frame.texture
            };
            let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());

            let (view, resolve_target) = (self.options.msaa_samples > 1)
                .then_some(self.msaa_texture_view.get(&viewport_id))
                .flatten()
                .map_or((&target_view, None), |texture_view| {
                    (texture_view, Some(&target_view))
                });

            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_color[0] as f64,
                            g: clear_color[1] as f64,
                            b: clear_color[2] as f64,
                            a: clear_color[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: self.depth_texture_view.get(&viewport_id).map(|view| {
                    wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: self
                            .options
                            .depth_stencil_format
                            .is_some_and(|depth_stencil_format| {
                                depth_stencil_format.has_depth_aspect()
                            })
                            .then_some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                // It is very unlikely that the depth buffer is needed after egui finished rendering
                                // so no need to store it. (this can improve performance on tiling GPUs like mobile chips or Apple Silicon)
                                store: wgpu::StoreOp::Discard,
                            }),
                        stencil_ops: self
                            .options
                            .depth_stencil_format
                            .is_some_and(|depth_stencil_format| {
                                depth_stencil_format.has_stencil_aspect()
                            })
                            .then_some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(0),
                                store: wgpu::StoreOp::Discard,
                            }),
                    }
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Forgetting the pass' lifetime means that we are no longer compile-time protected from
            // runtime errors caused by accessing the parent encoder before the render pass is dropped.
            // Since we don't pass it on to the renderer, we should be perfectly safe against this mistake here!
            renderer.render(
                &mut render_pass.forget_lifetime(),
                clipped_primitives,
                &screen_descriptor,
            );

            if capture && let Some(capture_state) = &mut self.screen_capture_state {
                capture_buffer = Some(capture_state.copy_textures(
                    &render_state.device,
                    &output_frame,
                    &mut encoder,
                ));
            }
        }

        let encoded = {
            profiling::scope!("CommandEncoder::finish");
            encoder.finish()
        };

        // Submit the commands: both the main buffer and user-defined ones.
        {
            profiling::scope!("Queue::submit");
            // wgpu doesn't document where vsync can happen. Maybe here?
            let start = web_time::Instant::now();
            render_state
                .queue
                .submit(core::iter::chain(user_cmd_bufs, [encoded]));
            vsync_sec += start.elapsed().as_secs_f32();
        };

        // Ensure that the queue guard does not do unnecessary work when dropped
        render_queue_guard.commands_submitted = true;

        // Free textures marked for destruction **after** queue submit since they might still be used in the current frame.
        // Calling `wgpu::Texture::destroy` on a texture that is still in use would invalidate the command buffer(s) it is used in.
        // However, once we called `wgpu::Queue::submit`, it is up for wgpu to determine how long the underlying gpu resource has to live.
        {
            let mut renderer = render_state.renderer.write();
            #[expect(clippy::iter_over_hash_type)] // Order doesn't matter here
            for id in textures_delta.free.drain() {
                renderer.free_texture(&id);
            }
        }

        if let Some(capture_buffer) = capture_buffer
            && let Some(screen_capture_state) = &mut self.screen_capture_state
        {
            screen_capture_state.read_screen_rgba(
                self.context.clone(),
                capture_buffer,
                capture_data,
                self.capture_tx.clone(),
                viewport_id,
            );
        }

        window.pre_present_notify();

        {
            profiling::scope!("present");
            // wgpu doesn't document where vsync can happen. Maybe here?
            let start = web_time::Instant::now();
            render_state.queue.present(output_frame);
            vsync_sec += start.elapsed().as_secs_f32();
        }
        // EternalMonitor patch: a device that keeps drawing starts the replacement count again.
        self.replacement.drew(Instant::now());

        vsync_sec
    }

    /// Call this at the beginning of each frame to receive the requested screenshots.
    pub fn handle_screenshots(&self, events: &mut Vec<Event>) {
        for (viewport_id, user_data, screenshot) in self.capture_rx.try_iter() {
            let screenshot = Arc::new(screenshot);
            for data in user_data {
                events.push(Event::Screenshot {
                    viewport_id,
                    user_data: data,
                    image: Arc::clone(&screenshot),
                });
            }
        }
    }

    pub fn gc_viewports(&mut self, active_viewports: &ViewportIdSet) {
        self.surfaces.retain(|id, _| active_viewports.contains(id));
        self.depth_texture_view
            .retain(|id, _| active_viewports.contains(id));
        self.msaa_texture_view
            .retain(|id, _| active_viewports.contains(id));
    }

    #[expect(clippy::needless_pass_by_ref_mut, clippy::unused_self)]
    pub fn destroy(&mut self) {
        // TODO(emilk): something here?
    }
}
