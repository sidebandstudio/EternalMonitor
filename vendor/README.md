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
  each paint, which makes new ones when `winit::Replacement` says: at once,
  then after 0.5 s, doubling up to 30 s. After 10 failures in a row (devices
  that could not be made, or new ones lost again before they drew for 10 s)
  it gives up, says so once, and the window stays blank. Streaming runs on
  its own devices and is not affected. The new device's first frame uploads
  the font atlas again. Other textures are the app's to load again
  (`host/src/gui/renderer.rs` counts devices for that).
- A replacement never blocks the window's thread. The window makes the new
  surface (it belongs to the window, and Metal wants it on the main thread).
  Listing adapters and making the device, where a driver can stall, run on
  their own thread (`winit::Answer`), which the window polls each frame. One
  that hasn't answered in 10 s counts as a failed try, and no other starts
  while it is out; a late device is still taken. The thread is named
  `winit::REPLACEMENT_THREAD`, so the host's panic hook treats a panic there
  as one failed try: it doesn't turn the virtual display off as it does for
  other background threads. Configuring the new surface,
  which makes the swapchain, stays on the window's thread, because DXGI sends
  the window messages while it does that.
- `configure_surface` replaces the device, instead of panicking, when the
  adapter supports no surface any more.
- `Renderer::upload_failed`, `winit::SKIPPED_FRAMES` and `winit::DEVICES_LOST`
  let the app see skipped frames and dropped devices. The host counts them, so
  a backend that only skips frames is never recorded as one that drew, and a
  device lost before the window worked is recorded as its backend's failure
  even if no replacement is ever made.
- A D3D12 device is only replaced once nothing holds the lost one, so the host
  drops the copy eframe keeps in `Frame` (`release_render_state`).

Drop this copy when an egui-wgpu release survives device loss, or carry the
patch to the version eframe moves to. When eframe moves on (0.37 uses
egui-wgpu 0.37) this patch stops applying, because `[patch.crates-io]` only
replaces 0.36.2: Cargo warns that the patch was not used. Then
`host/tests/window_device_loss.rs` is the guard. It fails to build without
the patch's `upload_failed` and `Replacement`, and on an unpatched release it
fails with the old panic.
