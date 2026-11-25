//! Windows capture backend: DXGI Desktop Duplication, cursor compositing,
//! and display-output enumeration. Everything Win32 in the capture stage
//! lives here; the portable loop policy (pacing, target resolution, virtual
//! display reconciliation) stays in the parent module.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};
use windows::core::Interface;

use super::listing::Judged;
use super::recovery::{
    note_capture_resumed, note_gpu_outcome, CaptureBlock, CaptureUnavailable, LossRecovery,
    LossStep, OpenStep, Reopen, LOSS_GIVE_UP,
};
use super::rotation::Rotation;
use super::{
    acquired, client_connected, composite_pointer, frame_budget_for, frame_buffer, heartbeat,
    idle_heartbeat, keepalive_allowed, keepalive_due, nap_unless, pointer_apart, pointer_place,
    read_back_image, reconcile_virtual_display, recovery_poll, redraw_cached, resolve_target,
    Acquired, FrameSlot, FrameUse, IdleGate, OutputInfo, Picture, PointerPlace, RawFrame, Redraw,
    StartPicture, ACQUIRE_TIMEOUT_MS, FPS_WINDOW, FRAME_PADDING, PICTURE_HOLD,
};
use crate::control::{CaptureTarget, SharedControl};
use crate::gpu_preference::Outcome;
use crate::stall::{Call, Stall};
use crate::stats::PIPELINE_STATS;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ADAPTER_DESC1, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_NOT_FOUND, DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO,
};

/// Parse a null-terminated UTF-16 fixed array (DXGI `DeviceName` / `Description`) to a String.
fn utf16_to_string(buf: &[u16]) -> String {
    String::from_utf16_lossy(
        &buf.iter()
            .copied()
            .take_while(|&c| c != 0)
            .collect::<Vec<_>>(),
    )
}

/// Every desktop-attached display output across all adapters, from a fresh
/// factory. Read-only. Never panics — on any DXGI error it returns whatever
/// was discovered so far (possibly empty). Through [`super::listing`].
pub(super) fn outputs(judged: Judged) -> Vec<OutputInfo> {
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(error) => {
            warn!(error = %error, "CreateDXGIFactory1 failed during output enumeration");
            return Vec::new();
        }
    };
    list_outputs(&factory, judged)
        .0
        .into_iter()
        .map(|listed| listed.info)
        .collect()
}

/// A desktop-attached output with the adapter and output objects it was
/// listed from. Capture opens these objects rather than looking the
/// output up again by its ordinals.
pub(super) struct Listed {
    pub(super) info: OutputInfo,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
}

/// Every desktop-attached output, and how many adapters were listed.
/// Through [`super::listing`], which judges what it lists.
pub(super) fn list_outputs(factory: &IDXGIFactory1, _: Judged) -> (Vec<Listed>, u32) {
    let mut outputs = Vec::new();

    let mut adapter_index = 0u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(a) => a,
            Err(_) => break,
        };
        let (adapter_name, adapter_luid) = match unsafe { adapter.GetDesc1() } {
            Ok(desc) => (utf16_to_string(&desc.Description), adapter_id(desc)),
            Err(_) => (String::new(), (0, 0)),
        };

        let mut output_index = 0u32;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(o) => o,
                Err(_) => break,
            };
            if let Ok(desc) = unsafe { output.GetDesc() } {
                if desc.AttachedToDesktop.as_bool() {
                    let r = desc.DesktopCoordinates;
                    outputs.push(Listed {
                        info: OutputInfo {
                            adapter_index,
                            output_index,
                            device_name: utf16_to_string(&desc.DeviceName),
                            width: (r.right - r.left).max(0) as u32,
                            height: (r.bottom - r.top).max(0) as u32,
                            left: r.left,
                            top: r.top,
                            is_primary: r.left == 0 && r.top == 0,
                            adapter_name: adapter_name.clone(),
                            adapter_id: adapter_luid,
                        },
                        adapter: adapter.clone(),
                        output,
                    });
                }
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    (outputs, adapter_index)
}

/// Enumerate adapters, select the output to duplicate, create duplication,
/// and pull frames at the target fps. Runs on a blocking thread — never call from async context.
pub fn run_capture_loop(
    slot: &FrameSlot,
    shared: SharedControl,
    generation: u64,
    frame_number: &mut u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // The capture adapter FOLLOWS the chosen output (which may differ from the
    // encoder-detection adapter on a multi-GPU system); the encoder stays vendor-based
    // and independent.
    let requested_target = shared.capture_target.lock().clone();
    // Turn the virtual display on/off to match the request before enumerating, so the
    // managed virtual output only exists while we're capturing it (and only while an iPad
    // is connected).
    let Some(target) = reconcile_virtual_display(&requested_target, &shared, generation) else {
        return Ok(());
    };
    let second_screen = matches!(&target, CaptureTarget::Output(name)
        if shared.vdd_output.lock().as_deref() == Some(name.as_str()));
    // DXGI factories snapshot adapters at creation. Enable the driver first,
    // then enumerate and open the chosen output from the same fresh factory.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let (listed, adapters) = super::listing::listed(&shared, &factory);
    let outputs: Vec<OutputInfo> = listed.iter().map(|listed| listed.info.clone()).collect();
    for o in &outputs {
        info!(
            device = %o.device_name,
            adapter = %o.adapter_name,
            width = o.width,
            height = o.height,
            primary = o.is_primary,
            "Discovered display output"
        );
    }

    // Capture retries every few seconds while no output exists; say so once.
    static NO_OUTPUTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !outputs.is_empty() && NO_OUTPUTS.swap(false, Ordering::Relaxed) {
        info!(outputs = outputs.len(), "A display output is back");
    }
    // No output listed is a display change the stage waits out, not a failure.
    let Some(index) = resolve_target(&outputs, &target) else {
        if !NO_OUTPUTS.swap(true, Ordering::Relaxed) {
            info!(
                adapters,
                "No display is attached to the desktop, as while a laptop's lid is closed or the screens are off"
            );
        }
        return Err(Box::new(no_output_listed(adapters)));
    };
    let Listed {
        info: chosen,
        adapter,
        output,
    } = listed
        .into_iter()
        .nth(index)
        .expect("resolved from this list");
    let adapter_name = chosen.adapter_name.clone();
    let capture_label = format!(
        "{} ({}x{})",
        chosen.device_name, chosen.width, chosen.height
    );
    info!(
        device = %chosen.device_name,
        adapter = %adapter_name,
        width = chosen.width,
        height = chosen.height,
        "Capture: selected display output"
    );

    info!(adapter = %adapter_name, "Capture using adapter");
    // The mirror's card, by LUID, for the graphics card checks.
    let mirror_card = unsafe { adapter.GetDesc1() }
        .ok()
        .filter(|_| !second_screen)
        .map(|desc| crate::gpu_preference::Adapter {
            id: adapter_id(desc),
            name: adapter_name.clone(),
        });
    {
        let mut stats = PIPELINE_STATS.lock();
        stats.set_gpu_name(adapter_name.clone());
        stats.set_capture_display(capture_label);
    }

    // --- Create D3D11 device on the selected adapter ---
    let device = {
        let _call = super::second_screen::driver_call(&shared, generation);
        create_device(adapter, super::hold_watchdog(&shared, generation))??
    };
    info!("D3D11 device created");

    // --- Get immediate device context ---
    let device_context: ID3D11DeviceContext = unsafe { device.GetImmediateContext()? };

    // --- Duplicate the output ---
    let output1: IDXGIOutput1 = output.cast()?;
    // `None` only between releasing a lost duplication and creating its
    // replacement inside the ACCESS_LOST handler.
    let mut duplication: Option<IDXGIOutputDuplication> = Some({
        let _call = super::second_screen::driver_call(&shared, generation);
        duplicate_output(&output1, &device, super::hold_watchdog(&shared, generation))?
            .inspect_err(|error| {
                if error.code() == DXGI_ERROR_UNSUPPORTED {
                    check_gpu(&output1, &shared, mirror_card.as_ref().map(|card| card.id));
                }
            })
            .map_err(blocked_or_error)?
    });
    info!("Desktop duplication active");
    note_gpu_outcome(&shared, Outcome::Worked);
    if !second_screen {
        super::note_mirror_works(&shared, mirror_card.as_ref());
    }

    // --- Query desktop dimensions from duplication ---
    // These are `mut` because a mid-stream resolution change surfaces as ACCESS_LOST; the error
    // handler re-reads the geometry and rebuilds the staging texture / CPU buffer below.
    let dupl_desc = unsafe { duplication.as_ref().expect("just created").GetDesc() };
    // `ModeDesc` is the desktop's size, upright, as measured on an RTX
    // 5080. A screen turned in Windows' display settings is duplicated as
    // the display scans it out, its sides swapped for a quarter turn (see
    // `Rotation`): the staging texture starts at that size. CopyResource
    // copies nothing between textures of different sizes, so an image
    // acquired at another size rebuilds it at the image's. Frames are read
    // back upright, at the desktop's size the pointer and input use.
    let mut frame_width = dupl_desc.ModeDesc.Width;
    let mut frame_height = dupl_desc.ModeDesc.Height;
    let mut rotation = Rotation::from_dxgi(dupl_desc.Rotation.0);
    // The mode the duplication last reported, for the ACCESS_LOST handler
    // to tell a change from it: the frames' size may be the image's instead.
    let mut mode = (frame_width, frame_height);
    let (mut tex_width, mut tex_height) = rotation.duplicated((frame_width, frame_height));
    info!(
        width = frame_width,
        height = frame_height,
        ?rotation,
        "Desktop duplication dimensions"
    );
    if second_screen {
        // The size the second screen is at, for the app; a resize updates it.
        super::note_vdd_size(&shared, &chosen.device_name, (frame_width, frame_height));
    }
    if frame_width == 0 || frame_height == 0 {
        return Err(format!(
            "Desktop duplication reported invalid dimensions {frame_width}x{frame_height}"
        )
        .into());
    }

    // Publish the output's desktop-space rect for the input relay: the iPad's
    // normalized touches map onto exactly this rectangle. DesktopCoordinates
    // (not ModeDesc) is authoritative — under DPI virtualization the two can
    // differ, and injected coordinates live in desktop space. On a turned
    // screen both are the upright desktop's size (measured on an RTX 5080).
    publish_capture_geometry(&output1, &shared);

    // --- Create staging texture for CPU readback (reused every frame) ---
    let mut staging_texture: ID3D11Texture2D = unsafe {
        let mut tex = None;
        device.CreateTexture2D(
            &make_staging_desc(tex_width, tex_height),
            None,
            Some(&mut tex),
        )?;
        tex.expect("CreateTexture2D returned None")
    };
    info!("Staging texture created for CPU readback");

    let mut row_bytes = (frame_width * 4) as usize;
    let mut frame_bytes = row_bytes * frame_height as usize;
    // Buffer recycling (see synthetic.rs): steady state re-uses one buffer.
    let mut spare: Option<Arc<Vec<u8>>> = None;
    // The last published frame, shared for keepalive resends without a copy.
    // Starts as a black frame so a static screen still yields a startup IDR.
    let mut last_frame: Arc<Vec<u8>> = Arc::new(frame_buffer(frame_bytes));

    // --- Capture loop state ---
    let mut frame_timestamps: VecDeque<Instant> = VecDeque::with_capacity(FPS_WINDOW + 1);
    let mut dirty_rect_buf: Vec<RECT> = Vec::with_capacity(64);
    // Time of the last frame handed downstream — drives the idle keepalive (T1d).
    let mut last_send = Instant::now();

    // Desktop Duplication never bakes the mouse cursor into the desktop image — it sends
    // the position every frame and the shape only when it changes. Cache the shape and
    // composite it into each frame so the cursor is visible on the iPad.
    let mut cursor = CursorState::default();
    let mut cursor_visible = false;
    let mut cursor_x: i32 = 0;
    let mut cursor_y: i32 = 0;
    let mut recovery = LossRecovery::default();
    let mut resumed = false;
    let mut idle = IdleGate::new();
    let mut picture = Picture::default();
    let mut start_picture = StartPicture::new(Instant::now());
    // ACCESS_LOST from a call after the acquire (ReleaseFrame, the staging
    // Map). It is handled as if the next acquire had returned it: before,
    // it ended the capture loop and restarted the pipeline.
    let mut lost_after_acquire: Option<windows::core::Error> = None;
    // Whether `last_frame` has the pointer drawn in: it has unless the
    // client draws the pointer itself (`SharedControl::client_draws_pointer`).
    let mut last_frame_has_pointer = true;
    // Where Windows puts the pointer (`pointer_place`): into the picture on
    // the second screen without the driver's hardware cursor, or for an
    // image too large for it, and then nothing else draws one. Where the
    // running driver can't be told, as 0.3.4: no pointer messages.
    let driver = if second_screen {
        super::running_driver_cursor()
    } else {
        crate::vdd_settings::DriverCursor::Unknown
    };
    shared
        .pointer
        .set_withheld(second_screen && driver == crate::vdd_settings::DriverCursor::Unknown);
    let place_of = |cursor: &CursorState| pointer_place(second_screen, driver, cursor.size());
    let mut place;
    {
        // The feed outlives this capture: a position from the output
        // captured before (the mirror a second screen starts from) would be
        // drawn on this one until Desktop Duplication reports its own,
        // which it does once the pointer is on this output.
        shared.pointer.set_position(super::pointer::Position {
            visible: false,
            x: 0,
            y: 0,
            host_time_us: crate::clock::host_now_us(),
        });
    }

    loop {
        if !shared.running.load(Ordering::SeqCst)
            || !crate::supervisor::generation_is_current(generation)
        {
            info!("Capture loop stopping on running=false");
            break;
        }
        heartbeat(&shared.hb_capture_loop_ms);
        if picture.held_too_long(Instant::now(), second_screen) {
            info!(
                device = %chosen.device_name,
                held_s = PICTURE_HOLD.as_secs(),
                "The second screen showed no new image since its duplication came back — opening it again"
            );
            return Err(Box::new(Reopen {
                detail: format!(
                    "no new image from {} since its duplication came back",
                    chosen.device_name
                ),
            }));
        }

        let wait = idle.wait_before_acquire(client_connected(&shared), Instant::now());
        if !wait.is_zero() {
            // Nobody is watching. Waiting (instead of acquiring and dropping
            // frames) keeps desktop changes pending for the next acquire.
            let deadline = Instant::now() + wait;
            while Instant::now() < deadline
                && !client_connected(&shared)
                && shared.running.load(Ordering::SeqCst)
                && crate::supervisor::generation_is_current(generation)
            {
                idle_heartbeat(&shared, crate::clock::host_now_us() / 1000);
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50)),
                );
            }
            continue;
        }

        // An iPad that draws the pointer gets a picture without it; any
        // other client, or none, one with it. The staging texture still
        // holds the last desktop image, so the cached frame is redrawn from
        // it when the connected client's answer differs from the cache's.
        place = place_of(&cursor);
        let draws_pointer = pointer_apart(shared.client_draws_pointer(), place);
        if picture.seen_image && last_frame_has_pointer == draws_pointer {
            let mut pixel_buf = frame_buffer(frame_bytes);
            // A lost duplication goes to the handler below, which keeps the
            // cache; the rebuild is tried again after it.
            let copied = redraw_cached(
                lost_after_acquire.is_some(),
                || {
                    copy_staging(
                        &device_context,
                        &staging_texture,
                        (tex_width, tex_height),
                        rotation,
                        row_bytes,
                        &mut pixel_buf,
                    )
                },
                |error| error.code() == DXGI_ERROR_ACCESS_LOST,
            )?;
            match copied {
                Redraw::Later => {}
                Redraw::Lost(error) => lost_after_acquire = Some(error),
                Redraw::Copied => {
                    if composite_pointer(draws_pointer, place, cursor_visible, cursor.has_shape) {
                        draw_cursor(
                            &mut pixel_buf,
                            frame_width,
                            frame_height,
                            row_bytes,
                            &cursor,
                            cursor_x,
                            cursor_y,
                        );
                    }
                    spare = None;
                    last_frame = Arc::new(pixel_buf);
                    last_frame_has_pointer = !draws_pointer;
                    if client_connected(&shared) {
                        resend_last_frame(
                            slot,
                            &shared,
                            frame_number,
                            &mut last_send,
                            &last_frame,
                            (frame_width, frame_height),
                        );
                    }
                }
            }
        }

        if start_picture.due(picture.real(), second_screen, Instant::now()) {
            let pixels = duplication
                .as_ref()
                .and_then(|_| read_screen_gdi(&output1, (frame_width, frame_height)));
            if let Some(pixels) = pixels {
                info!(
                    device = %chosen.device_name,
                    "Desktop duplication gave no image yet — starting from the screen's picture"
                );
                spare = None;
                last_frame = Arc::new(pixels);
                last_frame_has_pointer = false;
                picture.cached();
                let now = Instant::now();
                if idle.on_frame(client_connected(&shared), now) == FrameUse::Publish {
                    *frame_number += 1;
                    PIPELINE_STATS
                        .lock()
                        .record_capture(frame_width, frame_height);
                    heartbeat(&shared.hb_capture_frame_ms);
                    slot.publish(RawFrame {
                        frame_number: *frame_number,
                        timestamp: now,
                        data: Arc::clone(&last_frame),
                        width: frame_width,
                        height: frame_height,
                        cached: true,
                    });
                    last_send = now;
                }
            }
        }

        let frame_start = Instant::now();
        let frame_budget = frame_budget_for(shared.effective_fps());
        // Half the frame budget, clamped: at 60 fps this is the classic 8 ms;
        // high-rate modes (ETERNAL_FPS=120) must not block a whole 16 ms in
        // AcquireNextFrame, and below 4 ms the wakeups outrun the compositor.
        let acquire_timeout_ms =
            ((frame_budget.as_millis() as u32) / 2).clamp(4, ACQUIRE_TIMEOUT_MS);

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut desktop_resource: Option<IDXGIResource> = None;

        let acquire_start = Instant::now();
        let dupl = duplication
            .as_ref()
            .expect("the ACCESS_LOST handler re-creates the duplication before continuing");
        let hr = match lost_after_acquire.take() {
            Some(lost) => Err(lost),
            None => unsafe {
                dupl.AcquireNextFrame(acquire_timeout_ms, &mut frame_info, &mut desktop_resource)
            },
        };
        let acquire_us = acquire_start.elapsed().as_micros();
        if hr.is_ok()
            || hr
                .as_ref()
                .is_err_and(|e| e.code() == DXGI_ERROR_WAIT_TIMEOUT)
        {
            // A frame or an idle timeout proves the duplication works.
            if let Some((losses, took)) = recovery.healthy(acquire_start) {
                info!(
                    losses,
                    took_ms = took.as_millis() as u64,
                    "Desktop duplication recovered"
                );
                resumed = false;
            }
            if !resumed {
                note_capture_resumed(&shared);
                resumed = true;
            }
        }

        match hr {
            Ok(()) => {
                *frame_number += 1;
                if !super::second_screen::PROOF_NEEDS_IMAGE {
                    super::second_screen::note_image(&shared, generation);
                }

                // --- Track cursor shape/position (must read shape before ReleaseFrame) ---
                // The shape is always read: its size decides whether Windows
                // draws it into the picture.
                let new_shape = frame_info.PointerShapeBufferSize > 0
                    && cursor.update_shape(dupl, frame_info.PointerShapeBufferSize);
                place = place_of(&cursor);
                if new_shape && place == PointerPlace::Apart {
                    if let Some(image) = super::pointer::convert_shape(
                        cursor.shape_type,
                        cursor.width,
                        cursor.height,
                        cursor.pitch,
                        cursor.hot,
                        &cursor.shape,
                    ) {
                        shared.pointer.set_shape(image);
                    }
                }
                if frame_info.LastMouseUpdateTime != 0 || new_shape {
                    if frame_info.LastMouseUpdateTime != 0 {
                        cursor_visible = frame_info.PointerPosition.Visible.as_bool();
                        cursor_x = frame_info.PointerPosition.Position.x;
                        cursor_y = frame_info.PointerPosition.Position.y;
                    }
                    shared.pointer.set_position(super::pointer::Position {
                        // Only a pointer Windows keeps apart is shown apart.
                        visible: cursor_visible && place == PointerPlace::Apart,
                        x: cursor_x,
                        y: cursor_y,
                        host_time_us: crate::clock::host_now_us(),
                    });
                }

                // --- Get dirty rect count ---
                let dirty_count = get_dirty_rect_count(dupl, &frame_info, &mut dirty_rect_buf);

                // --- Rolling FPS (60-frame window) ---
                let now = Instant::now();
                frame_timestamps.push_back(now);
                if frame_timestamps.len() > FPS_WINDOW {
                    frame_timestamps.pop_front();
                }
                let rolling_fps = if frame_timestamps.len() >= 2 {
                    let window = frame_timestamps
                        .back()
                        .unwrap()
                        .duration_since(*frame_timestamps.front().unwrap());
                    let secs = window.as_secs_f64();
                    if secs > 0.0 {
                        (frame_timestamps.len() - 1) as f64 / secs
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                debug!(
                    frame = *frame_number,
                    dirty_rects = dirty_count,
                    last_present = frame_info.LastPresentTime,
                    accumulated = frame_info.AccumulatedFrames,
                    acquire_us = acquire_us,
                    fps = format!("{rolling_fps:.1}"),
                    "Frame acquired"
                );

                // --- Copy desktop texture to staging and read pixels into a
                // recycled buffer (the readback is the ONLY full-frame copy) ---
                // A pointer-only update needs no new frame when the iPad
                // draws the pointer: the transport sends the position.
                match acquired(
                    frame_info.LastPresentTime,
                    picture.seen_image,
                    draws_pointer,
                ) {
                    Acquired::ReadBack => {}
                    skip => {
                        if let Err(error) = unsafe { dupl.ReleaseFrame() } {
                            lost_after_acquire = Some(access_lost_or(error)?);
                            continue;
                        }
                        if skip == Acquired::PointerOnly
                            && client_connected(&shared)
                            && keepalive_due(
                                last_send.elapsed(),
                                shared.force_next_idr.load(Ordering::SeqCst),
                                picture.real(),
                            )
                            && keepalive_allowed(picture.real(), second_screen)
                        {
                            resend_last_frame(
                                slot,
                                &shared,
                                frame_number,
                                &mut last_send,
                                &last_frame,
                                (frame_width, frame_height),
                            );
                        }
                        continue;
                    }
                }
                let desktop_texture = desktop_resource
                    .as_ref()
                    .map(|resource| resource.cast::<ID3D11Texture2D>());
                // The staging texture's size is inferred from the mode and
                // the rotation; the acquired image's own decides.
                if let Some(Ok(texture)) = &desktop_texture {
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    unsafe { texture.GetDesc(&mut desc) };
                    let image = (desc.Width, desc.Height);
                    if image != (tex_width, tex_height) && image.0 != 0 && image.1 != 0 {
                        let upright = rotation.upright(image);
                        warn!(
                            staging = %format_args!("{tex_width}x{tex_height}"),
                            image = %format_args!("{}x{}", image.0, image.1),
                            upright = %format_args!("{}x{}", upright.0, upright.1),
                            ?rotation,
                            "The duplicated image is not the size its mode gave — rebuilding staging resources at its size"
                        );
                        (tex_width, tex_height) = image;
                        (frame_width, frame_height) = upright;
                        row_bytes = (frame_width * 4) as usize;
                        frame_bytes = row_bytes * frame_height as usize;
                        spare = None;
                        last_frame = Arc::new(frame_buffer(frame_bytes));
                        picture.resized();
                        start_picture.duplicated(Instant::now());
                        if second_screen {
                            super::note_vdd_size(&shared, &chosen.device_name, upright);
                        }
                        staging_texture = unsafe {
                            let mut tex = None;
                            device.CreateTexture2D(
                                &make_staging_desc(tex_width, tex_height),
                                None,
                                Some(&mut tex),
                            )?;
                            tex.expect("CreateTexture2D returned None")
                        };
                    }
                }
                let seen_image = &mut picture.seen_image;
                let frame_data = read_back_image(&shared, generation, seen_image, || {
                    windows::core::Result::Ok(if let Some(desktop_texture) = desktop_texture {
                        match desktop_texture {
                            Ok(desktop_texture) => {
                                unsafe {
                                    device_context.CopyResource(&staging_texture, &desktop_texture);
                                }

                                // Release acquired frame — staging texture has its own copy
                                unsafe {
                                    dupl.ReleaseFrame()?;
                                }

                                let mut pixel_buf = match spare
                                    .take()
                                    .and_then(|arc| Arc::try_unwrap(arc).ok())
                                {
                                    Some(buf) if buf.len() == frame_bytes + FRAME_PADDING => buf,
                                    _ => frame_buffer(frame_bytes),
                                };

                                copy_staging(
                                    &device_context,
                                    &staging_texture,
                                    (tex_width, tex_height),
                                    rotation,
                                    row_bytes,
                                    &mut pixel_buf,
                                )?;

                                // Composite the mouse cursor into the BGRA frame.
                                if composite_pointer(
                                    draws_pointer,
                                    place,
                                    cursor_visible,
                                    cursor.has_shape,
                                ) {
                                    draw_cursor(
                                        &mut pixel_buf,
                                        frame_width,
                                        frame_height,
                                        row_bytes,
                                        &cursor,
                                        cursor_x,
                                        cursor_y,
                                    );
                                }

                                Some(Arc::new(pixel_buf))
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to cast IDXGIResource to ID3D11Texture2D");
                                unsafe {
                                    dupl.ReleaseFrame()?;
                                }
                                None
                            }
                        }
                    } else {
                        unsafe {
                            dupl.ReleaseFrame()?;
                        }
                        None
                    })
                });
                let frame_data = match frame_data {
                    Ok(frame_data) => frame_data,
                    Err(error) => {
                        lost_after_acquire = Some(access_lost_or(error)?);
                        continue;
                    }
                };

                if let Some(data) = frame_data {
                    picture.cached();
                    if idle.on_frame(client_connected(&shared), now) == FrameUse::Publish {
                        PIPELINE_STATS
                            .lock()
                            .record_capture(frame_width, frame_height);
                        heartbeat(&shared.hb_capture_frame_ms);

                        slot.publish(RawFrame {
                            frame_number: *frame_number,
                            timestamp: now,
                            data: Arc::clone(&data),
                            width: frame_width,
                            height: frame_height,
                            cached: false,
                        });
                        last_send = now;
                    } else {
                        idle_heartbeat(&shared, crate::clock::host_now_us() / 1000);
                    }
                    // The displaced spare (if any) plus last_frame keep at most
                    // two buffers alive; steady state recycles them.
                    spare = Some(Arc::clone(&last_frame));
                    last_frame = data;
                    last_frame_has_pointer = !draws_pointer;
                }
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                // Desktop unchanged — not an error.
                if !shared.running.load(Ordering::SeqCst)
                    || !crate::supervisor::generation_is_current(generation)
                {
                    info!("Capture loop stopping after timeout on running=false");
                    break;
                }
                // Idle keepalive: a static or blank (freshly-enabled) display produces no new
                // frames, so the encoder would never see a frame to turn into the startup IDR and
                // the iPad would sit on a black screen until it times out. While a client is
                // connected, resend the last captured image so the encoder emits the pending IDR
                // and the stream stays alive. pixel_buf holds the last composited frame (zeroed
                // black before the first real frame, which is still a valid keyframe). A
                // pending keyframe request is answered without waiting for the keepalive.
                if client_connected(&shared)
                    && keepalive_due(
                        last_send.elapsed(),
                        shared.force_next_idr.load(Ordering::SeqCst),
                        picture.real(),
                    )
                    && keepalive_allowed(picture.real(), second_screen)
                {
                    resend_last_frame(
                        slot,
                        &shared,
                        frame_number,
                        &mut last_send,
                        &last_frame,
                        (frame_width, frame_height),
                    );
                } else if !client_connected(&shared)
                    && last_send.elapsed() >= Duration::from_secs(2)
                {
                    // Idle throttle: nobody is watching and the desktop is
                    // static — stop spinning at capture rate. 4 Hz polling,
                    // whose nap ends within 50 ms of a client connecting
                    // (registration also restarts the pipeline for
                    // virtual-display targets). The 250 ms naps stay far
                    // under the supervisor's 3 s loop-heartbeat wedge
                    // threshold.
                    nap_unless(
                        Duration::from_millis(250),
                        || {
                            client_connected(&shared)
                                || !super::capture_is_current(&shared, generation)
                        },
                        || idle_heartbeat(&shared, crate::clock::host_now_us() / 1000),
                        std::thread::sleep,
                    );
                }
                continue;
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                // Normal during UAC prompts, the lock screen, mode changes and
                // full-screen transitions. A duplication re-created too early is
                // lost again at once, so the retries back off instead of spinning.
                // Routine while it is back within a second; see `LOSS_QUIET`.
                if !recovery.in_episode() {
                    info!("Desktop duplication access lost — reinitializing");
                }
                let allowed = keepalive_allowed(picture.real(), second_screen);
                let dims = (frame_width, frame_height);
                let call = super::second_screen::driver_call(&shared, generation);
                let moved = target_moved(&shared, &output1, &chosen, &target, false, || {
                    if crate::supervisor::generation_is_current(generation) {
                        keep_picture_alive(
                            slot,
                            &shared,
                            frame_number,
                            &mut last_send,
                            &last_frame,
                            dims,
                            allowed,
                        );
                    }
                });
                drop(call);
                if moved {
                    return Err(moved_away(&chosen));
                }
                // A second screen that shows no picture before this is
                // re-created failed for a passing reason.
                super::second_screen::note_interruption(&shared, generation);
                // Release the lost duplication before asking for a new one:
                // Windows refuses a second duplication of the same output.
                duplication = None;
                let mut last_code = e.code();
                // Set while the last try was a driver call that did not
                // return in time. The last try decides, as for errors.
                let mut last_stall: Option<Stall> = None;
                while duplication.is_none() {
                    let delay = match recovery.lost(Instant::now()) {
                        LossStep::Retry(delay) => delay,
                        LossStep::GiveUp => {
                            // The stage waits for the driver; it passes.
                            if let Some(stall) = last_stall {
                                return Err(Box::new(stall));
                            }
                            return Err(Box::new(CaptureUnavailable {
                                block: CaptureBlock::from_hresult(last_code.0)
                                    .unwrap_or(CaptureBlock::DisplayChanging),
                                detail: format!(
                                    "desktop duplication not reacquired within {}s (last error {:#010x})",
                                    LOSS_GIVE_UP.as_secs(),
                                    last_code.0 as u32
                                ),
                            }));
                        }
                    };
                    // Keep the iPad's session and last picture alive meanwhile.
                    let deadline = Instant::now() + delay;
                    loop {
                        if !shared.running.load(Ordering::SeqCst)
                            || !crate::supervisor::generation_is_current(generation)
                        {
                            return Ok(());
                        }
                        warn_if_overdue(&mut recovery);
                        keep_picture_alive(
                            slot,
                            &shared,
                            frame_number,
                            &mut last_send,
                            &last_frame,
                            (frame_width, frame_height),
                            keepalive_allowed(picture.real(), second_screen),
                        );
                        let now = Instant::now();
                        if now >= deadline {
                            break;
                        }
                        std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
                    }
                    // The picture stays alive while a slow driver answers.
                    let poll = || {
                        if !crate::supervisor::generation_is_current(generation) {
                            return;
                        }
                        warn_if_overdue(&mut recovery);
                        keep_picture_alive(
                            slot,
                            &shared,
                            frame_number,
                            &mut last_send,
                            &last_frame,
                            (frame_width, frame_height),
                            keepalive_allowed(picture.real(), second_screen),
                        );
                    };
                    let call = super::second_screen::driver_call(&shared, generation);
                    let retried = duplicate_output(&output1, &device, poll);
                    drop(call);
                    let retried = match retried {
                        Ok(retried) => {
                            last_stall = None;
                            retried
                        }
                        // Logged; the ladder keeps trying until it gives up.
                        // A stuck driver passes, like a hung device.
                        Err(stall) => {
                            super::second_screen::note_passing_retry_error(&shared, generation);
                            last_stall = Some(stall);
                            continue;
                        }
                    };
                    match retried {
                        Ok(d) => {
                            duplication = Some(d);
                            picture.recreated(Instant::now());
                            start_picture.duplicated(Instant::now());
                            super::second_screen::note_recovery(
                                &shared,
                                generation,
                                Instant::now(),
                            );
                        }
                        Err(retry_error) => {
                            debug!(error = %retry_error, "DuplicateOutput retry failed");
                            last_code = retry_error.code();
                            if super::recovery::passing_code(last_code.0) {
                                super::second_screen::note_passing_retry_error(&shared, generation);
                            } else {
                                super::second_screen::note_persistent_error(&shared, generation);
                            }
                            // Tell the GUI why while the retries continue.
                            if let Some(block) = CaptureBlock::from_hresult(last_code.0) {
                                super::recovery::note_capture_blocked(&shared, block);
                            }
                            // The output may be gone: look at the whole layout.
                            let allowed = keepalive_allowed(picture.real(), second_screen);
                            let dims = (frame_width, frame_height);
                            let call = super::second_screen::driver_call(&shared, generation);
                            let moved =
                                target_moved(&shared, &output1, &chosen, &target, true, || {
                                    if crate::supervisor::generation_is_current(generation) {
                                        keep_picture_alive(
                                            slot,
                                            &shared,
                                            frame_number,
                                            &mut last_send,
                                            &last_frame,
                                            dims,
                                            allowed,
                                        );
                                    }
                                });
                            drop(call);
                            if moved {
                                return Err(moved_away(&chosen));
                            }
                        }
                    }
                }
                // A resolution/topology change also surfaces as ACCESS_LOST. Re-read the
                // duplication geometry and rebuild the staging texture + CPU buffer if it changed,
                // otherwise CopyResource silently no-ops on a size mismatch and the iPad freezes.
                let new_desc = unsafe { duplication.as_ref().expect("re-created above").GetDesc() };
                let (new_w, new_h) = (new_desc.ModeDesc.Width, new_desc.ModeDesc.Height);
                // Turning the screen in Windows' settings may keep the mode's
                // size and change only the rotation; the frames change shape.
                let new_rotation = Rotation::from_dxgi(new_desc.Rotation.0);
                if new_w != 0 && new_h != 0 && ((new_w, new_h) != mode || new_rotation != rotation)
                {
                    info!(
                        old_width = frame_width,
                        old_height = frame_height,
                        new_width = new_w,
                        new_height = new_h,
                        old_rotation = ?rotation,
                        new_rotation = ?new_rotation,
                        "Capture resolution changed — rebuilding staging resources"
                    );
                    (frame_width, frame_height) = (new_w, new_h);
                    mode = (new_w, new_h);
                    rotation = new_rotation;
                    (tex_width, tex_height) = rotation.duplicated((frame_width, frame_height));
                    row_bytes = (frame_width * 4) as usize;
                    frame_bytes = row_bytes * frame_height as usize;
                    spare = None;
                    last_frame = Arc::new(frame_buffer(frame_bytes));
                    picture.resized();
                    start_picture.duplicated(Instant::now());
                    if second_screen {
                        // The size the app shows; nothing else is done.
                        super::note_vdd_size(&shared, &chosen.device_name, (new_w, new_h));
                    }
                    staging_texture = unsafe {
                        let mut tex = None;
                        device.CreateTexture2D(
                            &make_staging_desc(tex_width, tex_height),
                            None,
                            Some(&mut tex),
                        )?;
                        tex.expect("CreateTexture2D returned None")
                    };
                }
                // A resolution/topology change usually moves the output's
                // desktop rect too — keep the input mapping in sync.
                publish_capture_geometry(&output1, &shared);
                continue;
            }
            Err(e) => {
                error!(error = %e, "AcquireNextFrame failed");
                return Err(e.into());
            }
        }

        // Frame pacing — sleep remainder of budget
        let frame_elapsed = frame_start.elapsed();
        if frame_elapsed < frame_budget {
            std::thread::sleep(frame_budget - frame_elapsed);
        } else {
            debug!(
                over_budget_us = (frame_elapsed - frame_budget).as_micros(),
                "Frame over budget"
            );
        }
    }

    Ok(())
}

/// D3D11CreateDevice under its time limit; see [`crate::stall`].
fn create_device(
    adapter: windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
    progress: impl FnMut(),
) -> Result<windows::core::Result<ID3D11Device>, Stall> {
    crate::stall::run(
        Call::D3d11CreateDevice,
        move || {
            let mut device: Option<ID3D11Device> = None;
            unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    None,
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )?;
            }
            Ok(device.expect("D3D11CreateDevice succeeded but returned no device"))
        },
        progress,
    )
}

/// DuplicateOutput under its time limit. A duplication that comes back after
/// the limit is released at once on the call's thread: Windows allows one
/// duplication of an output, and the retries need it gone.
fn duplicate_output(
    output: &IDXGIOutput1,
    device: &ID3D11Device,
    progress: impl FnMut(),
) -> Result<windows::core::Result<IDXGIOutputDuplication>, Stall> {
    let (output, device) = (output.clone(), device.clone());
    crate::stall::run(
        Call::DuplicateOutput,
        move || unsafe { output.DuplicateOutput(&device) },
        progress,
    )
}

/// Hand the last captured image downstream again, so the encoder emits a
/// frame (and any pending IDR) while the desktop itself delivers none.
fn resend_last_frame(
    slot: &FrameSlot,
    shared: &SharedControl,
    frame_number: &mut u64,
    last_send: &mut Instant,
    last_frame: &Arc<Vec<u8>>,
    (width, height): (u32, u32),
) {
    *frame_number += 1;
    let now = Instant::now();
    PIPELINE_STATS.lock().record_capture(width, height);
    heartbeat(&shared.hb_capture_frame_ms);
    slot.publish(RawFrame {
        frame_number: *frame_number,
        timestamp: now,
        data: Arc::clone(last_frame),
        width,
        height,
        cached: true,
    });
    *last_send = now;
}

/// Windows won't duplicate `output` from the card this process runs on: see
/// [`super::note_gpu_refused`]. `mirror_card`: `output` is the mirror's
/// screen, captured from the card with this LUID.
fn check_gpu(
    output: &IDXGIOutput1,
    shared: &SharedControl,
    mirror_card: Option<crate::gpu_preference::AdapterId>,
) {
    note_gpu_outcome(shared, Outcome::StillUnsupported);
    let Ok(desc) = (unsafe { output.GetDesc() }) else {
        return;
    };
    let screen = utf16_to_string(&desc.DeviceName);
    super::note_gpu_refused(shared, &screen, crate::gpu_preference::facts(), mirror_card);
}

/// An adapter's LUID, as the graphics card checks compare it.
fn adapter_id(desc: DXGI_ADAPTER_DESC1) -> crate::gpu_preference::AdapterId {
    (desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart)
}

/// Windows states that block duplication become [`CaptureUnavailable`], which
/// the stage retries; anything else fails the stage as before.
fn blocked_or_error(error: windows::core::Error) -> Box<dyn std::error::Error + Send + Sync> {
    unavailable_or_error(CaptureBlock::from_hresult(error.code().0), error)
}

/// Enumeration listed no display attached to the desktop. The detection
/// adapter's first output, found by its ordinal from startup, was tried
/// next: it never captured anything in the field, and its error text
/// (DXGI_ERROR_NOT_FOUND, "there is no adapter with the specified ordinal")
/// hid what was wrong.
fn no_output_listed(adapters: u32) -> CaptureUnavailable {
    let (step, detail) = if adapters == 0 {
        (
            OpenStep::Adapter,
            "no graphics adapter is listed".to_string(),
        )
    } else {
        (
            OpenStep::OnlyOutput,
            format!("no display is attached to the desktop ({adapters} graphics adapters listed)"),
        )
    };
    CaptureUnavailable {
        block: CaptureBlock::opening(step, DXGI_ERROR_NOT_FOUND.0)
            .expect("DXGI_ERROR_NOT_FOUND is a blocked state"),
        detail,
    }
}

/// `error` when it is DXGI_ERROR_ACCESS_LOST, for the loop's handler;
/// anything else ends the capture loop, as before.
fn access_lost_or(
    error: windows::core::Error,
) -> Result<windows::core::Error, Box<dyn std::error::Error + Send + Sync>> {
    if error.code() == DXGI_ERROR_ACCESS_LOST {
        Ok(error)
    } else {
        Err(error.into())
    }
}

/// After ACCESS_LOST: whether the stage should open the target afresh,
/// because it now resolves to another output or adapter than `chosen` (see
/// [`super::target_moved`]). The duplicated output's own description is
/// asked first, and the outputs are listed (a new factory and every adapter)
/// only when it is off the desktop or renamed, while capture runs on the
/// primary for a missing display, or when `list` asks after a failed
/// re-creation. Both run under the stall limits while `progress` keeps the
/// picture alive; one that does not return in time counts as no move, and
/// the re-creation goes on. An output seen off the desktop or renamed, or a
/// listing without it as it was, is a detach for a second screen's proof
/// (see [`super::second_screen::note_detached`]).
fn target_moved(
    shared: &SharedControl,
    output: &IDXGIOutput1,
    chosen: &OutputInfo,
    target: &CaptureTarget,
    list: bool,
    mut progress: impl FnMut(),
) -> bool {
    let fallback = matches!(target, CaptureTarget::Output(name) if *name != chosen.device_name);
    if !list && !fallback {
        let (output, name) = (output.clone(), chosen.device_name.clone());
        let same = crate::stall::run(
            Call::ListOutputs,
            move || {
                unsafe { output.GetDesc() }.is_ok_and(|desc| {
                    desc.AttachedToDesktop.as_bool() && utf16_to_string(&desc.DeviceName) == name
                })
            },
            &mut progress,
        );
        if same.unwrap_or(true) {
            return false;
        }
        super::second_screen::note_detached(shared);
    }
    let (shared, target, chosen) = (shared.clone(), target.clone(), chosen.clone());
    let started = Instant::now();
    let moved = crate::stall::run(
        Call::ListOutputs,
        move || super::target_moved(&super::listing::outputs(&shared), &target, &chosen),
        progress,
    )
    .unwrap_or(false);
    info!(
        took_ms = started.elapsed().as_millis() as u64,
        moved, "Listed the display outputs after a lost duplication"
    );
    moved
}

/// [`Reopen`] for a target that moved off `chosen`.
fn moved_away(chosen: &OutputInfo) -> Box<dyn std::error::Error + Send + Sync> {
    info!(
        device = %chosen.device_name,
        "The display layout changed — opening the display to capture again"
    );
    Box::new(Reopen {
        detail: format!(
            "the display layout changed while capturing {}",
            chosen.device_name
        ),
    })
}

/// While the duplication is away: resends the last picture when the viewer
/// would otherwise go without one (see [`recovery_poll`]), if `allowed`.
fn keep_picture_alive(
    slot: &FrameSlot,
    shared: &SharedControl,
    frame_number: &mut u64,
    last_send: &mut Instant,
    last_frame: &Arc<Vec<u8>>,
    dims: (u32, u32),
    allowed: bool,
) {
    let now_ms = crate::clock::host_now_us() / 1000;
    if recovery_poll(shared, now_ms, last_send.elapsed()) && allowed {
        resend_last_frame(slot, shared, frame_number, last_send, last_frame, dims);
    }
}

/// Logs, once per loss, that the duplication is not back within a second.
fn warn_if_overdue(recovery: &mut LossRecovery) {
    if recovery.overdue(Instant::now()) {
        warn!("Desktop duplication not back within a second — still retrying");
    }
}

fn unavailable_or_error(
    block: Option<CaptureBlock>,
    error: windows::core::Error,
) -> Box<dyn std::error::Error + Send + Sync> {
    match block {
        Some(block) => Box::new(CaptureUnavailable {
            block,
            detail: error.to_string(),
        }),
        None => Box::new(error),
    }
}

/// Build the BGRA staging-texture descriptor for CPU readback at the given dimensions. Factored
/// out so the capture loop can recreate it after a mid-stream resolution change.
/// Push the captured output's desktop-space rect into shared state so the
/// input relay maps the client's normalized touches onto the right monitor.
fn publish_capture_geometry(output: &IDXGIOutput1, shared: &SharedControl) {
    if let Ok(desc) = unsafe { output.GetDesc() } {
        let r = desc.DesktopCoordinates;
        *shared.capture_geometry.lock() = Some(crate::input::CaptureGeometry {
            left: r.left,
            top: r.top,
            width: (r.right - r.left).max(1) as u32,
            height: (r.bottom - r.top).max(1) as u32,
        });
    }
}

/// The picture `output` shows, read through GDI, as `width`x`height` BGRA
/// rows in a [`frame_buffer`]: for a duplication that gives no image (see
/// [`StartPicture`]). The pointer is not in it. GDI reads the desktop
/// upright, as capture's frames are on a screen turned in Windows' settings
/// (see [`Rotation`]). None if the desktop rect isn't that size (the screen
/// was resized or turned since), if GDI can't read it (no interactive
/// desktop), or if it read only black, as on the secure desktop: the
/// duplication's own image is waited for then.
fn read_screen_gdi(output: &IDXGIOutput1, (width, height): (u32, u32)) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC,
        ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
    };
    let rect = unsafe { output.GetDesc() }.ok()?.DesktopCoordinates;
    if (rect.right - rect.left, rect.bottom - rect.top) != (width as i32, height as i32) {
        return None;
    }
    let row_bytes = width as usize * 4;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative: top-down rows, as capture's.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(screen);
        let mut bits = std::ptr::null_mut();
        let pixels = match CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) if !bits.is_null() => {
                let previous = SelectObject(memory, bitmap);
                let copied = BitBlt(
                    memory,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    screen,
                    rect.left,
                    rect.top,
                    SRCCOPY,
                )
                .is_ok();
                let _ = GdiFlush();
                let pixels = copied
                    .then(|| {
                        let mut buf = frame_buffer(row_bytes * height as usize);
                        std::ptr::copy_nonoverlapping(
                            bits as *const u8,
                            buf.as_mut_ptr(),
                            row_bytes * height as usize,
                        );
                        // GDI leaves the alpha byte 0; Desktop Duplication's is opaque.
                        let (pixels, _) = buf[..row_bytes * height as usize].as_chunks_mut::<4>();
                        let mut lit = false;
                        for pixel in pixels {
                            lit |= pixel[..3] != [0, 0, 0];
                            pixel[3] = 255;
                        }
                        lit.then_some(buf)
                    })
                    .flatten();
                SelectObject(memory, previous);
                let _ = DeleteObject(bitmap);
                pixels
            }
            Ok(bitmap) => {
                let _ = DeleteObject(bitmap);
                None
            }
            Err(_) => None,
        };
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        pixels
    }
}

/// Reads the staging texture's image, `size` as duplicated, into `buf`
/// upright ([`super::rotation::copy_upright`]): row by row when it is
/// upright already, as before (the texture's pitch may differ from
/// `row_bytes`, the upright rows').
fn copy_staging(
    device_context: &ID3D11DeviceContext,
    staging_texture: &ID3D11Texture2D,
    size: (u32, u32),
    rotation: Rotation,
    row_bytes: usize,
    buf: &mut [u8],
) -> windows::core::Result<()> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        device_context.Map(staging_texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
    }
    let src_pitch = mapped.RowPitch as usize;
    let src_ptr = mapped.pData as *const u8;
    let (width, height) = (size.0 as usize, size.1 as usize);
    // One row at a time: the padding the pitch may leave after a row's
    // pixels need not be initialized, so no slice spans it.
    let source_row = |row: usize| {
        assert!(row < height);
        // SAFETY: a mapped staging texture of `size` holds `height` rows of
        // `width` BGRA pixels, `RowPitch` apart, until the Unmap.
        unsafe { std::slice::from_raw_parts(src_ptr.add(row * src_pitch), width * 4) }
    };
    super::rotation::copy_upright(rotation, source_row, size, buf, row_bytes);
    unsafe {
        device_context.Unmap(staging_texture, 0);
    }
    Ok(())
}

fn make_staging_desc(width: u32, height: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: Default::default(),
    }
}

/// Query the actual number of dirty rectangles from DXGI for this frame.
fn get_dirty_rect_count(
    duplication: &IDXGIOutputDuplication,
    frame_info: &DXGI_OUTDUPL_FRAME_INFO,
    buf: &mut Vec<RECT>,
) -> usize {
    let meta_size = frame_info.TotalMetadataBufferSize as usize;
    if meta_size == 0 {
        return 0;
    }

    let rect_capacity = meta_size / std::mem::size_of::<RECT>();
    buf.resize(rect_capacity.max(1), RECT::default());

    let mut actual_size = 0u32;
    match unsafe {
        duplication.GetFrameDirtyRects(
            (buf.len() * std::mem::size_of::<RECT>()) as u32,
            buf.as_mut_ptr(),
            &mut actual_size,
        )
    } {
        Ok(()) => actual_size as usize / std::mem::size_of::<RECT>(),
        Err(_) => 0,
    }
}

use super::pointer::{
    SHAPE_COLOR as DXGI_POINTER_SHAPE_COLOR, SHAPE_MASKED_COLOR as DXGI_POINTER_SHAPE_MASKED_COLOR,
    SHAPE_MONOCHROME as DXGI_POINTER_SHAPE_MONOCHROME,
};

/// Caches the latest mouse-cursor shape from DXGI Desktop Duplication. The shape only
/// arrives when it changes, so it's kept across frames and composited at the per-frame
/// pointer position.
#[derive(Default)]
struct CursorState {
    shape: Vec<u8>,
    shape_type: u32,
    width: u32,
    height: u32,
    pitch: u32,
    hot: (u32, u32),
    has_shape: bool,
}

impl CursorState {
    /// The pointer image's size in pixels, once there is one. A monochrome
    /// shape's buffer stacks two masks.
    fn size(&self) -> Option<(u32, u32)> {
        self.has_shape.then_some(match self.shape_type {
            DXGI_POINTER_SHAPE_MONOCHROME => (self.width, self.height / 2),
            _ => (self.width, self.height),
        })
    }

    /// Fetch the current pointer shape. Must be called while a frame is acquired
    /// (before `ReleaseFrame`). On failure the previous shape is kept and
    /// this returns false.
    fn update_shape(&mut self, duplication: &IDXGIOutputDuplication, buffer_size: u32) -> bool {
        self.shape.resize(buffer_size as usize, 0);
        let mut required = 0u32;
        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        match unsafe {
            duplication.GetFramePointerShape(
                buffer_size,
                self.shape.as_mut_ptr() as *mut core::ffi::c_void,
                &mut required,
                &mut info,
            )
        } {
            Ok(()) => {
                self.shape_type = info.Type;
                self.width = info.Width;
                self.height = info.Height;
                self.pitch = info.Pitch;
                self.hot = (info.HotSpot.x.max(0) as u32, info.HotSpot.y.max(0) as u32);
                self.has_shape = true;
                true
            }
            Err(e) => {
                warn!(error = %e, "GetFramePointerShape failed — cursor shape not updated");
                false
            }
        }
    }
}

/// Composite the cached cursor into a BGRA frame at top-left `(px, py)` (this output's
/// pixel space). Handles the three DXGI pointer shape types; out-of-bounds pixels are
/// clipped.
fn draw_cursor(
    buf: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    row_bytes: usize,
    cursor: &CursorState,
    px: i32,
    py: i32,
) {
    match cursor.shape_type {
        DXGI_POINTER_SHAPE_COLOR => {
            draw_color_cursor(buf, frame_w, frame_h, row_bytes, cursor, px, py, false)
        }
        DXGI_POINTER_SHAPE_MASKED_COLOR => {
            draw_color_cursor(buf, frame_w, frame_h, row_bytes, cursor, px, py, true)
        }
        DXGI_POINTER_SHAPE_MONOCHROME => {
            draw_monochrome_cursor(buf, frame_w, frame_h, row_bytes, cursor, px, py)
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_color_cursor(
    buf: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    row_bytes: usize,
    cursor: &CursorState,
    px: i32,
    py: i32,
    masked: bool,
) {
    let pitch = cursor.pitch as usize;
    for row in 0..cursor.height {
        let sy = py + row as i32;
        if sy < 0 || sy >= frame_h as i32 {
            continue;
        }
        for col in 0..cursor.width {
            let sx = px + col as i32;
            if sx < 0 || sx >= frame_w as i32 {
                continue;
            }
            let src = row as usize * pitch + col as usize * 4;
            if src + 4 > cursor.shape.len() {
                continue;
            }
            let (b, g, r, a) = (
                cursor.shape[src],
                cursor.shape[src + 1],
                cursor.shape[src + 2],
                cursor.shape[src + 3],
            );
            let dst = sy as usize * row_bytes + sx as usize * 4;
            if dst + 4 > buf.len() {
                continue;
            }
            if masked {
                // MASKED_COLOR: alpha 0 = opaque pixel; alpha 0xFF = XOR with screen.
                if a == 0 {
                    buf[dst] = b;
                    buf[dst + 1] = g;
                    buf[dst + 2] = r;
                } else {
                    buf[dst] ^= b;
                    buf[dst + 1] ^= g;
                    buf[dst + 2] ^= r;
                }
            } else {
                // COLOR: straight alpha blend over the screen.
                let af = a as u32;
                let inv = 255 - af;
                buf[dst] = ((b as u32 * af + buf[dst] as u32 * inv) / 255) as u8;
                buf[dst + 1] = ((g as u32 * af + buf[dst + 1] as u32 * inv) / 255) as u8;
                buf[dst + 2] = ((r as u32 * af + buf[dst + 2] as u32 * inv) / 255) as u8;
            }
        }
    }
}

fn draw_monochrome_cursor(
    buf: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    row_bytes: usize,
    cursor: &CursorState,
    px: i32,
    py: i32,
) {
    // Monochrome shapes pack an AND mask (top half) and an XOR mask (bottom half),
    // 1 bit per pixel. The real cursor height is half the reported buffer height.
    let pitch = cursor.pitch as usize;
    let cur_h = cursor.height / 2;
    for row in 0..cur_h {
        let sy = py + row as i32;
        if sy < 0 || sy >= frame_h as i32 {
            continue;
        }
        for col in 0..cursor.width {
            let sx = px + col as i32;
            if sx < 0 || sx >= frame_w as i32 {
                continue;
            }
            let byte = col as usize / 8;
            let bit = 7u8 - (col % 8) as u8;
            let and_idx = row as usize * pitch + byte;
            let xor_idx = (row + cur_h) as usize * pitch + byte;
            if xor_idx >= cursor.shape.len() {
                continue;
            }
            let and_bit = (cursor.shape[and_idx] >> bit) & 1;
            let xor_bit = (cursor.shape[xor_idx] >> bit) & 1;
            let dst = sy as usize * row_bytes + sx as usize * 4;
            if dst + 4 > buf.len() {
                continue;
            }
            match (and_bit, xor_bit) {
                (0, 0) => {
                    // Opaque black.
                    buf[dst] = 0;
                    buf[dst + 1] = 0;
                    buf[dst + 2] = 0;
                }
                (0, 1) => {
                    // Opaque white.
                    buf[dst] = 255;
                    buf[dst + 1] = 255;
                    buf[dst + 2] = 255;
                }
                (1, 0) => {} // Transparent — leave the screen pixel.
                _ => {
                    // (1, 1): invert the screen pixel.
                    buf[dst] = !buf[dst];
                    buf[dst + 1] = !buf[dst + 1];
                    buf[dst + 2] = !buf[dst + 2];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Native: GDI reads the primary display at its own size, as opaque BGRA
    /// in a capture buffer, and nothing at another size or turned on its
    /// side. Needs an interactive desktop that isn't all black; skips with no
    /// output listed (a non-interactive SSH session).
    /// `cargo test -p eternal-host --lib the_screen_is_read_through_gdi -- --ignored --nocapture`
    #[test]
    #[ignore = "native: needs an interactive Windows desktop"]
    fn the_screen_is_read_through_gdi_only_at_its_own_size() {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.expect("DXGI factory");
        let shared = SharedControl::new(0, 8_000_000);
        let (listed, _) = super::super::listing::listed(&shared, &factory);
        let Some(primary) = listed.into_iter().find(|listed| listed.info.is_primary) else {
            eprintln!("skipping: no display output is listed in this session");
            return;
        };
        let output: IDXGIOutput1 = primary.output.cast().expect("IDXGIOutput1");
        let (width, height) = (primary.info.width, primary.info.height);
        let pixels =
            read_screen_gdi(&output, (width, height)).expect("the desktop read through GDI");
        let picture_bytes = (width * height * 4) as usize;
        assert_eq!(pixels.len(), picture_bytes + FRAME_PADDING);
        let (read, _) = pixels[..picture_bytes].as_chunks::<4>();
        assert!(read.iter().all(|pixel| pixel[3] == 255), "not opaque");
        assert!(read.iter().any(|pixel| pixel[..3] != [0, 0, 0]));
        assert!(
            read_screen_gdi(&output, (width + 1, height)).is_none(),
            "another size was read"
        );
        if width != height {
            assert!(
                read_screen_gdi(&output, (height, width)).is_none(),
                "the turned size was read"
            );
        }
        eprintln!("read {width}x{height} through GDI");
    }

    /// `pixels`, `width`x`height` BGRA rows, as a top-down 32-bit BMP.
    fn write_bmp(path: &std::path::Path, (width, height): (u32, u32), pixels: &[u8]) {
        let image = (width * height * 4) as usize;
        let mut file = Vec::with_capacity(54 + image);
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&(54 + image as u32).to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&(width as i32).to_le_bytes());
        file.extend_from_slice(&(-(height as i32)).to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&[0; 24]);
        file.extend_from_slice(&pixels[..image]);
        std::fs::write(path, file).expect("write the picture");
    }

    /// The share of pixels of the same colour in two BGRA pictures, `a_size`
    /// and `b_size` with rows of their own width, where they overlap from
    /// their top-left corners.
    fn matching_share(a: &[u8], a_size: (u32, u32), b: &[u8], b_size: (u32, u32)) -> f64 {
        let width = a_size.0.min(b_size.0) as usize;
        let height = a_size.1.min(b_size.1) as usize;
        let same: usize = (0..height)
            .map(|y| {
                let (ours, _) = a[y * a_size.0 as usize * 4..][..width * 4].as_chunks::<4>();
                let (theirs, _) = b[y * b_size.0 as usize * 4..][..width * 4].as_chunks::<4>();
                ours.iter()
                    .zip(theirs)
                    .filter(|(p, q)| p[..3] == q[..3])
                    .count()
            })
            .sum();
        same as f64 / (width * height).max(1) as f64
    }

    /// Native: each screen, as turned in Windows' display settings, reads
    /// back upright. The staging texture takes the acquired image's size, as
    /// capture's does; turned by the duplication's rotation the picture is
    /// the desktop rect's size and matches what GDI shows of it (most of it:
    /// the pointer and anything that moved in between differ), clearly
    /// better than turned any of the other three ways. Prints what Windows
    /// reports for each and how well each turn matches, and with
    /// `ETERNAL_ROTATION_DUMP` set to a folder, saves both pictures there as
    /// BMPs to look at. `ETERNAL_ROTATION_ONLY` names the one display to
    /// check, which then must be compared. Something must change on the
    /// screen for Desktop Duplication to give an image, and it must not look
    /// the same turned (`Pattern-Window.ps1`).
    /// `cargo test -p eternal-host --lib every_screen_is_read_back_upright -- --ignored --nocapture`
    #[test]
    #[ignore = "native: needs an interactive Windows desktop"]
    fn every_screen_is_read_back_upright() {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        // The right turn matches at least this much better than any other:
        // an animated wallpaper costs the right one about a tenth, and a
        // wrong one matched under a third (November 2026).
        const CLEAR_MARGIN: f64 = 0.25;
        const TURNS: [Rotation; 4] = [
            Rotation::Identity,
            Rotation::Rotate90,
            Rotation::Rotate180,
            Rotation::Rotate270,
        ];
        // Desktop coordinates in pixels, as the app (per-monitor aware) sees them.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let only = std::env::var("ETERNAL_ROTATION_ONLY").ok();
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.expect("DXGI factory");
        let shared = SharedControl::new(0, 8_000_000);
        let (listed, _) = super::super::listing::listed(&shared, &factory);
        if listed.is_empty() && only.is_none() {
            eprintln!("skipping: no display output is listed in this session");
            return;
        }
        let dump = std::env::var_os("ETERNAL_ROTATION_DUMP").map(std::path::PathBuf::from);
        let mut compared = 0;
        for listed in listed {
            if only
                .as_ref()
                .is_some_and(|only| !listed.info.device_name.eq_ignore_ascii_case(only))
            {
                continue;
            }
            let name = listed.info.device_name.replace(['\\', '.'], "");
            let device = create_device(listed.adapter.clone(), || {})
                .unwrap()
                .expect("a device on the screen's card");
            let context = unsafe { device.GetImmediateContext() }.unwrap();
            let output: IDXGIOutput1 = listed.output.cast().expect("IDXGIOutput1");
            let Ok(Ok(duplication)) = duplicate_output(&output, &device, || {}) else {
                eprintln!("{name}: not duplicated here (another session or card); skipped");
                continue;
            };
            let desc = unsafe { duplication.GetDesc() };
            let rect = unsafe { output.GetDesc() }.unwrap().DesktopCoordinates;
            let rotation = Rotation::from_dxgi(desc.Rotation.0);
            let mode = (desc.ModeDesc.Width, desc.ModeDesc.Height);
            let desktop = (
                (rect.right - rect.left) as u32,
                (rect.bottom - rect.top) as u32,
            );
            eprintln!(
                "{name}: mode {}x{}, rotation {} ({rotation:?}), desktop {}x{}",
                mode.0, mode.1, desc.Rotation.0, desktop.0, desktop.1
            );
            if mode != desktop {
                eprintln!("{name}: NOTE the mode is not the desktop's size, as measured before");
            }
            let mut staged = None;
            for _ in 0..40 {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                if let Err(error) =
                    unsafe { duplication.AcquireNextFrame(100, &mut info, &mut resource) }
                {
                    if error.code() != DXGI_ERROR_WAIT_TIMEOUT {
                        eprintln!("{name}: AcquireNextFrame: {error}");
                    }
                    continue;
                }
                let texture: ID3D11Texture2D = resource.unwrap().cast().unwrap();
                let mut tex_desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { texture.GetDesc(&mut tex_desc) };
                let fresh = info.LastPresentTime != 0;
                eprintln!(
                    "{name}: frame {}x{}, presented {}, accumulated {}",
                    tex_desc.Width, tex_desc.Height, info.LastPresentTime, info.AccumulatedFrames
                );
                if fresh {
                    let image = (tex_desc.Width, tex_desc.Height);
                    if image != rotation.duplicated(mode) {
                        eprintln!(
                            "{name}: NOTE the image is not the mode turned as measured before"
                        );
                    }
                    let staging: ID3D11Texture2D = unsafe {
                        let mut tex = None;
                        device
                            .CreateTexture2D(
                                &make_staging_desc(image.0, image.1),
                                None,
                                Some(&mut tex),
                            )
                            .unwrap();
                        tex.unwrap()
                    };
                    unsafe { context.CopyResource(&staging, &texture) };
                    staged = Some((staging, image));
                }
                unsafe { duplication.ReleaseFrame() }.unwrap();
                if staged.is_some() {
                    break;
                }
            }
            let Some((staging, image)) = staged else {
                eprintln!("{name}: no new image within 4 s; skipped");
                continue;
            };
            let Some(gdi) = read_screen_gdi(&output, desktop) else {
                eprintln!("{name}: GDI read nothing (black or no desktop); skipped");
                continue;
            };
            assert_eq!(
                rotation.upright(image),
                desktop,
                "{name}: the image turned upright is the desktop's size"
            );
            let mut shares = Vec::new();
            let mut upright = Vec::new();
            for turn in TURNS {
                let size = turn.upright(image);
                let row_bytes = size.0 as usize * 4;
                let mut picture = frame_buffer(row_bytes * size.1 as usize);
                copy_staging(&context, &staging, image, turn, row_bytes, &mut picture).unwrap();
                let share = matching_share(&picture, size, &gdi, desktop);
                eprintln!(
                    "{name}: turned {turn:?}, {:.1}% matches GDI's picture",
                    share * 100.0
                );
                shares.push((turn, share));
                if turn == rotation {
                    upright = picture;
                }
            }
            if let Some(dir) = &dump {
                std::fs::create_dir_all(dir).unwrap();
                write_bmp(&dir.join(format!("{name}-upright.bmp")), desktop, &upright);
                write_bmp(&dir.join(format!("{name}-gdi.bmp")), desktop, &gdi);
            }
            let share = shares
                .iter()
                .find(|(turn, _)| *turn == rotation)
                .map(|&(_, share)| share)
                .unwrap();
            assert!(
                share > 0.75,
                "{name}: the upright picture is not the desktop"
            );
            for &(turn, other) in &shares {
                assert!(
                    turn == rotation || share - other >= CLEAR_MARGIN,
                    "{name}: turned {rotation:?} matches {:.1}% and turned {turn:?} {:.1}%: \
                     this desktop does not tell the turns apart (Pattern-Window.ps1)",
                    share * 100.0,
                    other * 100.0
                );
            }
            compared += 1;
        }
        if let Some(only) = &only {
            assert!(
                compared > 0,
                "ETERNAL_ROTATION_ONLY={only}: no display was compared"
            );
        }
    }

    /// Native: a real D3D11CreateDevice held past its limit. The caller gets
    /// the timeout on time, a second attempt is refused at once, the late
    /// device is released on its own thread, and then a device is created.
    /// `cargo test -p eternal-host --lib a_real_device_creation -- --ignored --nocapture`
    #[test]
    #[ignore = "native: needs a Windows graphics adapter"]
    fn a_real_device_creation_that_stalls_recovers() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        std::env::set_var("ETERNAL_SIMULATE_STALL", "d3d11_create_device,encoder_open");
        let adapter = || {
            let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.unwrap();
            unsafe { factory.EnumAdapters1(0) }.unwrap()
        };
        let mut beats = 0;
        let started = Instant::now();
        let held = create_device(adapter(), || beats += 1);
        let waited = started.elapsed();
        assert!(matches!(held, Err(Stall::TimedOut { .. })), "{held:?}");
        assert!(
            waited >= Duration::from_secs(4) && waited < Duration::from_secs(6),
            "{waited:?}"
        );
        assert!(beats >= 60, "{beats} heartbeats in {waited:?}");
        let started = Instant::now();
        let refused = create_device(adapter(), || {});
        assert!(matches!(refused, Err(Stall::Refused { .. })), "{refused:?}");
        assert!(started.elapsed() < Duration::from_millis(500));
        // The held call creates its device 8 s after it began, and drops it.
        let deadline = Instant::now() + Duration::from_secs(60);
        while crate::stall::abandoned(Call::D3d11CreateDevice) > 0 {
            assert!(
                Instant::now() < deadline,
                "the late device was not released"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let device = create_device(adapter(), || {}).unwrap().unwrap();
        assert!(unsafe { device.GetImmediateContext() }.is_ok());
    }
}
