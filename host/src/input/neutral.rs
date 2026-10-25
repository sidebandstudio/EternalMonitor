//! Platform-neutral input: what the user did, before any platform's units.
//!
//! `resolve_intents` reads one wire event and says what happened: a pointer
//! moved or a button changed at a wire position, a wheel turned, a HID key
//! went down, a UTF-16 unit was typed, or the Pencil sampled. Positions stay
//! in the wire's 0..=65535 space and keys stay HID usages. Each platform
//! converts these to its own units: `super::resolve` to `SendInput` and
//! scan codes on Windows, and `super::mac` to CGEvents on macOS.

use eternal_wire::v2::control::InputEvent;

use super::{
    PenPhase, KIND_HOVER, KIND_KEY, KIND_MOUSE_ABS, KIND_PENCIL, KIND_SCROLL, KIND_TEXT,
    KIND_TOUCH, PHASE_BEGAN, PHASE_CANCELLED, PHASE_ENDED, PHASE_MOVED,
};

/// A position on the wire: 0..=65535 on each axis across the displayed
/// video, already letterbox-corrected by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// One Pencil sample, in wire units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PenIntent {
    pub phase: PenPhase,
    pub at: NormPoint,
    /// 0..=1000; zero unless the tip is down (`Down` or `Move`).
    pub pressure_x1000: u16,
    /// Degrees, clamped to ±90.
    pub tilt_x: i16,
    pub tilt_y: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputIntent {
    PointerMove {
        at: NormPoint,
    },
    ButtonDown {
        button: Button,
        at: NormPoint,
    },
    ButtonUp {
        button: Button,
        at: NormPoint,
    },
    /// The client's pixel deltas, positive dy away from the user, with the
    /// pointer position the wheel turned at.
    Scroll {
        at: NormPoint,
        dx: i16,
        dy: i16,
    },
    /// A USB HID keyboard-page usage (`input/hid.rs`).
    Key {
        usage: u16,
        down: bool,
    },
    /// One UTF-16 code unit; an emoji arrives as two.
    Text(u16),
    Pen(PenIntent),
}

/// Resolve one wire event into what the user did. Unknown versions, kinds
/// and phases resolve to nothing, as do keys any HID table would reject
/// later: the platform converter decides which usages it supports.
pub fn resolve_intents(event: &InputEvent) -> Vec<InputIntent> {
    if event.input_ver != 1 && !(event.input_ver == 2 && event.kind == KIND_PENCIL) {
        return Vec::new();
    }
    let at = NormPoint {
        x: event.x_norm,
        y: event.y_norm,
    };
    let button = if event.buttons & 0b100 != 0 {
        Button::Middle
    } else if event.buttons & 0b10 != 0 {
        Button::Right
    } else {
        Button::Left
    };

    match event.kind {
        KIND_PENCIL => {
            let contact = event.input_ver == 1 || event.buttons & 1 != 0;
            let phase = match (event.phase, contact) {
                (PHASE_BEGAN, true) => PenPhase::Down,
                (PHASE_MOVED, true) => PenPhase::Move,
                (PHASE_ENDED, true) => PenPhase::Up,
                (PHASE_CANCELLED, true) => PenPhase::Cancel,
                (PHASE_MOVED, false) => PenPhase::Hover,
                (PHASE_ENDED | PHASE_CANCELLED, false) => PenPhase::Leave,
                _ => return Vec::new(),
            };
            vec![InputIntent::Pen(PenIntent {
                phase,
                at,
                pressure_x1000: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                    event.pressure_x1000.min(1000)
                } else {
                    0
                },
                tilt_x: event.tilt_x.clamp(-90, 90),
                tilt_y: event.tilt_y.clamp(-90, 90),
            })]
        }
        KIND_TOUCH | KIND_MOUSE_ABS => match event.phase {
            PHASE_BEGAN => vec![
                InputIntent::PointerMove { at },
                InputIntent::ButtonDown { button, at },
            ],
            PHASE_MOVED => vec![InputIntent::PointerMove { at }],
            PHASE_ENDED | PHASE_CANCELLED => vec![
                InputIntent::PointerMove { at },
                InputIntent::ButtonUp { button, at },
            ],
            _ => Vec::new(),
        },
        KIND_HOVER if event.phase == PHASE_MOVED => vec![InputIntent::PointerMove { at }],
        KIND_KEY => match event.phase {
            PHASE_BEGAN => vec![InputIntent::Key {
                usage: event.keycode,
                down: true,
            }],
            PHASE_ENDED | PHASE_CANCELLED => vec![InputIntent::Key {
                usage: event.keycode,
                down: false,
            }],
            _ => Vec::new(),
        },
        KIND_TEXT if event.phase == PHASE_BEGAN => vec![InputIntent::Text(event.keycode)],
        KIND_SCROLL => vec![InputIntent::Scroll {
            at,
            dx: event.scroll_dx,
            dy: event.scroll_dy,
        }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u8, phase: u8) -> InputEvent {
        InputEvent {
            input_ver: 1,
            kind,
            phase,
            buttons: 1,
            event_id: 1,
            x_norm: 1234,
            y_norm: 65_535,
            pressure_x1000: 0,
            scroll_dx: 0,
            scroll_dy: 0,
            keycode: 0,
            modifiers: 0,
            client_time_us: 0,
            tilt_x: 0,
            tilt_y: 0,
        }
    }

    const AT: NormPoint = NormPoint { x: 1234, y: 65_535 };

    #[test]
    fn positions_stay_in_wire_units_and_buttons_keep_their_priority() {
        let mut e = event(KIND_TOUCH, PHASE_BEGAN);
        e.buttons = 0b111;
        assert_eq!(
            resolve_intents(&e),
            [
                InputIntent::PointerMove { at: AT },
                InputIntent::ButtonDown {
                    button: Button::Middle,
                    at: AT
                }
            ]
        );
        e.buttons = 0b011;
        e.phase = PHASE_CANCELLED;
        assert_eq!(
            resolve_intents(&e)[1],
            InputIntent::ButtonUp {
                button: Button::Right,
                at: AT
            }
        );
        e.phase = 9;
        assert!(resolve_intents(&e).is_empty());
    }

    #[test]
    fn keys_stay_hid_usages_even_ones_no_table_knows() {
        let mut e = event(KIND_KEY, PHASE_BEGAN);
        e.keycode = 0xffff;
        assert_eq!(
            resolve_intents(&e),
            [InputIntent::Key {
                usage: 0xffff,
                down: true
            }]
        );
        e.phase = PHASE_MOVED;
        assert!(resolve_intents(&e).is_empty());
    }

    #[test]
    fn pen_pressure_is_kept_only_in_contact_and_tilt_is_clamped() {
        let mut e = event(KIND_PENCIL, PHASE_MOVED);
        e.input_ver = 2;
        e.pressure_x1000 = 5000;
        e.tilt_x = -200;
        e.tilt_y = 45;
        assert_eq!(
            resolve_intents(&e),
            [InputIntent::Pen(PenIntent {
                phase: PenPhase::Move,
                at: AT,
                pressure_x1000: 1000,
                tilt_x: -90,
                tilt_y: 45,
            })]
        );
        e.buttons = 0;
        let [InputIntent::Pen(hover)] = resolve_intents(&e)[..] else {
            panic!()
        };
        assert_eq!((hover.phase, hover.pressure_x1000), (PenPhase::Hover, 0));
        // Version 2 is only for the Pencil.
        let mut touch = event(KIND_TOUCH, PHASE_MOVED);
        touch.input_ver = 2;
        assert!(resolve_intents(&touch).is_empty());
    }

    #[test]
    fn scroll_carries_both_deltas_even_when_zero() {
        let mut e = event(KIND_SCROLL, PHASE_BEGAN);
        e.scroll_dy = -40;
        assert_eq!(
            resolve_intents(&e),
            [InputIntent::Scroll {
                at: AT,
                dx: 0,
                dy: -40
            }]
        );
    }
}
