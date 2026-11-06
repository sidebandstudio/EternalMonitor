//! Mouse cursor compositing for DXGI capture. Desktop Duplication never draws
//! the cursor into the desktop image, so capture draws it into each frame.
//! Portable so its tests run everywhere.

// DXGI pointer shape types (DXGI_OUTDUPL_POINTER_SHAPE_TYPE_*).
const DXGI_POINTER_SHAPE_MONOCHROME: u32 = 1;
const DXGI_POINTER_SHAPE_COLOR: u32 = 2;
const DXGI_POINTER_SHAPE_MASKED_COLOR: u32 = 4;

/// Caches the latest mouse-cursor shape from DXGI Desktop Duplication. The shape only
/// arrives when it changes, so it's kept across frames and composited at the per-frame
/// pointer position.
#[derive(Default)]
pub(super) struct CursorState {
    pub(super) shape: Vec<u8>,
    pub(super) shape_type: u32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pitch: u32,
    pub(super) has_shape: bool,
}

impl CursorState {
    /// The cursor to draw at `(x, y)`, if it is shown and its shape is known.
    pub(super) fn at(&self, visible: bool, x: i32, y: i32) -> Option<(&Self, i32, i32)> {
        (visible && self.has_shape).then_some((self, x, y))
    }

    /// Width and height of what [`draw_cursor`] draws, in pixels.
    fn drawn_size(&self) -> (u32, u32) {
        match self.shape_type {
            DXGI_POINTER_SHAPE_COLOR | DXGI_POINTER_SHAPE_MASKED_COLOR => (self.width, self.height),
            DXGI_POINTER_SHAPE_MONOCHROME => (self.width, self.height / 2),
            _ => (0, 0),
        }
    }
}

/// Where the cursor was drawn into a frame, and the desktop pixels it covers.
/// A pointer-only update moves the cursor in a copy of the last frame: no
/// desktop readback, which used to be the cost of every pointer frame (#183).
#[derive(Default)]
pub(super) struct DrawnCursor {
    /// Left, top, width and height in pixels, clipped to the frame.
    rect: (usize, usize, usize, usize),
    under: Vec<u8>,
}

impl DrawnCursor {
    /// Draws `cursor` at its position into `buf`, a frame of the desktop
    /// without a cursor, and keeps the pixels it covers. `None` draws none.
    pub(super) fn draw(
        &mut self,
        buf: &mut [u8],
        frame_w: u32,
        frame_h: u32,
        row_bytes: usize,
        cursor: Option<(&CursorState, i32, i32)>,
    ) {
        self.rect = (0, 0, 0, 0);
        self.under.clear();
        let Some((cursor, px, py)) = cursor else {
            return;
        };
        let (w, h) = cursor.drawn_size();
        let clip = |at: i32, len: u32, max: u32| {
            let start = at.clamp(0, max as i32);
            let end = at.saturating_add(len as i32).clamp(0, max as i32);
            (start as usize, (end - start) as usize)
        };
        let (left, width) = clip(px, w, frame_w);
        let (top, height) = clip(py, h, frame_h);
        for row in top..top + height {
            let at = row * row_bytes + left * 4;
            self.under.extend_from_slice(&buf[at..at + width * 4]);
        }
        self.rect = (left, top, width, height);
        draw_cursor(buf, frame_w, frame_h, row_bytes, cursor, px, py);
    }

    /// Takes the drawn cursor out of `buf`, a copy of the frame it was drawn into.
    pub(super) fn erase(&self, buf: &mut [u8], row_bytes: usize) {
        let (left, top, width, _) = self.rect;
        if width == 0 {
            return;
        }
        for (row, pixels) in self.under.chunks_exact(width * 4).enumerate() {
            let at = (top + row) * row_bytes + left * 4;
            buf[at..at + width * 4].copy_from_slice(pixels);
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

    /// A desktop with a different value in every byte.
    fn desktop(w: u32, h: u32) -> Vec<u8> {
        (0..w as usize * h as usize * 4 + crate::capture::FRAME_PADDING)
            .map(|i| (i * 7 % 251) as u8)
            .collect()
    }

    /// The three shape types, with see-through, blended and XOR pixels.
    fn cursors() -> [CursorState; 3] {
        let color = CursorState {
            shape: (0..24 * 20 * 4).map(|i| (i * 13 % 256) as u8).collect(),
            shape_type: DXGI_POINTER_SHAPE_COLOR,
            width: 24,
            height: 20,
            pitch: 24 * 4,
            has_shape: true,
        };
        let masked = CursorState {
            shape: (0..16 * 16 * 4)
                .map(|i| {
                    if i % 4 == 3 {
                        [0, 0xFF][i / 4 % 2]
                    } else {
                        (i * 5 % 256) as u8
                    }
                })
                .collect(),
            shape_type: DXGI_POINTER_SHAPE_MASKED_COLOR,
            width: 16,
            height: 16,
            pitch: 16 * 4,
            has_shape: true,
        };
        let mono = CursorState {
            shape: (0..32 * 4).map(|i| (i * 37 % 256) as u8).collect(),
            shape_type: DXGI_POINTER_SHAPE_MONOCHROME,
            width: 32,
            height: 32,
            pitch: 4,
            has_shape: true,
        };
        [color, masked, mono]
    }

    /// How capture drew the cursor before #183: the desktop, then the cursor.
    fn read_back(
        clean: &[u8],
        (w, h): (u32, u32),
        cursor: Option<(&CursorState, i32, i32)>,
    ) -> Vec<u8> {
        let mut frame = clean.to_vec();
        if let Some((cursor, x, y)) = cursor {
            draw_cursor(&mut frame, w, h, w as usize * 4, cursor, x, y);
        }
        frame
    }

    const SPOTS: [(i32, i32); 6] = [(10, 9), (-7, -5), (50, 40), (-30, 3), (63, 47), (500, 3)];

    #[test]
    fn a_desktop_update_is_drawn_as_before() {
        let size = (64, 48);
        let clean = desktop(size.0, size.1);
        for cursor in &cursors() {
            for (x, y) in SPOTS {
                let mut frame = clean.clone();
                let mut drawn = DrawnCursor::default();
                drawn.draw(&mut frame, size.0, size.1, 256, cursor.at(true, x, y));
                assert!(
                    frame == read_back(&clean, size, Some((cursor, x, y))),
                    "{}",
                    cursor.shape_type
                );
            }
            let mut frame = clean.clone();
            DrawnCursor::default().draw(&mut frame, size.0, size.1, 256, cursor.at(false, 10, 9));
            assert!(frame == clean);
        }
    }

    /// A pointer-only frame moves the cursor in a copy of the last frame. It
    /// must match the frame a desktop readback would have given.
    #[test]
    fn a_moved_cursor_matches_a_fresh_readback() {
        for size in [(64, 48), (63, 47)] {
            let row_bytes = size.0 as usize * 4;
            let clean = desktop(size.0, size.1);
            let cursors = cursors();
            let mut last = clean.clone();
            let mut drawn = DrawnCursor::default();
            // Moves, a hidden cursor, and shape changes in between.
            let steps = SPOTS
                .iter()
                .enumerate()
                .flat_map(|(i, &(x, y))| cursors.iter().map(move |cursor| (cursor, x, y, i != 2)))
                .collect::<Vec<_>>();
            for (cursor, x, y, visible) in steps {
                let mut frame = last.clone();
                drawn.erase(&mut frame, row_bytes);
                drawn.draw(
                    &mut frame,
                    size.0,
                    size.1,
                    row_bytes,
                    cursor.at(visible, x, y),
                );
                let expected = read_back(&clean, size, cursor.at(visible, x, y));
                assert!(
                    frame == expected,
                    "{size:?} type {} at ({x}, {y}) visible {visible}",
                    cursor.shape_type
                );
                last = frame;
            }
        }
    }
}
