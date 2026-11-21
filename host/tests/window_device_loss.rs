//! The window survives the loss of its graphics device, as in a driver
//! reset. egui-wgpu 0.36 panicked on the next frame ("Failed to create
//! staging buffer for index data"), which ended the app and the stream; the
//! patched copy in `vendor/egui-wgpu` skips the frame and replaces the device
//! on a schedule (`Replacement`), giving up after repeated failures.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::winit::{watch, Answer, Asked, Replacement, REPLACEMENT_TIMEOUT};
use eframe::egui_wgpu::{
    replacement_candidates, AdapterKey, RenderState, Renderer, RendererOptions, ScreenDescriptor,
    WgpuConfiguration,
};
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

#[test]
fn a_stuck_replacement_times_out_without_blocking_and_its_late_device_is_taken() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let start = Instant::now();
    // As a driver call that stalls while the replacement device is made.
    let mut answer = Answer::ask("test replacement", start, REPLACEMENT_TIMEOUT, move || {
        let _ = released.recv();
        "device"
    })
    .unwrap();
    assert!(matches!(answer.poll(start), Asked::Waiting));
    assert!(matches!(
        answer.poll(start + REPLACEMENT_TIMEOUT),
        Asked::TimedOut
    ));
    // One timeout per period, each a failed try for the backoff.
    assert!(matches!(
        answer.poll(start + REPLACEMENT_TIMEOUT),
        Asked::Waiting
    ));
    assert!(matches!(
        answer.poll(start + REPLACEMENT_TIMEOUT * 2),
        Asked::TimedOut
    ));

    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match answer.poll(start) {
            Asked::Answered(device) => break assert_eq!(device, "device"),
            Asked::Waiting => std::thread::sleep(Duration::from_millis(10)),
            other => panic!("{other:?}"),
        }
        assert!(Instant::now() < deadline);
    }
}

#[test]
fn a_replacement_thread_that_dies_ends_the_wait() {
    let start = Instant::now();
    let mut answer = Answer::<()>::ask("test replacement", start, REPLACEMENT_TIMEOUT, || {
        panic!("simulated driver panic")
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !matches!(answer.poll(start), Asked::Ended) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_replacement_that_never_answers_ends_in_giving_up() {
    // The painter counts each timeout of a stuck replacement as a failed
    // try, and starts no other while it is out.
    let (_never, never) = std::sync::mpsc::channel::<()>();
    let mut now = Instant::now();
    let mut replacement = Replacement::default();
    replacement.lost(now);
    let mut answer = Answer::ask("test replacement", now, REPLACEMENT_TIMEOUT, move || {
        let _ = never.recv();
    })
    .unwrap();
    let mut timeouts = 0;
    for _ in 0..100 {
        if replacement.wait(now).is_none() {
            break;
        }
        now += REPLACEMENT_TIMEOUT;
        if let Asked::TimedOut = answer.poll(now) {
            timeouts += 1;
            replacement.failed(now);
        }
        assert!(timeouts <= Replacement::MAX_FAILURES);
    }
    assert_eq!(timeouts, Replacement::MAX_FAILURES);
}

#[test]
fn a_replacement_device_is_made_without_any_window_or_surface() {
    // What the replacement thread runs gets the configuration, the instance and
    // adapter identities only, so a thread stuck in a driver can't hold the
    // window or touch it after it is gone.
    let config = WgpuConfiguration::default();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let drew_on = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .expect("a graphics adapter");
    let preferred = AdapterKey::of(&drew_on);
    let lost = Arc::new(AtomicBool::new(false));
    let new = block_on(RenderState::new_device(
        &config,
        &instance,
        Some(&preferred),
        &[],
        |device| watch(device, &lost),
    ))
    .expect("a device");
    assert_eq!(new.adapter_key(), preferred);
    // The loss handlers are on the device: a wgpu error marks it lost, where
    // wgpu's default handler would panic.
    assert!(!lost.load(Ordering::SeqCst));
    let _misaligned = new.device().create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 3,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    assert!(lost.load(Ordering::SeqCst));
    let buffer = new.device().create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    assert_eq!(buffer.size(), 256);
}

fn adapter(name: &str, backend: wgpu::Backend, vendor: u32) -> AdapterKey {
    AdapterKey {
        luid: None,
        backend,
        vendor,
        device: 1,
        device_type: wgpu::DeviceType::DiscreteGpu,
        name: name.into(),
        pci_bus: String::new(),
    }
}

#[test]
fn a_replacement_prefers_the_adapter_that_drew_and_moves_past_ones_that_cannot() {
    use wgpu::Backend::{Dx12, Vulkan};
    // A hybrid PC: the window started on the dGPU with DX12.
    let igpu = adapter("iGPU", Dx12, 0x1002);
    let dgpu = adapter("dGPU", Dx12, 0x10de);
    let dgpu_vulkan = adapter("dGPU", Vulkan, 0x10de);
    let listed = [igpu.clone(), dgpu.clone(), dgpu_vulkan.clone()];

    // The adapter that drew is the only choice while it is listed.
    assert_eq!(replacement_candidates(&listed, Some(&dgpu), &[]), [1]);
    // Gone from the listing, as across a driver reset: any of the others.
    assert_eq!(
        replacement_candidates(&[igpu.clone(), dgpu_vulkan.clone()], Some(&dgpu), &[]),
        [0, 1]
    );
    // Nothing left to try.
    assert!(replacement_candidates(&listed, Some(&dgpu), &listed).is_empty());

    // Each one the window can't draw on is left out, and the next try moves on,
    // with the selector taking the first candidate.
    let can_draw = |key: &AdapterKey| *key == dgpu_vulkan;
    let mut excluded = Vec::new();
    let mut tried = Vec::new();
    let chosen = loop {
        let candidates = replacement_candidates(&listed, Some(&dgpu), &excluded);
        let key = listed[candidates[0]].clone();
        tried.push(key.clone());
        if can_draw(&key) {
            break key;
        }
        excluded.push(key);
        assert!(tried.len() <= listed.len());
    };
    assert_eq!(chosen, dgpu_vulkan);
    assert_eq!(tried, [dgpu, igpu, dgpu_vulkan]);
}

#[test]
fn two_identical_cards_are_told_apart_by_their_luid() {
    use wgpu::Backend::Dx12;
    // Two of the same card on different buses: wgpu's DX12 bus comes from the
    // PCI ids, so everything but the LUID matches.
    let card = |luid| AdapterKey {
        luid: Some(luid),
        pci_bus: "0000:01:00.0".into(),
        ..adapter("RTX 5080", Dx12, 0x10de)
    };
    let (first, second) = (card(0x0000_0000_0001_a2b3), card(0x0000_0000_0001_c4d5));
    let listed = [first.clone(), second.clone()];

    // The one that drew is chosen, not its twin listed first.
    assert_eq!(replacement_candidates(&listed, Some(&second), &[]), [1]);
    // Leaving one out leaves the other.
    assert_eq!(
        replacement_candidates(&listed, Some(&first), std::slice::from_ref(&first)),
        [1]
    );
    assert_eq!(replacement_candidates(&listed, None, &[second]), [0]);
}

#[cfg(windows)]
#[test]
fn every_dx12_adapter_has_its_own_luid() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::DX12));
    assert!(!adapters.is_empty());
    let luids: Vec<u64> = adapters
        .iter()
        .map(|adapter| AdapterKey::of(adapter).luid.expect("a DX12 LUID"))
        .collect();
    let mut unique = luids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), luids.len(), "{luids:x?}");
}

#[test]
fn an_adapter_with_a_new_luid_after_a_driver_update_is_still_itself() {
    use wgpu::Backend::Vulkan;
    // Two of the same card; Vulkan reports each one's own PCI bus.
    let card = |luid, bus: &str| AdapterKey {
        luid: Some(luid),
        pci_bus: bus.into(),
        ..adapter("RTX 5080", Vulkan, 0x10de)
    };
    let drew = card(1, "0000:01:00.0");
    let twin = card(2, "0000:02:00.0");
    // A driver update gives the card that drew a new LUID; its twin is listed first.
    let listed = [twin.clone(), card(3, "0000:01:00.0")];
    assert_eq!(replacement_candidates(&listed, Some(&drew), &[]), [1]);
    // An adapter left out stays left out with its new LUID.
    assert_eq!(
        replacement_candidates(&listed, None, std::slice::from_ref(&drew)),
        [0]
    );
    assert_eq!(
        replacement_candidates(&listed, Some(&twin), std::slice::from_ref(&drew)),
        [0]
    );
}

#[test]
fn two_cards_alike_but_for_their_new_luids_are_no_preference() {
    use wgpu::Backend::Dx12;
    // On DX12 identical cards share the bus too: only the LUID tells them apart,
    // and after an update both have new ones.
    let card = |luid| AdapterKey {
        luid: Some(luid),
        pci_bus: "0000:01:00.0".into(),
        ..adapter("RTX 5080", Dx12, 0x10de)
    };
    let listed = [card(3), card(4)];
    assert_eq!(replacement_candidates(&listed, Some(&card(1)), &[]), [0, 1]);
    // Nor is either left out for an old LUID.
    assert_eq!(replacement_candidates(&listed, None, &[card(2)]), [0, 1]);
}
