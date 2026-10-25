//! Maps the wire's normalized pointer position onto a macOS display.
//!
//! The client sends 0..=65535 on each axis across the displayed video
//! content rect, already letterbox-corrected (`InputEvent` in
//! `proto/src/v2/control.rs`). The host captures one whole display, so that
//! rect is the whole display. Two spaces describe it, and they differ:
//!
//! - **Points:** `CGDisplayBounds`, the space CGEvent locations use. Global,
//!   top-left origin, y down. Displays left of or above the main display
//!   have negative origins. Locations may be fractional.
//! - **Pixels:** the captured frame. A Retina display has 2 (or 3) pixels
//!   per point, so a display's pixel size is not its bounds' size.
//!
//! A rotated display needs no special case: `CGDisplayBounds` reports the
//! rotated size and the capture arrives as displayed, so both spaces are
//! already in the orientation the client sees.

/// A display's `CGDisplayBounds`, in global points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl DisplayBounds {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// A captured frame's size, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturePixels {
    pub width: u32,
    pub height: u32,
}

/// Position of wire value `n` along an axis of `size` units: the centre of
/// the n-th of 65,536 equal slices. It is always inside `[0, size)`.
fn slice_centre(n: u16, size: f64) -> f64 {
    (f64::from(n) + 0.5) * size / 65_536.0
}

/// Map a wire position to a global point on `bounds`, for a CGEvent.
///
/// The result stays strictly inside `[origin, origin + size)`: the far edge,
/// `origin + size`, belongs to the neighbouring display, and a pointer
/// placed on it would move to another screen. Slice centres already stop
/// half a slice short of it; the clamp keeps that true even where rounding
/// at a large origin could reach the edge.
pub fn norm_to_display_point(x_norm: u16, y_norm: u16, bounds: DisplayBounds) -> (f64, f64) {
    let inside = |origin: f64, size: f64, n: u16| {
        let last = (origin + size).next_down().max(origin);
        (origin + slice_centre(n, size)).min(last)
    };
    (
        inside(bounds.x, bounds.width, x_norm),
        inside(bounds.y, bounds.height, y_norm),
    )
}

/// The capture pixel a wire position points at, in `0..width` and
/// `0..height`. It is the pixel `norm_to_display_point`'s point falls in.
pub fn norm_to_capture_pixel(x_norm: u16, y_norm: u16, capture: CapturePixels) -> (u32, u32) {
    let pixel =
        |n: u16, size: u32| (slice_centre(n, f64::from(size)) as u32).min(size.saturating_sub(1));
    (pixel(x_norm, capture.width), pixel(y_norm, capture.height))
}

/// The capture pixel containing a global point on `bounds`, clamped to the
/// frame. Converts points to pixels with the display's own scale, which can
/// differ between displays and between axes.
pub fn point_to_capture_pixel(
    x: f64,
    y: f64,
    bounds: DisplayBounds,
    capture: CapturePixels,
) -> (u32, u32) {
    let pixel = |value: f64, origin: f64, points: f64, pixels: u32| {
        let scaled = ((value - origin) * f64::from(pixels) / points).floor();
        scaled.clamp(0.0, f64::from(pixels.saturating_sub(1))) as u32
    };
    (
        pixel(x, bounds.x, bounds.width, capture.width),
        pixel(y, bounds.y, bounds.height, capture.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// devbox1's external display: 1x, at the origin.
    const MAIN: DisplayBounds = DisplayBounds {
        x: 0.0,
        y: 0.0,
        width: 3440.0,
        height: 1440.0,
    };
    const MAIN_PIXELS: CapturePixels = CapturePixels {
        width: 3440,
        height: 1440,
    };
    /// The Duo inner screen at @2x, right of MAIN and lower.
    const RIGHT: DisplayBounds = DisplayBounds {
        x: 3440.0,
        y: 250.0,
        width: 1335.0,
        height: 939.0,
    };
    const RIGHT_PIXELS: CapturePixels = CapturePixels {
        width: 2670,
        height: 1878,
    };
    /// The Duo outer screen at @2x, left of and above MAIN.
    const LEFT_ABOVE: DisplayBounds = DisplayBounds {
        x: -1017.0,
        y: -699.0,
        width: 1017.0,
        height: 699.0,
    };
    const LEFT_ABOVE_PIXELS: CapturePixels = CapturePixels {
        width: 2034,
        height: 1398,
    };
    /// The inner screen rotated to portrait, below MAIN.
    const ROTATED: DisplayBounds = DisplayBounds {
        x: 1000.0,
        y: 1440.0,
        width: 939.0,
        height: 1335.0,
    };
    const ROTATED_PIXELS: CapturePixels = CapturePixels {
        width: 1878,
        height: 2670,
    };

    const ALL: [(DisplayBounds, CapturePixels); 4] = [
        (MAIN, MAIN_PIXELS),
        (RIGHT, RIGHT_PIXELS),
        (LEFT_ABOVE, LEFT_ABOVE_PIXELS),
        (ROTATED, ROTATED_PIXELS),
    ];

    #[test]
    fn corners_and_centre_land_inside_the_display() {
        for (bounds, _) in ALL {
            for (nx, ny) in [(0, 0), (65_535, 0), (0, 65_535), (65_535, 65_535)] {
                let (x, y) = norm_to_display_point(nx, ny, bounds);
                assert!(bounds.contains(x, y), "{bounds:?} ({nx},{ny}) -> ({x},{y})");
            }
            let (x, y) = norm_to_display_point(32_767, 32_767, bounds);
            assert!((x - (bounds.x + bounds.width / 2.0)).abs() < bounds.width / 65_536.0);
            assert!((y - (bounds.y + bounds.height / 2.0)).abs() < bounds.height / 65_536.0);
        }
    }

    #[test]
    fn the_far_edge_is_the_last_pixel_never_the_neighbour() {
        for (bounds, pixels) in ALL {
            let (x, y) = norm_to_display_point(65_535, 65_535, bounds);
            assert!(x < bounds.x + bounds.width && y < bounds.y + bounds.height);
            // Half of one 1/65536 slice short of the edge.
            let (slice_x, slice_y) = (bounds.width / 65_536.0, bounds.height / 65_536.0);
            assert!(
                bounds.x + bounds.width - x < slice_x && bounds.y + bounds.height - y < slice_y
            );
            assert_eq!(
                point_to_capture_pixel(x, y, bounds, pixels),
                (pixels.width - 1, pixels.height - 1)
            );
            assert_eq!(
                norm_to_capture_pixel(65_535, 65_535, pixels),
                (pixels.width - 1, pixels.height - 1)
            );
            let (x, y) = norm_to_display_point(0, 0, bounds);
            assert!(x - bounds.x < slice_x && y - bounds.y < slice_y);
            assert_eq!(point_to_capture_pixel(x, y, bounds, pixels), (0, 0));
        }
        // RIGHT's far x edge is exactly where a display to its right would
        // start; MAIN's far edge is RIGHT's origin.
        let (x, _) = norm_to_display_point(65_535, 0, MAIN);
        assert!(!RIGHT.contains(x, RIGHT.y));
    }

    #[test]
    fn negative_origins_never_reach_the_main_display() {
        let (x, y) = norm_to_display_point(65_535, 65_535, LEFT_ABOVE);
        assert!(x < 0.0 && y < 0.0, "({x},{y})");
        assert!(!MAIN.contains(x, y));
        let (x, y) = norm_to_display_point(0, 0, LEFT_ABOVE);
        assert!(x >= -1017.0 && y >= -699.0);
    }

    #[test]
    fn every_wire_value_is_inside_and_strictly_increasing() {
        for (bounds, _) in ALL {
            let mut previous = f64::NEG_INFINITY;
            for n in 0..=u16::MAX {
                let (x, y) = norm_to_display_point(n, n, bounds);
                assert!(bounds.contains(x, y), "{bounds:?} n={n}");
                assert!(x > previous, "{bounds:?} n={n}");
                previous = x;
            }
        }
    }

    #[test]
    fn the_injected_point_lands_in_the_pixel_the_user_touched() {
        // Mixed scales side by side: 1x MAIN next to 2x Duo screens, plus a
        // fractional 1.5x and a capture scaled differently on each axis.
        let mut cases = ALL.to_vec();
        cases.push((
            RIGHT,
            CapturePixels {
                width: 2002,
                height: 1408,
            },
        ));
        cases.push((
            LEFT_ABOVE,
            CapturePixels {
                width: 3051,
                height: 1398,
            },
        ));
        for (bounds, pixels) in cases {
            for n in 0..=u16::MAX {
                let (x, y) = norm_to_display_point(n, n, bounds);
                assert_eq!(
                    point_to_capture_pixel(x, y, bounds, pixels),
                    norm_to_capture_pixel(n, n, pixels),
                    "{bounds:?} {pixels:?} n={n}"
                );
            }
        }
    }

    #[test]
    fn every_pixel_is_reachable_at_every_scale() {
        for (_, pixels) in ALL {
            let mut hit = vec![false; pixels.width as usize];
            for n in 0..=u16::MAX {
                hit[norm_to_capture_pixel(n, 0, pixels).0 as usize] = true;
            }
            assert!(hit.iter().all(|&h| h), "{pixels:?}");
        }
    }

    #[test]
    fn a_rotated_display_maps_in_its_displayed_orientation() {
        // Top-right of the portrait video is the top-right of the portrait
        // bounds; no rotation transform is applied to input.
        let (x, y) = norm_to_display_point(65_535, 0, ROTATED);
        assert!((x - (ROTATED.x + ROTATED.width)).abs() < 0.02);
        assert!((y - ROTATED.y).abs() < 0.02);
        assert_eq!(
            point_to_capture_pixel(x, y, ROTATED, ROTATED_PIXELS),
            (ROTATED_PIXELS.width - 1, 0)
        );
    }

    #[test]
    fn fractional_origins_from_scaled_arrangements_are_honoured() {
        let bounds = DisplayBounds {
            x: -1335.5,
            y: 12.25,
            width: 1335.0,
            height: 939.0,
        };
        let (x, y) = norm_to_display_point(0, 65_535, bounds);
        assert!(bounds.contains(x, y));
        assert!(x < -1335.0);
    }

    #[test]
    fn points_outside_the_display_clamp_to_its_edge_pixels() {
        assert_eq!(
            point_to_capture_pixel(-5000.0, 1e9, RIGHT, RIGHT_PIXELS),
            (0, RIGHT_PIXELS.height - 1)
        );
    }

    #[test]
    fn full_scale_never_reaches_the_neighbour_even_far_from_the_origin() {
        // A display so far out that its 1/65536 slice is below the spacing
        // of f64 values there: the edge itself would be the next value.
        for bounds in [
            DisplayBounds {
                x: 1e15,
                y: -1e15,
                width: 0.25,
                height: 0.25,
            },
            DisplayBounds {
                x: 3440.0,
                y: 0.0,
                width: 1335.0,
                height: 939.0,
            },
        ] {
            let (x, y) = norm_to_display_point(u16::MAX, u16::MAX, bounds);
            assert!(bounds.contains(x, y), "{bounds:?} -> ({x}, {y})");
        }
    }
}
