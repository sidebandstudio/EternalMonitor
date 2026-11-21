//! The pointer, sent apart from the video to an iPad that draws it itself
//! (`FEATURE_DRAWS_POINTER`). Capture converts each new Windows pointer
//! image once ([`convert_shape`]) and records every position Windows
//! reports in the [`PointerFeed`]; the transport sends them at most once a
//! frame budget (`transport::pointer`). For any other client capture keeps
//! drawing the pointer into the video, as before.
//!
//! Positions stay in the captured output's pixels, where the video has
//! always drawn the pointer: Desktop Duplication reports them relative to
//! that output, so another monitor's offset never enters, and the iPad
//! scales them with the picture.

use std::sync::Arc;

use eternal_wire::v2::control::{pointer_shape_len, POINTER_MAX_SIDE, POINTER_SHAPE_INVERT};
use parking_lot::Mutex;

// DXGI_OUTDUPL_POINTER_SHAPE_TYPE_*.
pub const SHAPE_MONOCHROME: u32 = 1;
pub const SHAPE_COLOR: u32 = 2;
pub const SHAPE_MASKED_COLOR: u32 = 4;

/// A pointer image in the wire's form: premultiplied BGRA, then an invert
/// mask when any pixel inverts the picture under it (see
/// `eternal_wire::v2::control::pointer_shape_len`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerImage {
    pub source_type: u8,
    pub flags: u8,
    pub width: u16,
    pub height: u16,
    pub hot: (u16, u16),
    pub bytes: Vec<u8>,
}

/// Converts a shape as `GetFramePointerShape` returns it. `height` is the
/// buffer's: a monochrome shape stacks its AND mask over its XOR mask.
///
/// What the iPad can draw of each kind:
/// - Color: alpha-blended, as the video drew it.
/// - Monochrome: exact. AND 0 is opaque black (XOR 0) or white (XOR 1);
///   AND 1 is clear (XOR 0) or inverts the picture (XOR 1), as the I-beam
///   over text does.
/// - Masked color: mask 0 pixels are opaque. Mask 1 pixels XOR the picture
///   with their color; black changes nothing, so it is clear, and every
///   other color inverts the picture. That is exact for white, the color
///   Windows uses, and a close match otherwise.
///
/// A side past `POINTER_MAX_SIDE` keeps its top-left part. `None` for an
/// unknown kind or an empty image.
pub fn convert_shape(
    source_type: u32,
    width: u32,
    height: u32,
    pitch: u32,
    hot: (u32, u32),
    raw: &[u8],
) -> Option<PointerImage> {
    let rows = match source_type {
        SHAPE_MONOCHROME => height / 2,
        SHAPE_COLOR | SHAPE_MASKED_COLOR => height,
        _ => return None,
    };
    let max = u32::from(POINTER_MAX_SIDE);
    let (w, h) = (width.min(max), rows.min(max));
    if w == 0 || h == 0 {
        return None;
    }
    let (wu, hu, pitch) = (w as usize, h as usize, pitch as usize);
    let mut image = vec![0u8; wu * hu * 4];
    let mut mask = vec![0u8; wu.div_ceil(8) * hu];
    let mut inverts = false;
    let byte = |index: usize| raw.get(index).copied();
    for y in 0..hu {
        for x in 0..wu {
            let invert = match source_type {
                SHAPE_MONOCHROME => {
                    let (at, bit) = (x / 8, 7 - (x % 8) as u8);
                    let (Some(and), Some(xor)) =
                        (byte(y * pitch + at), byte((y + rows as usize) * pitch + at))
                    else {
                        continue;
                    };
                    match ((and >> bit) & 1, (xor >> bit) & 1) {
                        (0, xor) => {
                            let v = if xor == 1 { 255 } else { 0 };
                            image[(y * wu + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
                            false
                        }
                        (_, xor) => xor == 1,
                    }
                }
                _ => {
                    let at = y * pitch + x * 4;
                    let Some(px) = raw.get(at..at + 4) else {
                        continue;
                    };
                    let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
                    let out = &mut image[(y * wu + x) * 4..][..4];
                    if source_type == SHAPE_COLOR {
                        let premultiply = |c: u8| ((u32::from(c) * u32::from(a) + 127) / 255) as u8;
                        out.copy_from_slice(&[premultiply(b), premultiply(g), premultiply(r), a]);
                        false
                    } else if a == 0 {
                        out.copy_from_slice(&[b, g, r, 255]);
                        false
                    } else {
                        (b, g, r) != (0, 0, 0)
                    }
                }
            };
            if invert {
                mask[y * wu.div_ceil(8) + x / 8] |= 0x80 >> (x % 8);
                inverts = true;
            }
        }
    }
    let flags = if inverts { POINTER_SHAPE_INVERT } else { 0 };
    if inverts {
        image.extend_from_slice(&mask);
    }
    let (width, height) = (w as u16, h as u16);
    debug_assert_eq!(image.len(), pointer_shape_len(width, height, flags));
    Some(PointerImage {
        source_type: source_type as u8,
        flags,
        width,
        height,
        hot: (
            hot.0.min(u32::from(u16::MAX)) as u16,
            hot.1.min(u32::from(u16::MAX)) as u16,
        ),
        bytes: image,
    })
}

/// The current pointer image with the id the wire names it by.
#[derive(Debug, PartialEq, Eq)]
pub struct Shape {
    pub id: u32,
    pub image: PointerImage,
}

/// Where Windows last put the pointer, in the captured output's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub visible: bool,
    /// The image's top-left corner.
    pub x: i32,
    pub y: i32,
    pub host_time_us: u64,
}

/// The latest pointer image and position, written by capture and read by
/// the transport, which [`PointerFeed::changed`] wakes.
#[derive(Default)]
pub struct PointerFeed {
    state: Mutex<FeedState>,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct FeedState {
    shape: Option<Arc<Shape>>,
    position: Option<Position>,
    last_id: u32,
    withheld: bool,
}

impl PointerFeed {
    /// A new image gets a new id; the same image again (as a re-created
    /// duplication reports it) keeps its id, so it is not sent again.
    pub fn set_shape(&self, image: PointerImage) {
        let mut state = self.state.lock();
        if state
            .shape
            .as_ref()
            .is_some_and(|shape| shape.image == image)
        {
            return;
        }
        state.last_id = state.last_id.wrapping_add(1).max(1);
        state.shape = Some(Arc::new(Shape {
            id: state.last_id,
            image,
        }));
        drop(state);
        self.changed.notify_one();
    }

    pub fn set_position(&self, position: Position) {
        self.state.lock().position = Some(position);
        self.changed.notify_one();
    }

    /// Capture can't tell where Windows puts the pointer
    /// (`capture::PointerPlace::Unknown`): it stays in the video, and no
    /// pointer messages go to the iPad but a hidden state to clear one it
    /// was drawing.
    pub fn set_withheld(&self, withheld: bool) {
        self.state.lock().withheld = withheld;
        self.changed.notify_one();
    }

    pub fn withheld(&self) -> bool {
        self.state.lock().withheld
    }

    pub fn latest(&self) -> (Option<Arc<Shape>>, Option<Position>) {
        let state = self.state.lock();
        (state.shape.clone(), state.position)
    }

    /// Resolves after the next [`Self::set_shape`] or [`Self::set_position`],
    /// or at once when one came since the last wait.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One monochrome row per string, AND mask over XOR mask, 1 byte pitch.
    fn monochrome(and: &[u8], xor: &[u8]) -> Vec<u8> {
        [and, xor].concat()
    }

    fn pixel(image: &PointerImage, x: usize, y: usize) -> [u8; 4] {
        let at = (y * usize::from(image.width) + x) * 4;
        image.bytes[at..at + 4].try_into().unwrap()
    }

    fn inverts(image: &PointerImage, x: usize, y: usize) -> bool {
        if image.flags & POINTER_SHAPE_INVERT == 0 {
            return false;
        }
        let (w, h) = (usize::from(image.width), usize::from(image.height));
        let mask = &image.bytes[w * h * 4..];
        mask[y * w.div_ceil(8) + x / 8] & (0x80 >> (x % 8)) != 0
    }

    #[test]
    fn monochrome_draws_black_white_clear_and_inverts_with_its_xor_mask() {
        // Four pixels on one row: (AND, XOR) = (0,0) (0,1) (1,0) (1,1).
        let raw = monochrome(&[0b0011_0000], &[0b0101_0000]);
        let image = convert_shape(SHAPE_MONOCHROME, 4, 2, 1, (1, 0), &raw).unwrap();
        assert_eq!((image.width, image.height, image.hot), (4, 1, (1, 0)));
        assert_eq!(image.source_type, 1);
        assert_eq!(pixel(&image, 0, 0), [0, 0, 0, 255], "opaque black");
        assert_eq!(pixel(&image, 1, 0), [255, 255, 255, 255], "opaque white");
        assert_eq!(pixel(&image, 2, 0), [0, 0, 0, 0], "clear");
        assert_eq!(
            pixel(&image, 3, 0),
            [0, 0, 0, 0],
            "inverting pixels draw nothing"
        );
        assert_eq!(
            (0..4).map(|x| inverts(&image, x, 0)).collect::<Vec<_>>(),
            [false, false, false, true],
        );
        assert_eq!(
            image.bytes.len(),
            pointer_shape_len(4, 1, POINTER_SHAPE_INVERT)
        );
    }

    #[test]
    fn monochrome_without_inverting_pixels_sends_no_mask() {
        let raw = monochrome(&[0b0100_0000], &[0b1000_0000]);
        let image = convert_shape(SHAPE_MONOCHROME, 2, 2, 1, (0, 0), &raw).unwrap();
        assert_eq!(image.flags, 0);
        assert_eq!(image.bytes, [255, 255, 255, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn color_is_premultiplied_and_keeps_its_alpha() {
        // BGRA rows with 4 bytes of pitch padding.
        let raw = [
            200, 100, 50, 255, 200, 100, 50, 128, 0, 0, 0, 0, //
            10, 20, 30, 0, 255, 255, 255, 64, 0, 0, 0, 0,
        ];
        let image = convert_shape(SHAPE_COLOR, 2, 2, 12, (0, 0), &raw).unwrap();
        assert_eq!(image.flags, 0);
        assert_eq!(pixel(&image, 0, 0), [200, 100, 50, 255]);
        assert_eq!(pixel(&image, 1, 0), [100, 50, 25, 128]);
        assert_eq!(
            pixel(&image, 0, 1),
            [0, 0, 0, 0],
            "fully clear has no color"
        );
        assert_eq!(pixel(&image, 1, 1), [64, 64, 64, 64]);
    }

    #[test]
    fn masked_color_is_opaque_clear_or_inverting() {
        let raw = [
            10, 20, 30, 0, // mask 0: opaque color
            0, 0, 0, 0xFF, // XOR with black: no change
            255, 255, 255, 0xFF, // XOR with white: invert
            0, 0, 200, 0xFF, // XOR with red: drawn as invert
        ];
        let image = convert_shape(SHAPE_MASKED_COLOR, 4, 1, 16, (0, 0), &raw).unwrap();
        assert_eq!(image.flags, POINTER_SHAPE_INVERT);
        assert_eq!(pixel(&image, 0, 0), [10, 20, 30, 255]);
        for x in 1..4 {
            assert_eq!(pixel(&image, x, 0), [0, 0, 0, 0]);
        }
        assert_eq!(
            (0..4).map(|x| inverts(&image, x, 0)).collect::<Vec<_>>(),
            [false, false, true, true],
        );
    }

    #[test]
    fn short_buffers_unknown_kinds_and_huge_shapes_are_safe() {
        // Rows the buffer doesn't hold stay clear.
        let image = convert_shape(SHAPE_COLOR, 2, 2, 8, (0, 0), &[1, 2, 3, 255]).unwrap();
        assert_eq!(pixel(&image, 0, 0), [1, 2, 3, 255]);
        assert_eq!(pixel(&image, 1, 1), [0, 0, 0, 0]);
        assert_eq!(convert_shape(3, 2, 2, 8, (0, 0), &[0; 16]), None);
        assert_eq!(convert_shape(SHAPE_MONOCHROME, 8, 1, 1, (0, 0), &[0]), None);
        let side = u32::from(POINTER_MAX_SIDE) + 8;
        let raw = vec![0xFF; (side * 4 * side) as usize];
        let image = convert_shape(SHAPE_COLOR, side, side, side * 4, (0, 0), &raw).unwrap();
        assert_eq!(
            (image.width, image.height),
            (POINTER_MAX_SIDE, POINTER_MAX_SIDE)
        );
    }

    /// Desktop Duplication reports the pointer relative to the captured
    /// output, which is where it is drawn in the video. A tap on the drawn
    /// hotspot, normalized as the iPad does, lands on the desktop pixel
    /// Windows has the hotspot at, whatever the output's offset: a screen
    /// left of the main one, a second screen right of it, the main one.
    #[test]
    fn a_tap_on_the_drawn_hotspot_lands_where_windows_has_it() {
        use crate::input::{norm_to_output_pixel, CaptureGeometry};
        let outputs = [
            CaptureGeometry {
                left: -1920,
                top: 120,
                width: 1920,
                height: 1080,
            },
            CaptureGeometry {
                left: 3440,
                top: 0,
                width: 2388,
                height: 1668,
            },
            CaptureGeometry {
                left: 0,
                top: 0,
                width: 3440,
                height: 1440,
            },
        ];
        let hot = (6, 9);
        for output in outputs {
            for (x, y) in [
                (0, 0),
                (1003, 517),
                (output.width as i32 - 7, output.height as i32 - 10),
            ] {
                // Windows: the hotspot at this desktop pixel.
                let desktop = (output.left + x + hot.0, output.top + y + hot.1);
                // Duplication: the image's corner, relative to the output.
                let drawn = Position {
                    visible: true,
                    x,
                    y,
                    host_time_us: 0,
                };
                // The iPad: the centre of the hotspot's video pixel.
                let (w, h) = (output.width as f64, output.height as f64);
                let norm = |p: i32, n: f64| (((f64::from(p) + 0.5) / n) * 65535.0).round() as u16;
                let tapped = norm_to_output_pixel(
                    norm(drawn.x + hot.0, w),
                    norm(drawn.y + hot.1, h),
                    output,
                );
                // Within the input mapping's own rounding: it spreads the
                // video's pixels over one step fewer.
                assert!(
                    (tapped.0 - desktop.0).abs() <= 1 && (tapped.1 - desktop.1).abs() <= 1,
                    "{output:?} at {x},{y}: tapped {tapped:?}, Windows {desktop:?}"
                );
            }
        }
    }

    #[test]
    fn the_feed_names_a_new_image_once_and_keeps_the_latest_position() {
        let feed = PointerFeed::default();
        let image = |v: u8| PointerImage {
            source_type: 2,
            flags: 0,
            width: 1,
            height: 1,
            hot: (0, 0),
            bytes: vec![v; 4],
        };
        feed.set_shape(image(1));
        feed.set_shape(image(1));
        assert_eq!(
            feed.latest().0.unwrap().id,
            1,
            "the same image keeps its id"
        );
        feed.set_shape(image(2));
        assert_eq!(feed.latest().0.unwrap().id, 2);
        for x in 0..3 {
            feed.set_position(Position {
                visible: true,
                x,
                y: 0,
                host_time_us: 0,
            });
        }
        assert_eq!(feed.latest().1.unwrap().x, 2);
    }
}
