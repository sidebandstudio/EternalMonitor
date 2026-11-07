//! Synthetic capture source: a deterministic moving test pattern in BGRA.
//!
//! This is the capture backend on non-Windows platforms (there is no DXGI),
//! which makes the whole pipeline — encode, fragment, send — runnable on a
//! development Mac and in CI. It is also selectable on Windows via
//! `ETERNAL_CAPTURE=synthetic` for debugging the pipeline without touching
//! Desktop Duplication.
//!
//! Every frame carries its frame number encoded as a row of black/white
//! squares that survives lossy H.264 encoding, so end-to-end tests can decode
//! frames and assert real progression through the entire stack.
//!
//! `ETERNAL_SYNTH_POINTER=1` instead holds a still picture and moves a
//! pointer over it every frame, as when only the mouse moves on a still
//! desktop (see [`PointerScene`]).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use tracing::info;

use super::pointer::{convert_shape, PointerImage, Position, SHAPE_COLOR};
use super::{frame_budget_for, heartbeat, FrameSlot, RawFrame, IDLE_KEEPALIVE};
use crate::control::SharedControl;
use crate::stats::PIPELINE_STATS;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;

/// Geometry of the frame-counter marker row (see [`draw_frame_counter`]).
pub const COUNTER_BITS: usize = 24;
pub const COUNTER_SQUARE: usize = 16;
pub const COUNTER_MARGIN: usize = 4;
/// Height of the solid strip that hosts the counter squares.
pub const COUNTER_STRIP_H: usize = COUNTER_SQUARE + 2 * COUNTER_MARGIN;

/// Resolution for the synthetic source: `ETERNAL_SYNTH_SIZE=WxH`, else 1280x720.
fn synthetic_size() -> (u32, u32) {
    if let Ok(spec) = std::env::var("ETERNAL_SYNTH_SIZE") {
        if let Some((w, h)) = spec.trim().split_once(['x', 'X']) {
            if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                if (16..=7680).contains(&w) && (16..=4320).contains(&h) {
                    return (w, h);
                }
            }
        }
    }
    (DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

/// Produce frames until `shared.running` clears or the channel closes.
/// Mirrors the DXGI loop's contract: paced by `target_fps`, records capture
/// stats, ships BGRA `RawFrame`s.
pub fn run_capture_loop(
    slot: &FrameSlot,
    shared: SharedControl,
    generation: u64,
    frame_number: &mut u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // End-to-end tests with a fake virtual display: mirror or capture the
    // second screen as DXGI capture would, and prove it with the first frame.
    #[cfg(debug_assertions)]
    let Some(mut proving) = super::second_screen::fake::capture_second_screen(&shared, generation) else {
        return Ok(());
    };
    #[cfg(debug_assertions)]
    if !proving {
        super::second_screen::fake::mirror(&shared)?;
    }
    let (width, height) = synthetic_size();
    let mut buffers = capture_buffers(width, height);

    info!(width, height, "Synthetic capture source active");
    *shared.capture_geometry.lock() = Some(crate::input::CaptureGeometry {
        left: 0,
        top: 0,
        width,
        height,
    });

    // Absolute deadlines preserve the requested source rate. The macOS timer
    // avoids normal sleep coalescing without burning an entire CPU core.
    let mut timer = super::timing::FrameTimer::new();
    let mut next_deadline = Instant::now();
    let mut pointer = std::env::var("ETERNAL_SYNTH_POINTER")
        .is_ok_and(|v| v.trim() == "1")
        .then(|| PointerScene::new(&shared, width, height));

    loop {
        if !shared.running.load(Ordering::SeqCst)
            || !crate::supervisor::generation_is_current(generation)
        {
            info!("Capture loop stopping (stop requested or generation superseded)");
            break;
        }

        let frame_start = Instant::now();
        let frame_budget = frame_budget_for(shared.effective_fps());

        heartbeat(&shared.hb_capture_loop_ms);

        let data = match pointer.as_mut() {
            None => {
                *frame_number += 1;
                Some(render_next_frame(
                    &mut buffers,
                    width,
                    height,
                    *frame_number,
                ))
            }
            Some(scene) => scene.tick(&shared, &mut buffers, frame_start, frame_number),
        };

        if let Some(data) = data {
            PIPELINE_STATS.lock().record_capture(width, height);
            heartbeat(&shared.hb_capture_frame_ms);

            slot.publish(RawFrame {
                frame_number: *frame_number,
                timestamp: frame_start,
                data,
                width,
                height,
            });
            #[cfg(debug_assertions)]
            if std::mem::take(&mut proving) {
                super::second_screen::note_image(&shared, generation);
            }
        }

        next_deadline += frame_budget;
        let now = Instant::now();
        if next_deadline <= now {
            // Fell behind (encoder backpressure): don't accumulate debt.
            next_deadline = now;
            continue;
        }
        timer.wait_until(next_deadline);
    }

    Ok(())
}

/// A still picture with a pointer that moves every frame. For a client that
/// draws the pointer it goes to the pointer feed only, and a frame is sent
/// only for an IDR or the idle keepalive, as the DXGI loop does. For any
/// other client each move is a new frame with the pointer drawn in.
struct PointerScene {
    width: u32,
    height: u32,
    picture: Vec<u8>,
    image: PointerImage,
    step: u32,
    last_publish: Option<Instant>,
}

impl PointerScene {
    fn new(shared: &SharedControl, width: u32, height: u32) -> Self {
        let mut picture = super::frame_buffer((width * 4) as usize * height as usize);
        render_synthetic_frame(&mut picture, width, height, 0);
        let image = arrow();
        shared.pointer.set_shape(image.clone());
        info!(width, height, "Synthetic pointer over a still picture");
        Self {
            width,
            height,
            picture,
            image,
            step: 0,
            last_publish: None,
        }
    }

    fn tick(
        &mut self,
        shared: &SharedControl,
        buffers: &mut [Arc<Vec<u8>>; 3],
        now: Instant,
        frame_number: &mut u64,
    ) -> Option<Arc<Vec<u8>>> {
        self.step = self.step.wrapping_add(1);
        let (x, y) = pointer_path(self.step, self.width, self.height);
        shared.pointer.set_position(Position {
            visible: true,
            x,
            y,
            host_time_us: crate::clock::host_now_us(),
        });
        let draws = shared.client_draws_pointer();
        let due = !draws
            || self
                .last_publish
                .is_none_or(|at| now - at >= IDLE_KEEPALIVE)
            || shared.force_next_idr.load(Ordering::SeqCst);
        if !due {
            return None;
        }
        self.last_publish = Some(now);
        *frame_number += 1;
        let buffer = buffers
            .iter_mut()
            .find(|buffer| Arc::strong_count(buffer) == 1)
            .expect("encoder and latest-frame slot retain at most two buffers");
        let buf = Arc::get_mut(buffer).unwrap();
        buf.copy_from_slice(&self.picture);
        draw_frame_counter(buf, self.width, *frame_number);
        if !draws {
            draw_pointer(buf, self.width, self.height, &self.image, x, y);
        }
        Some(Arc::clone(buffer))
    }
}

/// Where the synthetic pointer is at `step`: across the picture and back,
/// a new position every step.
pub fn pointer_path(step: u32, width: u32, height: u32) -> (i32, i32) {
    let span = width.saturating_sub(64).max(2);
    let along = (step * 5) % (2 * span);
    let x = 16
        + if along < span {
            along
        } else {
            2 * span - along
        } as i32;
    let y = (height / 2) as i32 + ((step % 40) as i32 - 20);
    (x, y)
}

/// A 12x19 white arrow with a black edge, as Windows' color pointer is.
fn arrow() -> PointerImage {
    let (w, h) = (12u32, 19u32);
    let mut raw = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..=y.min(w - 1) {
            let edge = x == 0 || x == y.min(w - 1) || y == h - 1;
            let v = if edge { 0 } else { 255 };
            raw[((y * w + x) * 4) as usize..][..4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    convert_shape(SHAPE_COLOR, w, h, w * 4, (0, 0), &raw).expect("a valid arrow")
}

/// Draws a converted pointer image over a BGRA frame: "over" for its
/// premultiplied pixels, then inverting where its mask says.
pub fn draw_pointer(
    buf: &mut [u8],
    width: u32,
    height: u32,
    image: &PointerImage,
    px: i32,
    py: i32,
) {
    let (iw, ih) = (usize::from(image.width), usize::from(image.height));
    let mask = (image.flags & eternal_wire::v2::control::POINTER_SHAPE_INVERT != 0)
        .then(|| &image.bytes[iw * ih * 4..]);
    for row in 0..ih {
        let sy = py + row as i32;
        if sy < 0 || sy >= height as i32 {
            continue;
        }
        for col in 0..iw {
            let sx = px + col as i32;
            if sx < 0 || sx >= width as i32 {
                continue;
            }
            let src = &image.bytes[(row * iw + col) * 4..][..4];
            let at = (sy as usize * width as usize + sx as usize) * 4;
            let dst = &mut buf[at..at + 3];
            let keep = 255 - u32::from(src[3]);
            for c in 0..3 {
                dst[c] = (u32::from(src[c]) + u32::from(dst[c]) * keep / 255).min(255) as u8;
            }
            if mask
                .is_some_and(|mask| mask[row * iw.div_ceil(8) + col / 8] & (0x80 >> (col % 8)) != 0)
            {
                for c in dst.iter_mut() {
                    *c = !*c;
                }
            }
        }
    }
}

/// The encoder and latest-frame slot can each retain one buffer. Keep three
/// allocations so a delayed consumer never discards a busy spare and causes
/// a fresh full-frame allocation on the next tick.
fn capture_buffers(width: u32, height: u32) -> [Arc<Vec<u8>>; 3] {
    let frame_bytes = (width * 4) as usize * height as usize;
    std::array::from_fn(|_| Arc::new(super::frame_buffer(frame_bytes)))
}

fn render_next_frame(
    buffers: &mut [Arc<Vec<u8>>; 3],
    width: u32,
    height: u32,
    frame_number: u64,
) -> Arc<Vec<u8>> {
    let buffer = buffers
        .iter_mut()
        .find(|buffer| Arc::strong_count(buffer) == 1)
        .expect("encoder and latest-frame slot retain at most two buffers");
    render_synthetic_frame(Arc::get_mut(buffer).unwrap(), width, height, frame_number);
    Arc::clone(buffer)
}

/// Draw the full test pattern for `frame_number` into a BGRA buffer.
pub fn render_synthetic_frame(buf: &mut [u8], width: u32, height: u32, frame_number: u64) {
    let w = width as usize;
    let h = height as usize;
    let row_bytes = w * 4;
    debug_assert!(buf.len() >= row_bytes * h);

    // Slow-moving diagonal gradient background — cheap, deterministic motion
    // that forces the encoder to produce real deltas every frame.
    let phase = (frame_number * 3) as usize;
    for y in COUNTER_STRIP_H.min(h)..h {
        let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..w {
            let v = ((x + y + phase) & 0xFF) as u8;
            let px = &mut row[x * 4..x * 4 + 4];
            px[0] = v; // B
            px[1] = v / 2 + 64; // G
            px[2] = 255 - v; // R
            px[3] = 255;
        }
    }

    // A bright vertical sweep bar so motion is obvious to the eye.
    let bar_x = (phase * 2) % w.max(1);
    let bar_w = (w / 40).max(2);
    for y in COUNTER_STRIP_H.min(h)..h {
        let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
        for x in bar_x..(bar_x + bar_w).min(w) {
            let px = &mut row[x * 4..x * 4 + 4];
            px[0] = 26;
            px[1] = 122;
            px[2] = 255;
            px[3] = 255;
        }
    }

    draw_frame_counter(buf, width, frame_number);
}

/// Paint the counter strip: a black band across the top with [`COUNTER_BITS`]
/// squares; square `i` is white when bit `i` of `frame_number` is set.
/// 16 px squares survive H.264 at any reasonable bitrate.
pub fn draw_frame_counter(buf: &mut [u8], width: u32, frame_number: u64) {
    let w = width as usize;
    let row_bytes = w * 4;
    let strip_rows = COUNTER_STRIP_H.min(buf.len() / row_bytes);

    for y in 0..strip_rows {
        let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
        let (pixels, _) = row.as_chunks_mut::<4>();
        for px in pixels {
            *px = [0, 0, 0, 255];
        }
    }

    for bit in 0..COUNTER_BITS {
        if frame_number & (1 << bit) == 0 {
            continue;
        }
        let x0 = COUNTER_MARGIN + bit * (COUNTER_SQUARE + COUNTER_MARGIN);
        if x0 + COUNTER_SQUARE > w {
            break;
        }
        for y in COUNTER_MARGIN..COUNTER_MARGIN + COUNTER_SQUARE {
            let row = &mut buf[y * row_bytes..(y + 1) * row_bytes];
            for x in x0..x0 + COUNTER_SQUARE {
                let px = &mut row[x * 4..x * 4 + 4];
                px[0] = 255;
                px[1] = 255;
                px[2] = 255;
            }
        }
    }
}

/// Recover the frame number from a decoded luma (Y) plane. The inverse of
/// [`draw_frame_counter`] after a lossy encode/decode round trip: samples the
/// center of each square and thresholds against mid-gray.
pub fn decode_frame_counter_from_luma(luma: &[u8], stride: usize, width: u32) -> u64 {
    let w = width as usize;
    let cy = COUNTER_MARGIN + COUNTER_SQUARE / 2;
    let mut value = 0u64;
    for bit in 0..COUNTER_BITS {
        let x0 = COUNTER_MARGIN + bit * (COUNTER_SQUARE + COUNTER_MARGIN);
        let cx = x0 + COUNTER_SQUARE / 2;
        if cx >= w {
            break;
        }
        let sample = luma.get(cy * stride + cx).copied().unwrap_or(0);
        if sample > 128 {
            value |= 1 << bit;
        }
    }
    value
}

/// Same recovery from the original BGRA buffer (for unit tests without a codec).
pub fn decode_frame_counter_from_bgra(buf: &[u8], width: u32) -> u64 {
    let w = width as usize;
    let row_bytes = w * 4;
    let cy = COUNTER_MARGIN + COUNTER_SQUARE / 2;
    let mut value = 0u64;
    for bit in 0..COUNTER_BITS {
        let x0 = COUNTER_MARGIN + bit * (COUNTER_SQUARE + COUNTER_MARGIN);
        let cx = x0 + COUNTER_SQUARE / 2;
        if cx >= w {
            break;
        }
        let at = cy * row_bytes + cx * 4;
        // Any channel works — the squares are pure black/white.
        let sample = buf.get(at + 1).copied().unwrap_or(0);
        if sample > 128 {
            value |= 1 << bit;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_consumer_reuses_buffers_without_overwriting_held_frames() {
        let (width, height) = (640, 360);
        let mut buffers = capture_buffers(width, height);
        let addresses: Vec<_> = buffers.iter().map(|b| b.as_ptr()).collect();
        let mut held = render_next_frame(&mut buffers, width, height, 1);
        let mut held_number = 1;
        let mut pending = render_next_frame(&mut buffers, width, height, 2);
        for number in 3..150 {
            let next = render_next_frame(&mut buffers, width, height, number);
            assert!(addresses.contains(&next.as_ptr()));
            assert_eq!(decode_frame_counter_from_bgra(&held, width), held_number);
            assert_eq!(decode_frame_counter_from_bgra(&pending, width), number - 1);
            if number % 7 == 0 {
                held = pending;
                held_number = number - 1;
            }
            pending = next;
        }
    }

    /// swscale reads capture buffers in place, so each keeps FFmpeg's
    /// padding readable after its last row.
    #[test]
    fn capture_buffers_keep_padding_after_the_last_row() {
        for (width, height) in [(2733u32, 2048u32), (17, 3)] {
            for buffer in capture_buffers(width, height) {
                assert_eq!(
                    buffer.len(),
                    (width * height * 4) as usize + crate::capture::FRAME_PADDING
                );
            }
        }
    }

    #[test]
    fn frame_counter_round_trips_through_bgra() {
        let (w, h) = (640u32, 360u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for frame in [1u64, 2, 3, 42, 255, 4095, 0xFF_FFFF] {
            render_synthetic_frame(&mut buf, w, h, frame);
            assert_eq!(
                decode_frame_counter_from_bgra(&buf, w),
                frame & 0xFF_FFFF,
                "counter must round-trip for frame {frame}"
            );
        }
    }

    #[test]
    fn counter_survives_mild_blur() {
        // Simulate encoder softness: average each sampled pixel with neighbors
        // and confirm thresholding still recovers the value.
        let (w, h) = (640u32, 360u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        render_synthetic_frame(&mut buf, w, h, 0b1010_1100_0011);

        let row_bytes = (w * 4) as usize;
        let mut luma = vec![0u8; (w * h) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let g = buf[y * row_bytes + x * 4 + 1] as u16;
                let left = buf[y * row_bytes + x.saturating_sub(1) * 4 + 1] as u16;
                luma[y * w as usize + x] = ((g + left) / 2) as u8;
            }
        }
        assert_eq!(
            decode_frame_counter_from_luma(&luma, w as usize, w),
            0b1010_1100_0011
        );
    }

    #[test]
    fn synthetic_size_parses_and_bounds() {
        // Not touching the env var here (tests run in parallel) — just the default.
        assert_eq!(super::synthetic_size(), (1280, 720));
    }
}
