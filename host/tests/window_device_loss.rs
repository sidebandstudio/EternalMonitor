//! The window survives the loss of its graphics device, as in a driver
//! reset. egui-wgpu 0.36 panicked on the next frame ("Failed to create
//! staging buffer for index data"), which ended the app and the stream; the
//! patched copy in `vendor/egui-wgpu` skips the frame and replaces the device
//! on a schedule (`Replacement`), giving up after repeated failures.

use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::winit::Replacement;
use eframe::egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use eframe::wgpu;

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .expect("a graphics adapter");
    block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("a device")
}

/// One pass of a window with text and a button, tessellated.
fn pass(ctx: &egui::Context) -> (Vec<egui::ClippedPrimitive>, egui::TexturesDelta) {
    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.heading("EternalMonitor");
        let _ = ui.button("Start streaming");
    });
    let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
    (primitives, output.textures_delta)
}

#[test]
fn a_lost_device_skips_the_frame_instead_of_panicking() {
    let (device, queue) = device();
    let mut renderer = Renderer::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        RendererOptions::default(),
    );
    let ctx = egui::Context::default();
    let screen = ScreenDescriptor {
        size_in_pixels: [640, 480],
        pixels_per_point: 1.0,
    };

    let (primitives, mut textures) = pass(&ctx);
    assert!(!primitives.is_empty());
    for (id, deltas) in &textures.set {
        for delta in deltas {
            renderer.update_texture(&device, &queue, *id, delta);
        }
    }
    textures.clear();
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.update_buffers(&device, &queue, &mut encoder, &primitives, &screen);
    queue.submit([encoder.finish()]);
    assert!(!renderer.upload_failed());

    // wgpu now refuses every staging buffer, as after a driver reset.
    device.destroy();
    let (primitives, mut textures) = pass(&ctx);
    textures.clear();
    let mut encoder = device.create_command_encoder(&Default::default());
    let callbacks = renderer.update_buffers(&device, &queue, &mut encoder, &primitives, &screen);
    assert!(callbacks.is_empty());
    assert!(renderer.upload_failed());
}

const SECOND: Duration = Duration::from_secs(1);

/// The waits between tries until the painter gives up, when every
/// replacement fails at once.
fn waits_until_given_up(replacement: &mut Replacement, mut now: Instant) -> Vec<Duration> {
    let mut waits = Vec::new();
    while let Some(wait) = replacement.wait(now) {
        waits.push(wait);
        now += wait;
        assert!(waits.len() <= 20, "never gives up: {waits:?}");
        replacement.failed(now);
    }
    waits
}

#[test]
fn a_lost_device_is_replaced_at_once_then_backs_off_then_gives_up() {
    let start = Instant::now();
    let mut replacement = Replacement::default();
    assert_eq!(replacement.wait(start), Some(Duration::ZERO));
    replacement.drew(start);
    assert!(!replacement.lost(start + SECOND));
    let ms = Duration::from_millis;
    assert_eq!(
        waits_until_given_up(&mut replacement, start + SECOND),
        [
            ms(0),
            ms(500),
            ms(1000),
            ms(2000),
            ms(4000),
            ms(8000),
            ms(16000),
            ms(30000),
            ms(30000),
            ms(30000),
        ]
    );
    assert_eq!(replacement.wait(start + Duration::from_secs(3600)), None);
}

#[test]
fn a_new_device_that_fails_at_once_every_time_ends_in_giving_up() {
    // As with a wgpu error on every frame: each new device is lost again
    // right after it is made.
    let mut now = Instant::now();
    let mut replacement = Replacement::default();
    let mut gave_up = 0;
    for _ in 0..30 {
        let Some(wait) = replacement.wait(now) else {
            break;
        };
        now += wait;
        replacement.drew(now);
        now += Duration::from_millis(20);
        gave_up += usize::from(replacement.lost(now));
    }
    assert_eq!(replacement.wait(now), None);
    assert_eq!(gave_up, 1, "said once");
}

#[test]
fn a_device_that_drew_for_a_while_starts_the_count_again() {
    let mut now = Instant::now();
    let mut replacement = Replacement::default();
    replacement.lost(now);
    for _ in 0..(Replacement::MAX_FAILURES - 1) {
        replacement.failed(now);
    }
    // One failure short of giving up, a new device works for a while.
    now += replacement.wait(now).unwrap();
    replacement.drew(now);
    now += Replacement::STABLE_AFTER;
    assert!(!replacement.lost(now));
    assert_eq!(replacement.wait(now), Some(Duration::ZERO));
    replacement.failed(now);
    assert_eq!(replacement.wait(now), Some(Duration::from_millis(500)));
}
