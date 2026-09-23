//! Input relay: turns the client's INPUT_EVENT messages (normalized
//! coordinates over the displayed video) into Windows input injection.
//!
//! Everything in this file is portable, pure, and unit-tested: coordinate
//! mapping, event deduplication, and wheel scaling. The actual `SendInput`
//! calls live in `windows.rs`; other platforms record events for the
//! end-to-end tests instead.

use eternal_wire::v2::control::InputEvent;
mod hid;
pub use hid::hid_to_scancode;

#[cfg(windows)]
pub mod windows_inject;

#[cfg(any(windows, test))]
mod probe_guard;

/// Desktop-space rectangle of the captured output (from DXGI's
/// `DesktopCoordinates`) — the target space for absolute pointer mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureGeometry {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// The whole Windows virtual screen (all monitors' bounding box); absolute
/// `SendInput` coordinates are normalized 0..65535 over THIS rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualScreen {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// Input kinds on the wire (`InputEvent.kind`).
pub const KIND_TOUCH: u8 = 0;
pub const KIND_PENCIL: u8 = 1;
pub const KIND_MOUSE_ABS: u8 = 2;
pub const KIND_SCROLL: u8 = 3;
pub const KIND_KEY: u8 = 4;
pub const KIND_TEXT: u8 = 5;
pub const KIND_HOVER: u8 = 6;

/// Phases (`InputEvent.phase`).
pub const PHASE_BEGAN: u8 = 0;
pub const PHASE_MOVED: u8 = 1;
pub const PHASE_ENDED: u8 = 2;
pub const PHASE_CANCELLED: u8 = 3;

/// A fully-resolved injection command, ready for `SendInput` (or a test
/// recorder off Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injection {
    KeyDown {
        scan: u16,
        extended: bool,
    },
    KeyUp {
        scan: u16,
        extended: bool,
    },
    Unicode(u16),
    /// Absolute pointer move in virtual-screen normalized space (0..65535).
    MoveAbs {
        x: u16,
        y: u16,
    },
    LeftDown {
        x: u16,
        y: u16,
    },
    LeftUp {
        x: u16,
        y: u16,
    },
    RightDown {
        x: u16,
        y: u16,
    },
    RightUp {
        x: u16,
        y: u16,
    },
    MiddleDown {
        x: u16,
        y: u16,
    },
    MiddleUp {
        x: u16,
        y: u16,
    },
    /// Vertical wheel in WHEEL_DELTA units (positive = away from user).
    Wheel {
        delta: i32,
    },
    /// Horizontal wheel.
    HWheel {
        delta: i32,
    },
}

/// Map a wire-normalized point (0..65535 over the displayed video, which the
/// client letterbox-corrects before sending) to a desktop pixel on the
/// captured output.
pub fn norm_to_output_pixel(x_norm: u16, y_norm: u16, output: CaptureGeometry) -> (i32, i32) {
    let x =
        output.left + ((u32::from(x_norm) * output.width.saturating_sub(1).max(1)) / 65_535) as i32;
    let y =
        output.top + ((u32::from(y_norm) * output.height.saturating_sub(1).max(1)) / 65_535) as i32;
    (x, y)
}

/// Map a desktop pixel to `SendInput`'s absolute virtual-screen space
/// (0..65535 across the whole virtual desktop, MOUSEEVENTF_VIRTUALDESK).
pub fn desktop_pixel_to_abs(x: i32, y: i32, screen: VirtualScreen) -> (u16, u16) {
    let clamp_span = |value: i32, origin: i32, span: u32| -> u16 {
        let span = span.max(1) as i64;
        let rel = (i64::from(value) - i64::from(origin)).clamp(0, span - 1);
        ((rel * 65_535) / (span - 1).max(1)) as u16
    };
    (
        clamp_span(x, screen.left, screen.width),
        clamp_span(y, screen.top, screen.height),
    )
}

/// One pixel of client scroll → wheel delta. Apple reports pixel-ish deltas;
/// Windows wheels tick in 120s. 120/40 ≈ three-pixels-per-tick feels natural.
pub fn scroll_to_wheel(delta_px: i16) -> i32 {
    i32::from(delta_px) * 3
}

/// Number of recent edge ids remembered. The client sends each press/release
/// twice for loss tolerance, and Wi-Fi aggregation reorders freely, so the two
/// copies of one edge routinely arrive with other edges between them. A single
/// remembered id could not survive that: began(N), ended(N+1), began(N) walked
/// the slot N -> N+1 -> N and let the replayed press through, injecting a
/// double click where the user tapped once.
const RECENT_EDGE_IDS: usize = 8;

/// Drops duplicate began/ended events while letting distinct events through.
#[derive(Debug, Default)]
pub struct EventDeduper {
    recent: [Option<u32>; RECENT_EDGE_IDS],
    next: usize,
}

impl EventDeduper {
    /// True if the event should be processed.
    pub fn accept(&mut self, event: &InputEvent) -> bool {
        match event.phase {
            PHASE_BEGAN | PHASE_ENDED | PHASE_CANCELLED => {
                if self.recent.contains(&Some(event.event_id)) {
                    return false;
                }
                self.recent[self.next] = Some(event.event_id);
                self.next = (self.next + 1) % RECENT_EDGE_IDS;
                true
            }
            // Moves are idempotent-ish; duplicates are harmless.
            _ => true,
        }
    }
}

/// Resolve one wire event into zero or more injections.
pub fn resolve(
    event: &InputEvent,
    output: CaptureGeometry,
    screen: VirtualScreen,
) -> Vec<Injection> {
    if event.input_ver != 1 {
        return Vec::new();
    }
    let (px, py) = norm_to_output_pixel(event.x_norm, event.y_norm, output);
    let (ax, ay) = desktop_pixel_to_abs(px, py, screen);

    match event.kind {
        KIND_TOUCH | KIND_PENCIL | KIND_MOUSE_ABS => {
            let right_button = event.buttons & 0b10 != 0;
            let middle_button = event.buttons & 0b100 != 0;
            match event.phase {
                PHASE_BEGAN => {
                    if middle_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::MiddleDown { x: ax, y: ay },
                        ]
                    } else if right_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::RightDown { x: ax, y: ay },
                        ]
                    } else {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::LeftDown { x: ax, y: ay },
                        ]
                    }
                }
                PHASE_MOVED => vec![Injection::MoveAbs { x: ax, y: ay }],
                PHASE_ENDED | PHASE_CANCELLED => {
                    if middle_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::MiddleUp { x: ax, y: ay },
                        ]
                    } else if right_button {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::RightUp { x: ax, y: ay },
                        ]
                    } else {
                        vec![
                            Injection::MoveAbs { x: ax, y: ay },
                            Injection::LeftUp { x: ax, y: ay },
                        ]
                    }
                }
                _ => Vec::new(),
            }
        }
        KIND_HOVER if event.phase == PHASE_MOVED => vec![Injection::MoveAbs { x: ax, y: ay }],
        KIND_KEY => {
            let Some((scan, extended)) = hid_to_scancode(event.keycode) else {
                return Vec::new();
            };
            match event.phase {
                PHASE_BEGAN => vec![Injection::KeyDown { scan, extended }],
                PHASE_ENDED | PHASE_CANCELLED => vec![Injection::KeyUp { scan, extended }],
                _ => Vec::new(),
            }
        }
        KIND_TEXT if event.phase == PHASE_BEGAN => vec![Injection::Unicode(event.keycode)],
        KIND_SCROLL => {
            let mut injections = vec![Injection::MoveAbs { x: ax, y: ay }];
            if event.scroll_dy != 0 {
                injections.push(Injection::Wheel {
                    delta: scroll_to_wheel(event.scroll_dy),
                });
            }
            if event.scroll_dx != 0 {
                injections.push(Injection::HWheel {
                    delta: scroll_to_wheel(event.scroll_dx),
                });
            }
            injections
        }
        _ => Vec::new(),
    }
}

/// Off-Windows sink: records what WOULD be injected so the end-to-end tests
/// can assert the full wire→mapping path. Windows injects for real.
#[cfg(not(windows))]
pub mod recorder {
    use super::Injection;
    use std::sync::Mutex;

    static RECORDED: Mutex<Vec<Injection>> = Mutex::new(Vec::new());

    pub fn record(injections: &[Injection]) {
        RECORDED.lock().unwrap().extend_from_slice(injections);
    }

    pub fn take() -> Vec<Injection> {
        std::mem::take(&mut *RECORDED.lock().unwrap())
    }

    pub fn peek() -> Vec<Injection> {
        RECORDED.lock().unwrap().clone()
    }
}

/// Execute injections on this platform.
pub fn inject(injections: &[Injection]) {
    #[cfg(windows)]
    windows_inject::inject(injections);
    #[cfg(not(windows))]
    {
        recorder::record(injections);
        log_injections(injections);
    }
}

fn log_injections(injections: &[Injection]) {
    if std::env::var("ETERNAL_INPUT_RECORDER_LOG").is_ok_and(|value| value == "1") {
        for injection in injections {
            tracing::info!(command = ?injection, "Input injection");
        }
    }
}

/// The transport's per-connection relay state: an edge deduper scoped to the
/// current session (a superseded session restarts its `event_id` counter, so
/// dedupe state must not leak across sessions).
#[derive(Debug, Default)]
pub struct InputRelay {
    session: Option<(u32, EventDeduper)>,
    held: HeldInputs,
}

#[derive(Debug, Default)]
struct HeldInputs {
    keys: std::collections::BTreeSet<(u16, bool)>,
    buttons: u8,
    pointer: (u16, u16),
}

impl HeldInputs {
    fn record(&mut self, injections: &[Injection]) {
        for injection in injections {
            match *injection {
                Injection::KeyDown { scan, extended } => {
                    self.keys.insert((scan, extended));
                }
                Injection::KeyUp { scan, extended } => {
                    self.keys.remove(&(scan, extended));
                }
                Injection::MoveAbs { x, y } => self.pointer = (x, y),
                Injection::LeftDown { .. } => self.buttons |= 1,
                Injection::LeftUp { .. } => self.buttons &= !1,
                Injection::RightDown { .. } => self.buttons |= 2,
                Injection::RightUp { .. } => self.buttons &= !2,
                Injection::MiddleDown { .. } => self.buttons |= 4,
                Injection::MiddleUp { .. } => self.buttons &= !4,
                _ => {}
            }
        }
    }

    fn release(&mut self) -> Vec<Injection> {
        let mut releases: Vec<_> = std::mem::take(&mut self.keys)
            .into_iter()
            .map(|(scan, extended)| Injection::KeyUp { scan, extended })
            .collect();
        let (x, y) = self.pointer;
        if self.buttons & 1 != 0 {
            releases.push(Injection::LeftUp { x, y });
        }
        if self.buttons & 2 != 0 {
            releases.push(Injection::RightUp { x, y });
        }
        if self.buttons & 4 != 0 {
            releases.push(Injection::MiddleUp { x, y });
        }
        self.buttons = 0;
        releases
    }
}

impl InputRelay {
    /// Release keys/buttons when a session ends, expires, or is superseded.
    pub fn reset(&mut self) {
        inject(&self.held.release());
        self.session = None;
    }

    /// Dedupe, map, and inject one session-validated event.
    pub fn relay(&mut self, session_id: u32, event: &InputEvent, output: CaptureGeometry) {
        match &self.session {
            Some((id, _)) if *id == session_id => {}
            _ => {
                self.reset();
                self.session = Some((session_id, EventDeduper::default()));
            }
        }
        let deduper = &mut self.session.as_mut().expect("just ensured").1;
        if !deduper.accept(event) {
            return;
        }
        let screen = current_virtual_screen(output);
        let injections = resolve(event, output, screen);
        self.held.record(&injections);
        inject(&injections);
    }
}

impl Drop for InputRelay {
    fn drop(&mut self) {
        self.reset();
    }
}

/// The absolute space injections land in. Windows asks the OS for the live
/// multi-monitor bounding box; other platforms (the test recorder) treat the
/// captured output as the whole screen.
fn current_virtual_screen(output: CaptureGeometry) -> VirtualScreen {
    #[cfg(windows)]
    {
        let _ = output;
        windows_inject::virtual_screen()
    }
    #[cfg(not(windows))]
    VirtualScreen {
        left: output.left,
        top: output.top,
        width: output.width,
        height: output.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u8, phase: u8, x: u16, y: u16) -> InputEvent {
        InputEvent {
            input_ver: 1,
            kind,
            phase,
            buttons: 1,
            event_id: 1,
            x_norm: x,
            y_norm: y,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: 0,
            modifiers: 0,
            client_time_us: 0,
        }
    }

    const OUTPUT: CaptureGeometry = CaptureGeometry {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };
    const SINGLE_SCREEN: VirtualScreen = VirtualScreen {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn corners_and_center_map_correctly() {
        assert_eq!(norm_to_output_pixel(0, 0, OUTPUT), (0, 0));
        assert_eq!(norm_to_output_pixel(65_535, 65_535, OUTPUT), (1919, 1079));
        let (cx, cy) = norm_to_output_pixel(32_768, 32_768, OUTPUT);
        assert!((cx - 960).abs() <= 1, "center x {cx}");
        assert!((cy - 540).abs() <= 1, "center y {cy}");
    }

    #[test]
    fn extended_display_offset_is_applied() {
        // Virtual display sits LEFT of the primary at -1920.
        let extended = CaptureGeometry {
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        let screen = VirtualScreen {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        };
        // Touch at the extended display's center lands in the LEFT half of the
        // virtual screen.
        let (px, py) = norm_to_output_pixel(32_768, 32_768, extended);
        assert!((px - -960).abs() <= 1, "pixel x {px}");
        let (ax, _ay) = desktop_pixel_to_abs(px, py, screen);
        // The left display's center = 25% of the way across the dual-monitor
        // virtual screen (one pixel ≈ 17 abs units; allow ±2 px of rounding).
        assert!(
            (i32::from(ax) - 16_384).abs() <= 40,
            "abs x {ax} should be ~16384"
        );
    }

    #[test]
    fn tap_produces_move_then_click_edges() {
        let began = resolve(
            &event(KIND_TOUCH, PHASE_BEGAN, 100, 100),
            OUTPUT,
            SINGLE_SCREEN,
        );
        assert!(matches!(began[0], Injection::MoveAbs { .. }));
        assert!(matches!(began[1], Injection::LeftDown { .. }));

        let ended = resolve(
            &event(KIND_TOUCH, PHASE_ENDED, 100, 100),
            OUTPUT,
            SINGLE_SCREEN,
        );
        assert!(matches!(ended[1], Injection::LeftUp { .. }));
    }

    #[test]
    fn right_button_flag_makes_right_clicks() {
        let mut e = event(KIND_TOUCH, PHASE_BEGAN, 5, 5);
        e.buttons = 0b10;
        let injections = resolve(&e, OUTPUT, SINGLE_SCREEN);
        assert!(matches!(injections[1], Injection::RightDown { .. }));
    }

    #[test]
    fn scroll_maps_to_wheel_deltas() {
        let mut e = event(KIND_SCROLL, PHASE_MOVED, 0, 0);
        e.scroll_dy = -40;
        e.scroll_dx = 10;
        let injections = resolve(&e, OUTPUT, SINGLE_SCREEN);
        assert!(injections.contains(&Injection::Wheel { delta: -120 }));
        assert!(injections.contains(&Injection::HWheel { delta: 30 }));
    }

    #[cfg(not(windows))]
    #[test]
    fn relay_resets_dedupe_on_session_change() {
        let mut relay = InputRelay::default();
        let mut e = event(KIND_TOUCH, PHASE_BEGAN, 10, 10);
        e.event_id = 1;
        relay.relay(7, &e, OUTPUT); // injected (MoveAbs + LeftDown)
        relay.relay(7, &e, OUTPUT); // duplicate edge: dropped
        relay.relay(8, &e, OUTPUT); // new session, same event_id: injected
        let recorded = recorder::take();
        assert_eq!(recorded.len(), 5);
        assert!(matches!(recorded[2], Injection::LeftUp { .. }));
        // Empty the release that Drop correctly injects for session 8 too.
        drop(relay);
        assert!(matches!(
            recorder::take().as_slice(),
            [Injection::LeftUp { .. }]
        ));
    }

    #[test]
    fn reordered_duplicate_edges_still_dedupe() {
        // The wire sends each edge twice; the network may interleave them.
        // began(7), ended(8), began(7), ended(8) must inject one press and
        // one release, not two of each.
        let mut dedupe = EventDeduper::default();
        let mut began = event(KIND_TOUCH, PHASE_BEGAN, 0, 0);
        began.event_id = 7;
        let mut ended = event(KIND_TOUCH, PHASE_ENDED, 0, 0);
        ended.event_id = 8;

        assert!(dedupe.accept(&began));
        assert!(dedupe.accept(&ended));
        assert!(
            !dedupe.accept(&began),
            "the replayed press must not click again"
        );
        assert!(!dedupe.accept(&ended));
    }

    #[test]
    fn edge_duplicates_are_dropped_moves_pass() {
        let mut dedupe = EventDeduper::default();
        let mut began = event(KIND_TOUCH, PHASE_BEGAN, 0, 0);
        began.event_id = 7;
        assert!(dedupe.accept(&began));
        assert!(!dedupe.accept(&began), "second edge with the same id drops");

        let mut moved = event(KIND_TOUCH, PHASE_MOVED, 1, 1);
        moved.event_id = 7;
        assert!(dedupe.accept(&moved));
        assert!(dedupe.accept(&moved), "moves are not deduped");

        let mut ended = event(KIND_TOUCH, PHASE_ENDED, 2, 2);
        ended.event_id = 8;
        assert!(dedupe.accept(&ended));
    }

    #[test]
    fn keyboard_text_hover_and_middle_resolve_without_extra_clicks() {
        let mut e = event(KIND_KEY, PHASE_BEGAN, 0, 0);
        e.keycode = 0x4f;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::KeyDown {
                scan: 0x4d,
                extended: true
            }]
        );
        e.phase = PHASE_CANCELLED;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::KeyUp {
                scan: 0x4d,
                extended: true
            }]
        );
        e.keycode = 0xffff;
        assert!(resolve(&e, OUTPUT, SINGLE_SCREEN).is_empty());
        e.kind = KIND_TEXT;
        e.phase = PHASE_BEGAN;
        e.keycode = 0xd83d;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::Unicode(0xd83d)]
        );
        e.phase = PHASE_ENDED;
        assert!(resolve(&e, OUTPUT, SINGLE_SCREEN).is_empty());
        e.kind = KIND_HOVER;
        e.phase = PHASE_MOVED;
        e.buttons = 7;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN),
            vec![Injection::MoveAbs { x: 0, y: 0 }]
        );
        e.kind = KIND_MOUSE_ABS;
        e.buttons = 4;
        e.phase = PHASE_BEGAN;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN)[1],
            Injection::MiddleDown { x: 0, y: 0 }
        );
        e.phase = PHASE_ENDED;
        assert_eq!(
            resolve(&e, OUTPUT, SINGLE_SCREEN)[1],
            Injection::MiddleUp { x: 0, y: 0 }
        );
        e.kind = KIND_KEY;
        e.event_id = 99;
        let mut deduper = EventDeduper::default();
        assert!(deduper.accept(&e));
        assert!(!deduper.accept(&e));
    }

    #[test]
    fn held_inputs_release_once_after_disconnect() {
        let mut held = HeldInputs::default();
        held.record(&[
            Injection::KeyDown {
                scan: 0x1d,
                extended: false,
            },
            Injection::KeyDown {
                scan: 0x1d,
                extended: false,
            },
            Injection::KeyDown {
                scan: 0x2e,
                extended: false,
            },
            Injection::KeyUp {
                scan: 0x2e,
                extended: false,
            },
            Injection::MoveAbs { x: 123, y: 456 },
            Injection::MiddleDown { x: 123, y: 456 },
        ]);
        assert_eq!(
            held.release(),
            vec![
                Injection::KeyUp {
                    scan: 0x1d,
                    extended: false
                },
                Injection::MiddleUp { x: 123, y: 456 }
            ]
        );
        assert!(held.release().is_empty());
    }
}
