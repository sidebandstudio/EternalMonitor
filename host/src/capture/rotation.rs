//! A screen turned in Windows' display settings (Portrait, Landscape
//! (flipped), Portrait (flipped)). Desktop Duplication gives its image the
//! way the display scans it out, with `DXGI_OUTDUPL_DESC.Rotation` saying
//! how the desktop is turned on it. Its `ModeDesc` is the desktop's size,
//! upright: on a 1920x1080 monitor turned to Portrait it reads 1080x1920,
//! rotation 90 degrees, and the image is 1920x1080 (measured on an RTX 5080,
//! November 2026). Capture sized its copy from `ModeDesc`, which on a
//! quarter turn copies nothing: no picture. The image is turned upright while it is
//! read back, in the copy capture makes anyway, so the iPad gets the desktop
//! as Windows lays it out. The pointer's position, Windows' desktop
//! coordinates and GDI's picture of the screen are upright already.
//!
//! The mapping is the one Microsoft's Desktop Duplication sample uses to
//! draw a rotated output's dirty rectangles onto the desktop
//! (`DISPLAYMANAGER::SetDirtyVert`), for an upright image `W` by `H`:
//! 90 degrees puts the image's (x, y) at (W - 1 - y, x), 180 at
//! (W - 1 - x, H - 1 - y), 270 at (y, H - 1 - x).

/// How the desktop is turned on the image Desktop Duplication gives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Rotation {
    /// Upright: `DXGI_MODE_ROTATION_IDENTITY`, or `UNSPECIFIED`.
    #[default]
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
}

/// Square tiles for the quarter-turn copy, so reads and writes both stay
/// within a few cache lines at a time.
const TILE: usize = 64;

impl Rotation {
    /// From a `DXGI_MODE_ROTATION` value: 2, 3 and 4 are 90, 180 and 270
    /// degrees; 1 (identity), 0 (unspecified) and anything else upright.
    pub fn from_dxgi(value: i32) -> Self {
        match value {
            2 => Self::Rotate90,
            3 => Self::Rotate180,
            4 => Self::Rotate270,
            _ => Self::Identity,
        }
    }

    /// The upright size of an image `size` as Desktop Duplication gives it:
    /// the sides swap for a quarter turn.
    pub fn upright(self, (width, height): (u32, u32)) -> (u32, u32) {
        match self {
            Self::Rotate90 | Self::Rotate270 => (height, width),
            Self::Identity | Self::Rotate180 => (width, height),
        }
    }

    /// The size of the image Desktop Duplication gives for an `upright`
    /// desktop, as measured: the sides swap for a quarter turn. Capture
    /// starts its staging texture at this size and takes the acquired
    /// image's own if that differs.
    pub fn duplicated(self, upright: (u32, u32)) -> (u32, u32) {
        self.upright(upright)
    }

    /// Where the upright pixel (`x`, `y`) of an image whose duplicated size
    /// is `source` lies in the duplicated image.
    fn source_of(self, (x, y): (usize, usize), (width, height): (usize, usize)) -> (usize, usize) {
        match self {
            Self::Identity => (x, y),
            // Upright width is the source's height.
            Self::Rotate90 => (y, height - 1 - x),
            Self::Rotate180 => (width - 1 - x, height - 1 - y),
            // Upright height is the source's width.
            Self::Rotate270 => (width - 1 - y, x),
        }
    }
}

/// Copies a duplicated BGRA image of `size` pixels into `target` upright
/// ([`Rotation::upright`]), with rows `target_row_bytes` apart.
/// `source_row(y)` is the duplicated image's row `y`, at least `size.0`
/// pixels: rows are read one at a time, never across the padding a mapped
/// texture's pitch may leave between them. Upright, each row is one copy,
/// as before rotation was handled.
pub fn copy_upright<'a>(
    rotation: Rotation,
    source_row: impl Fn(usize) -> &'a [u8],
    size: (u32, u32),
    target: &mut [u8],
    target_row_bytes: usize,
) {
    let (width, height) = (size.0 as usize, size.1 as usize);
    if width == 0 || height == 0 {
        return;
    }
    let (out_width, out_height) = {
        let (w, h) = rotation.upright(size);
        (w as usize, h as usize)
    };
    assert!(
        target_row_bytes >= out_width * 4
            && target.len() >= target_row_bytes * (out_height - 1) + out_width * 4
    );
    if rotation == Rotation::Identity {
        for row in 0..height {
            target[row * target_row_bytes..][..width * 4]
                .copy_from_slice(&source_row(row)[..width * 4]);
        }
        return;
    }
    for tile_y in (0..out_height).step_by(TILE) {
        for tile_x in (0..out_width).step_by(TILE) {
            for y in tile_y..(tile_y + TILE).min(out_height) {
                let row = &mut target[y * target_row_bytes..][..out_width * 4];
                for x in tile_x..(tile_x + TILE).min(out_width) {
                    let (sx, sy) = rotation.source_of((x, y), (width, height));
                    row[x * 4..x * 4 + 4].copy_from_slice(&source_row(sy)[sx * 4..sx * 4 + 4]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duplicated image whose pixel at (x, y) holds x and y, rows padded
    /// as a mapped texture's pitch may be.
    fn numbered(width: usize, height: usize, pitch: usize) -> Vec<u8> {
        let mut image = vec![0xEE; pitch * height];
        for y in 0..height {
            for x in 0..width {
                image[y * pitch + x * 4..][..4].copy_from_slice(&[x as u8, y as u8, 7, 255]);
            }
        }
        image
    }

    fn at(image: &[u8], row_bytes: usize, (x, y): (usize, usize)) -> (u8, u8) {
        (
            image[y * row_bytes + x * 4],
            image[y * row_bytes + x * 4 + 1],
        )
    }

    #[test]
    fn dxgi_values_map_to_turns() {
        assert_eq!(Rotation::from_dxgi(0), Rotation::Identity, "unspecified");
        assert_eq!(Rotation::from_dxgi(1), Rotation::Identity);
        assert_eq!(Rotation::from_dxgi(2), Rotation::Rotate90);
        assert_eq!(Rotation::from_dxgi(3), Rotation::Rotate180);
        assert_eq!(Rotation::from_dxgi(4), Rotation::Rotate270);
        assert_eq!(Rotation::from_dxgi(9), Rotation::Identity);
        assert_eq!(Rotation::Rotate90.upright((2360, 1640)), (1640, 2360));
        assert_eq!(Rotation::Rotate270.upright((1920, 1080)), (1080, 1920));
        assert_eq!(Rotation::Rotate180.upright((1920, 1080)), (1920, 1080));
        assert_eq!(Rotation::Identity.upright((1920, 1080)), (1920, 1080));
        // ModeDesc of a 1920x1080 monitor turned to Portrait, and its image.
        assert_eq!(Rotation::Rotate90.duplicated((1080, 1920)), (1920, 1080));
        assert_eq!(Rotation::Rotate180.duplicated((1920, 1080)), (1920, 1080));
    }

    /// Each of the four turns, at sizes not a multiple of the tile, puts
    /// every pixel where Microsoft's sample draws it, its corners named
    /// here, and never writes past a row's pixels.
    #[test]
    fn each_rotation_turns_the_image_upright() {
        let (width, height, pitch) = (150usize, 70usize, 150 * 4 + 24);
        let source = numbered(width, height, pitch);
        // The duplicated image's top-left (0, 0), top-right (149, 0) and
        // bottom-left (0, 69) corners, where each lands upright.
        let cases = [
            (Rotation::Identity, (150, 70), [(0, 0), (149, 0), (0, 69)]),
            (Rotation::Rotate90, (70, 150), [(69, 0), (69, 149), (0, 0)]),
            (
                Rotation::Rotate180,
                (150, 70),
                [(149, 69), (0, 69), (149, 0)],
            ),
            (
                Rotation::Rotate270,
                (70, 150),
                [(0, 149), (0, 0), (69, 149)],
            ),
        ];
        for (rotation, (out_w, out_h), corners) in cases {
            assert_eq!(
                rotation.upright((width as u32, height as u32)),
                (out_w, out_h)
            );
            let row_bytes = out_w as usize * 4 + 8;
            let mut target = vec![0xAB; row_bytes * out_h as usize];
            copy_upright(
                rotation,
                |y| &source[y * pitch..][..width * 4],
                (width as u32, height as u32),
                &mut target,
                row_bytes,
            );
            for (corner, upright) in [(0, 0), (149, 0), (0, 69)].into_iter().zip(corners) {
                assert_eq!(
                    at(&target, row_bytes, upright),
                    (corner.0 as u8, corner.1 as u8),
                    "{rotation:?}: source {corner:?} upright at {upright:?}"
                );
            }
            // Every pixel came from the one the mapping names, and is opaque.
            for y in 0..out_h as usize {
                for x in 0..out_w as usize {
                    let (sx, sy) = rotation.source_of((x, y), (width, height));
                    assert_eq!(at(&target, row_bytes, (x, y)), (sx as u8, sy as u8));
                    assert_eq!(target[y * row_bytes + x * 4 + 3], 255);
                }
                assert!(
                    target[y * row_bytes + out_w as usize * 4..][..8]
                        .iter()
                        .all(|&b| b == 0xAB),
                    "{rotation:?}: row {y} padding untouched"
                );
            }
        }
    }

    /// The four turns are each other's inverses: 90 then 270 (and 180
    /// twice) gives the image back.
    #[test]
    fn opposite_turns_give_the_image_back() {
        let (width, height) = (66usize, 130usize);
        let source = numbered(width, height, width * 4);
        for (first, second) in [
            (Rotation::Rotate90, Rotation::Rotate270),
            (Rotation::Rotate270, Rotation::Rotate90),
            (Rotation::Rotate180, Rotation::Rotate180),
        ] {
            let (w, h) = first.upright((width as u32, height as u32));
            let mut once = vec![0; w as usize * 4 * h as usize];
            copy_upright(
                first,
                |y| &source[y * width * 4..][..width * 4],
                (width as u32, height as u32),
                &mut once,
                w as usize * 4,
            );
            // Turning the upright image by the inverse of a turn of its own.
            let mut back = vec![0; width * 4 * height];
            let turned_row = w as usize * 4;
            copy_upright(
                second,
                |y| &once[y * turned_row..][..turned_row],
                (w, h),
                &mut back,
                width * 4,
            );
            assert_eq!(back, source, "{first:?} then {second:?}");
        }
    }
}
