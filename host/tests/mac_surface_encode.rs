//! ScreenCaptureKit-style frames, which carry only a pixel buffer, through
//! the FFmpeg encode path (an `ETERNAL_MAC_ENCODER=ffmpeg` override or any
//! FFmpeg encoder on macOS). The frames must be encoded, not skipped, and
//! decode to the picture that was captured.
#![cfg(target_os = "macos")]

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eternal_host::capture::sck::Surface;
use eternal_host::capture::synthetic::{decode_frame_counter_from_luma, render_synthetic_frame};
use eternal_host::capture::{frame_slot, RawFrame};
use eternal_host::control::SharedControl;
use eternal_host::encoder::{run_encode_stage, NAL_CHANNEL_CAPACITY};
use eternal_host::gpu::GpuInfo;

const SIZE: (u32, u32) = (640, 360);
const FRAMES: u64 = 30;

#[test]
fn surface_only_frames_are_encoded_on_the_ffmpeg_path() {
    ffmpeg_next::init().unwrap();
    let (producer, consumer) = frame_slot();
    let (tx, mut rx) = tokio::sync::mpsc::channel(NAL_CHANNEL_CAPACITY);
    let shared = SharedControl::new(0, 8_000_000);
    shared.running.store(true, Ordering::SeqCst);
    // libx264 is an FFmpeg encoder, so the direct VideoToolbox backend is
    // not selected.
    let gpu = GpuInfo {
        encoder_name: "libx264".into(),
        codec_display_name: "libx264".into(),
        ..GpuInfo::software_fallback()
    };
    let generation = eternal_host::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
    let stage_shared = shared.clone();
    let stage = std::thread::spawn(move || {
        run_encode_stage(consumer, tx, stage_shared, gpu, generation).map_err(|e| e.to_string())
    });
    let collector = std::thread::spawn(move || {
        let mut packets = Vec::new();
        while let Some(nal) = rx.blocking_recv() {
            packets.push(nal);
        }
        packets
    });

    let (width, height) = SIZE;
    for number in 1..=FRAMES {
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        render_synthetic_frame(&mut bgra, width, height, number);
        let surface = Surface::from_bgra(width, height, &bgra).expect("pixel buffer");
        producer.publish(RawFrame {
            frame_number: number,
            timestamp: Instant::now(),
            data: Arc::new(Vec::new()),
            width,
            height,
            surface: Some(surface),
        });
        std::thread::sleep(Duration::from_millis(20));
    }
    producer.close();
    stage.join().unwrap().expect("encode stage");
    let packets = collector.join().unwrap();
    assert!(!packets.is_empty(), "surface-only frames were skipped");

    // Decode and read back the frame counters the pictures carry.
    let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264).unwrap();
    let mut decoder = ffmpeg_next::codec::Context::new_with_codec(codec)
        .decoder()
        .video()
        .unwrap();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let mut counters = Vec::new();
    for nal in &packets {
        decoder
            .send_packet(&ffmpeg_next::Packet::copy(&nal.data))
            .unwrap();
        while decoder.receive_frame(&mut frame).is_ok() {
            counters.push(decode_frame_counter_from_luma(
                frame.data(0),
                frame.stride(0),
                frame.width(),
            ));
        }
    }
    decoder.send_eof().unwrap();
    while decoder.receive_frame(&mut frame).is_ok() {
        counters.push(decode_frame_counter_from_luma(
            frame.data(0),
            frame.stride(0),
            frame.width(),
        ));
    }
    assert!(!counters.is_empty(), "nothing decoded");
    assert!(
        counters
            .iter()
            .all(|counter| (1..=FRAMES).contains(counter)),
        "decoded pictures are not the captured ones: {counters:?}"
    );
    assert!(
        counters.windows(2).all(|pair| pair[0] < pair[1]),
        "{counters:?}"
    );
}
