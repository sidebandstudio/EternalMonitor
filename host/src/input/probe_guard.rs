//! Extra containment for the opt-in Windows input hardware test.

use super::{CaptureGeometry, Injection, VirtualScreen};

pub fn permits(
    expected_pid: u32,
    foreground_pid: u32,
    bounds: CaptureGeometry,
    screen: VirtualScreen,
    mut cursor: (i32, i32),
    injections: &[Injection],
) -> bool {
    if expected_pid == 0 || expected_pid != foreground_pid {
        return false;
    }
    let inside = |(x, y): (i32, i32)| {
        x >= bounds.left
            && y >= bounds.top
            && i64::from(x) < i64::from(bounds.left) + i64::from(bounds.width)
            && i64::from(y) < i64::from(bounds.top) + i64::from(bounds.height)
    };
    for injection in injections {
        match *injection {
            Injection::MoveAbs { x, y }
            | Injection::LeftDown { x, y }
            | Injection::LeftUp { x, y }
            | Injection::RightDown { x, y }
            | Injection::RightUp { x, y }
            | Injection::MiddleDown { x, y }
            | Injection::MiddleUp { x, y } => {
                // SendInput maps 0..65535 over the complete virtual desktop.
                cursor = (
                    screen.left + (u64::from(x) * u64::from(screen.width) / 65_536) as i32,
                    screen.top + (u64::from(y) * u64::from(screen.height) / 65_536) as i32,
                );
                if !inside(cursor) {
                    return false;
                }
            }
            Injection::Wheel { .. } | Injection::HWheel { .. } => {
                if !inside(cursor) {
                    return false;
                }
            }
            // The hardware row types only Hi! and Enter. Modifiers and system
            // shortcuts are never needed while this guard is enabled.
            Injection::Unicode(72 | 105 | 33)
            | Injection::KeyDown {
                scan: 28,
                extended: false,
            }
            | Injection::KeyUp {
                scan: 28,
                extended: false,
            } => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(pid: u32, cursor: (i32, i32), batch: &[Injection]) -> bool {
        permits(
            42,
            pid,
            CaptureGeometry {
                left: 100,
                top: 100,
                width: 1720,
                height: 880,
            },
            VirtualScreen {
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            cursor,
            batch,
        )
    }

    #[test]
    fn rejects_focus_changes_outside_points_and_shortcuts() {
        assert!(!check(43, (960, 540), &[Injection::Unicode(72)]));
        assert!(!check(
            42,
            (960, 540),
            &[Injection::LeftDown { x: 0, y: 0 }]
        ));
        assert!(!check(42, (0, 0), &[Injection::Wheel { delta: 120 }]));
        assert!(!check(
            42,
            (960, 540),
            &[Injection::KeyDown {
                scan: 91,
                extended: true
            }]
        ));
        assert!(!check(42, (960, 540), &[Injection::Unicode(65)]));
    }

    #[test]
    fn accepts_contained_gestures_and_probe_text() {
        assert!(check(
            42,
            (0, 0),
            &[
                Injection::MoveAbs { x: 32768, y: 32768 },
                Injection::LeftDown { x: 32768, y: 32768 },
                Injection::LeftUp { x: 32768, y: 32768 },
                Injection::Wheel { delta: 120 },
                Injection::Unicode(72),
                Injection::Unicode(105),
                Injection::Unicode(33),
                Injection::KeyDown {
                    scan: 28,
                    extended: false
                },
                Injection::KeyUp {
                    scan: 28,
                    extended: false
                },
            ]
        ));
    }
}
