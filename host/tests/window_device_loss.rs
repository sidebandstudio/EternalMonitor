//! The window survives the loss of its graphics device, as in a driver
//! reset. egui-wgpu 0.36 panicked on the next frame ("Failed to create
//! staging buffer for index data"), which ended the app and the stream; the
//! patched copy in `vendor/egui-wgpu` skips the frame and replaces the device.

use eframe::egui;
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

    // wgpu now refuses every staging buffer, as after a driver reset.
    device.destroy();
    let (primitives, mut textures) = pass(&ctx);
    textures.clear();
    let mut encoder = device.create_command_encoder(&Default::default());
    let callbacks = renderer.update_buffers(&device, &queue, &mut encoder, &primitives, &screen);
    assert!(callbacks.is_empty());
}
