# Vendored crates

Cargo builds these in place of the crates.io release (`[patch.crates-io]` in
the workspace `Cargo.toml`). Each keeps its upstream license files.

## egui-wgpu 0.36.2

Copied from the crates.io release of egui-wgpu 0.36.2
(https://github.com/emilk/egui, MIT OR Apache-2.0), without its `Cargo.lock`
and `Cargo.toml.orig`. Every change is marked `EternalMonitor patch`.

Why: the PC window must survive the loss of its graphics device, as in a
driver reset or a device removed while another program uses the GPU. The
release panics on the next frame ("Failed to create staging buffer for index
data"), because wgpu gives no staging buffer once the device is lost, and the
panic ends the app and the stream with it. 0.37.0 still does.

The patch:
- `Renderer::update_buffers` skips a frame whose staging buffer can't be made,
  and `Renderer::render` then draws nothing.
- The winit `Painter` watches each device's lost callback. A wgpu error
  outside an error scope counts as a loss too, where wgpu's default handler
  panics: a window whose device or surface fails is drawn on a new one.
- When the device is lost, or a frame's upload fails, the `Painter` drops the
  device, its surfaces and egui's renderer. eframe calls `set_window` before
  each paint, which makes new ones; until that works the window skips frames
  and tries again every 500 ms. The new device's first frame uploads the font
  atlas again. Other textures are the app's to load again
  (`host/src/gui/renderer.rs` counts devices for that).
- A D3D12 device is only replaced once nothing holds the lost one, so the host
  drops the copy eframe keeps in `Frame` (`release_render_state`).

Drop this copy when an egui-wgpu release survives device loss, or carry the
patch to the version eframe moves to.
