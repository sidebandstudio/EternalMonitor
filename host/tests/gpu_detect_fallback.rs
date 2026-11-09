//! The real encode stage on the software facts startup uses when GPU
//! detection did not return in time: the processor encodes, and the window
//! says why.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eternal_host::capture::synthetic::render_synthetic_frame;
use eternal_host::capture::{frame_buffer, frame_slot, RawFrame};
use eternal_host::control::SharedControl;
use eternal_host::encoder::{run_encode_stage, NAL_CHANNEL_CAPACITY};
use eternal_host::gpu::GpuInfo;
use eternal_host::stats::PIPELINE_STATS;

/// The stage and PIPELINE_STATS are process-global, so stages run one at a time.
static STAGE_LOCK: Mutex<()> = Mutex::new(());

/// Runs a few frames through a fresh encode stage with `gpu` and the
/// encoder override `chosen`, and returns the fallback notice's two flags.
fn notice_after_stage(gpu: GpuInfo, chosen: Option<&str>) -> (bool, bool) {
    let _guard = STAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ffmpeg_next::init().unwrap();
    let (producer, consumer) = frame_slot();
    let (tx, mut rx) = tokio::sync::mpsc::channel(NAL_CHANNEL_CAPACITY);
    let shared = SharedControl::new(0, 8_000_000);
    shared.running.store(true, Ordering::SeqCst);
    *shared.encoder_override.lock() = chosen.map(str::to_string);
    let generation = eternal_host::supervisor::CURRENT_GENERATION.load(Ordering::SeqCst);
    let stage = std::thread::spawn(move || {
        run_encode_stage(consumer, tx, shared, gpu, generation).map_err(|e| e.to_string())
    });
    let collector = std::thread::spawn(move || {
        let mut packets = 0;
        while rx.blocking_recv().is_some() {
            packets += 1;
        }
        packets
    });
    let (width, height) = (640, 360);
    for number in 0..10 {
        let mut data = frame_buffer((width * height * 4) as usize);
        render_synthetic_frame(&mut data, width, height, number);
        producer.publish(RawFrame {
            frame_number: number,
            timestamp: Instant::now(),
            data: Arc::new(data),
            width,
            height,
            cached: false,
        });
        std::thread::sleep(Duration::from_millis(20));
    }
    producer.close();
    stage.join().unwrap().expect("the encode stage failed");
    assert!(collector.join().unwrap() > 0, "no packets");
    let stats = PIPELINE_STATS.lock();
    (stats.using_software_fallback, stats.gpu_detection_stalled)
}

#[test]
fn a_stalled_detection_encodes_on_the_processor_with_the_notice() {
    let stalled = GpuInfo {
        detection_stalled: true,
        ..GpuInfo::software_fallback()
    };
    assert_eq!(notice_after_stage(stalled.clone(), None), (true, true));
    // libx264 chosen in Settings is the user's choice, not a fallback.
    assert_eq!(notice_after_stage(stalled, Some("libx264")), (false, false));
    // A detection that answered and found no graphics card: no notice, as
    // before.
    assert_eq!(
        notice_after_stage(GpuInfo::software_fallback(), None),
        (false, false)
    );
}
