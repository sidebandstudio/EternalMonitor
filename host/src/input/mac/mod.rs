//! macOS input: neutral intents (`super::neutral`) to CGEvents on one
//! display, the virtual display in second-screen mode.
//!
//! Everything here but [`cg`] is plain Rust and runs on every platform's
//! tests. [`MacInput`] keeps the per-session state a real keyboard, mouse
//! and tablet would (held keys and modifiers, buttons, click counts, a
//! surrogate pair's first half, the pen's proximity and contact) and turns
//! each intent into [`MacEvent`]s. A [`MacInjector`] posts them: [`cg`]'s
//! `CgPoster` on a Mac, a recorder in tests. Nothing in this module posts.
//!
//! Points and pixels: every [`MacEvent`] location is a global point
//! (`CGDisplayBounds` space), never a capture pixel. [`display`] keeps the
//! two apart and stops the far edge short of the neighbouring display.

pub mod display;
pub mod keys;

#[cfg(target_os = "macos")]
pub mod cg;

use std::time::{Duration, Instant};

use super::neutral::{Button, InputIntent, NormPoint, PenIntent};
use super::PenPhase;
use display::{norm_to_display_point, DisplayBounds};
use keys::{hid_to_kvk, ModifierState, KVK_CAPS_LOCK};

/// A global position in points: a CGEvent location.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// The display input lands on, and its bounds in global points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MacTarget {
    pub display_id: u32,
    pub bounds: DisplayBounds,
}

impl MacTarget {
    pub fn point(&self, at: NormPoint) -> Point {
        let (x, y) = norm_to_display_point(at.x, at.y, self.bounds);
        Point { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenStroke {
    /// Moving in range without touching: a mouse move.
    Hover,
    Down,
    Drag,
    Up,
}

/// One event to post, in CGEvent terms but without CoreGraphics.
#[derive(Debug, Clone, PartialEq)]
pub enum MacEvent {
    MouseMoved {
        at: Point,
    },
    MouseDragged {
        button: Button,
        at: Point,
    },
    /// `clicks` is `kCGMouseEventClickState`: 2 makes a double click.
    MouseDown {
        button: Button,
        at: Point,
        clicks: i64,
    },
    MouseUp {
        button: Button,
        at: Point,
        clicks: i64,
    },
    /// Continuous, phaseless pixel scrolling at the pointer, in CGEvent's
    /// signs: positive `dy` scrolls up, positive `dx` scrolls left.
    ScrollPixels {
        dx: i32,
        dy: i32,
    },
    Key {
        kvk: u16,
        down: bool,
        flags: u64,
    },
    /// A modifier or Caps Lock edge: `kCGEventFlagsChanged`.
    FlagsChanged {
        kvk: u16,
        down: bool,
        flags: u64,
    },
    /// Text typed as one key down/up pair carrying a whole code point, so a
    /// surrogate pair is never split across events.
    Text {
        units: Vec<u16>,
    },
    TabletProximity {
        enter: bool,
        at: Point,
    },
    /// A mouse event with the tablet-point subtype. `pressure` is 0..=1 and
    /// `tilt` -1..=1 on each axis (±60° at the ends, as Qt reads it).
    Pen {
        stroke: PenStroke,
        at: Point,
        pressure: f64,
        tilt: (f64, f64),
    },
}

/// Why input is not being posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBlocked {
    /// Accessibility (PostEvent) is not granted, or was revoked.
    NoPermission,
    /// The Mac is locked: input must never reach the lock screen.
    ScreenLocked,
}

/// Posts events. The real one is `cg::CgPoster`; tests record.
pub trait MacInjector {
    /// Checked before every batch. While blocked nothing is posted and the
    /// session's held state is dropped, so nothing replays afterwards.
    fn blocked(&self) -> Option<InputBlocked>;
    fn post(&mut self, event: &MacEvent);
}

/// macOS's default double-click interval (`NSEvent.doubleClickInterval`).
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// How far apart, in points, two presses may be and still count together.
const DOUBLE_CLICK_SLOP: f64 = 4.0;

/// Synthesized events carry no click count of their own: macOS takes the
/// one the event says. This counts presses of one button close in time and
/// place, as the window server does for a real mouse.
#[derive(Debug, Default)]
struct ClickCounter {
    last: Option<(Button, Point, Instant)>,
    count: i64,
}

impl ClickCounter {
    fn press(&mut self, button: Button, at: Point, now: Instant) -> i64 {
        self.count = match self.last {
            Some((last_button, last_at, when))
                if last_button == button
                    && now.saturating_duration_since(when) <= DOUBLE_CLICK
                    && (at.x - last_at.x).abs() <= DOUBLE_CLICK_SLOP
                    && (at.y - last_at.y).abs() <= DOUBLE_CLICK_SLOP =>
            {
                self.count + 1
            }
            _ => 1,
        };
        self.last = Some((button, at, now));
        self.count
    }

    fn release(&self) -> i64 {
        self.count.max(1)
    }
}

#[derive(Debug, Default)]
struct PenState {
    in_range: bool,
    /// Where the tip is down, and with what pressure and tilt.
    contact: Option<(Point, f64, (f64, f64))>,
}

/// One client session's input on the Mac.
#[derive(Debug, Default)]
pub struct MacInput {
    modifiers: ModifierState,
    /// Non-modifier keys held down, by kVK.
    keys: std::collections::BTreeSet<u16>,
    /// Modifier keys held down, by kVK.
    held_modifiers: std::collections::BTreeSet<u16>,
    buttons: std::collections::BTreeSet<Button>,
    pointer: Option<Point>,
    clicks: ClickCounter,
    high_surrogate: Option<u16>,
    pen: PenState,
    blocked: Option<InputBlocked>,
}

/// Wire tilt in degrees to CGEvent's -1..=1, which Qt reads as ±60°.
pub fn tilt_unit(degrees: i16) -> f64 {
    (f64::from(degrees) / 60.0).clamp(-1.0, 1.0)
}

impl MacInput {
    /// Why the last batch was not posted, if it was not.
    pub fn blocked(&self) -> Option<InputBlocked> {
        self.blocked
    }

    /// Convert and post one event's intents onto `target`.
    pub fn deliver(
        &mut self,
        intents: &[InputIntent],
        target: MacTarget,
        now: Instant,
        injector: &mut dyn MacInjector,
    ) {
        let blocked = injector.blocked();
        if blocked != self.blocked {
            match blocked {
                Some(reason) => tracing::warn!(?reason, "Mac input blocked; dropping input"),
                None => tracing::info!("Mac input unblocked"),
            }
            self.blocked = blocked;
        }
        if blocked.is_some() {
            // Nothing can be posted, releases included. Forget what was
            // held, so input resumes from a clean state.
            self.forget();
            return;
        }
        let mut events = Vec::new();
        for intent in intents {
            self.convert(*intent, target, now, &mut events);
        }
        for event in &events {
            injector.post(event);
        }
    }

    /// Release everything held when the session ends, expires or is
    /// superseded. Caps Lock stays as the user left it.
    pub fn reset(&mut self, injector: &mut dyn MacInjector) {
        if injector.blocked().is_some() {
            self.forget();
            return;
        }
        for event in self.releases() {
            injector.post(&event);
        }
    }

    fn forget(&mut self) {
        self.keys.clear();
        self.held_modifiers.clear();
        self.modifiers.clear();
        self.buttons.clear();
        self.high_surrogate = None;
        self.pen = PenState::default();
    }

    fn releases(&mut self) -> Vec<MacEvent> {
        let mut events = Vec::new();
        self.end_pen(&mut events);
        let at = self.pointer.unwrap_or(Point { x: 0.0, y: 0.0 });
        for button in std::mem::take(&mut self.buttons) {
            events.push(MacEvent::MouseUp {
                button,
                at,
                clicks: self.clicks.release(),
            });
        }
        for kvk in std::mem::take(&mut self.keys) {
            events.push(MacEvent::Key {
                kvk,
                down: false,
                flags: self.modifiers.flags_for(kvk),
            });
        }
        for kvk in std::mem::take(&mut self.held_modifiers) {
            self.modifiers.update(kvk, false);
            events.push(MacEvent::FlagsChanged {
                kvk,
                down: false,
                flags: self.modifiers.flags_for(kvk),
            });
        }
        self.modifiers.clear();
        self.high_surrogate = None;
        events
    }

    fn convert(
        &mut self,
        intent: InputIntent,
        target: MacTarget,
        now: Instant,
        out: &mut Vec<MacEvent>,
    ) {
        match intent {
            InputIntent::PointerMove { at } => self.move_pointer(target.point(at), out),
            InputIntent::ButtonDown { button, at } => {
                let at = target.point(at);
                self.pointer = Some(at);
                let clicks = self.clicks.press(button, at, now);
                self.buttons.insert(button);
                out.push(MacEvent::MouseDown { button, at, clicks });
            }
            InputIntent::ButtonUp { button, at } => {
                let at = target.point(at);
                self.pointer = Some(at);
                if self.buttons.remove(&button) {
                    out.push(MacEvent::MouseUp {
                        button,
                        at,
                        clicks: self.clicks.release(),
                    });
                }
            }
            InputIntent::Scroll { at, dx, dy } => {
                self.move_pointer(target.point(at), out);
                if dx != 0 || dy != 0 {
                    // The wire follows Windows: positive dy away from the
                    // user (scroll up) and positive dx scrolls right.
                    // CGEvent's wheel 2 is positive to the left.
                    out.push(MacEvent::ScrollPixels {
                        dx: -i32::from(dx),
                        dy: i32::from(dy),
                    });
                }
            }
            InputIntent::Key { usage, down } => {
                let Some(kvk) = hid_to_kvk(usage) else {
                    return;
                };
                if self.modifiers.update(kvk, down) {
                    if kvk != KVK_CAPS_LOCK {
                        if down {
                            self.held_modifiers.insert(kvk);
                        } else {
                            self.held_modifiers.remove(&kvk);
                        }
                    }
                    out.push(MacEvent::FlagsChanged {
                        kvk,
                        down,
                        flags: self.modifiers.flags_for(kvk),
                    });
                    return;
                }
                if down {
                    self.keys.insert(kvk);
                } else {
                    self.keys.remove(&kvk);
                }
                out.push(MacEvent::Key {
                    kvk,
                    down,
                    flags: self.modifiers.flags_for(kvk),
                });
            }
            InputIntent::Text(unit) => self.text(unit, out),
            InputIntent::Pen(pen) => self.pen(pen, target, out),
        }
    }

    fn move_pointer(&mut self, at: Point, out: &mut Vec<MacEvent>) {
        self.pointer = Some(at);
        out.push(match self.buttons.first() {
            Some(&button) => MacEvent::MouseDragged { button, at },
            None => MacEvent::MouseMoved { at },
        });
    }

    /// The wire sends one UTF-16 unit per event. A high surrogate waits for
    /// its low half; a half without its partner is dropped, never posted.
    fn text(&mut self, unit: u16, out: &mut Vec<MacEvent>) {
        let pending = self.high_surrogate.take();
        match unit {
            0xd800..=0xdbff => {
                if pending.is_some() {
                    tracing::debug!("Dropped an unpaired high surrogate");
                }
                self.high_surrogate = Some(unit);
            }
            0xdc00..=0xdfff => match pending {
                Some(high) => out.push(MacEvent::Text {
                    units: vec![high, unit],
                }),
                None => tracing::debug!("Dropped an unpaired low surrogate"),
            },
            _ => out.push(MacEvent::Text { units: vec![unit] }),
        }
    }

    /// A tablet session: proximity in, then tablet-point mouse events with
    /// pressure, then proximity out. Each stroke ends out of proximity, so
    /// a Pencil that never hovers (the Duo, older iPads) does not leave the
    /// Mac thinking a pen is still in range; a hovering one comes straight
    /// back in.
    fn pen(&mut self, pen: PenIntent, target: MacTarget, out: &mut Vec<MacEvent>) {
        let at = target.point(pen.at);
        let pressure = f64::from(pen.pressure_x1000) / 1000.0;
        let tilt = (tilt_unit(pen.tilt_x), tilt_unit(pen.tilt_y));
        self.pointer = Some(at);
        match pen.phase {
            PenPhase::Hover => {
                if self.pen.contact.is_some() {
                    // The up was lost: lift where the tip is.
                    self.end_pen(out);
                }
                self.enter_range(at, out);
                out.push(MacEvent::Pen {
                    stroke: PenStroke::Hover,
                    at,
                    pressure: 0.0,
                    tilt,
                });
            }
            // A lost down recovers at the first sample of the stroke.
            PenPhase::Down | PenPhase::Move if self.pen.contact.is_none() => {
                self.enter_range(at, out);
                self.pen.contact = Some((at, pressure, tilt));
                out.push(MacEvent::Pen {
                    stroke: PenStroke::Down,
                    at,
                    pressure,
                    tilt,
                });
            }
            PenPhase::Down => {
                // A new stroke while the old one is down: its up was lost.
                self.end_pen(out);
                self.pen(pen, target, out);
            }
            PenPhase::Move => {
                self.pen.contact = Some((at, pressure, tilt));
                out.push(MacEvent::Pen {
                    stroke: PenStroke::Drag,
                    at,
                    pressure,
                    tilt,
                });
            }
            PenPhase::Up => {
                if let Some((_, _, tilt)) = self.pen.contact {
                    self.pen.contact = Some((at, 0.0, tilt));
                    self.end_pen(out);
                }
            }
            PenPhase::Cancel | PenPhase::Leave => self.end_pen(out),
        }
    }

    fn enter_range(&mut self, at: Point, out: &mut Vec<MacEvent>) {
        if !self.pen.in_range {
            self.pen.in_range = true;
            out.push(MacEvent::TabletProximity { enter: true, at });
        }
    }

    /// Lift a down tip where it last was, then leave proximity.
    fn end_pen(&mut self, out: &mut Vec<MacEvent>) {
        let mut at = self.pointer;
        if let Some((last, _, tilt)) = self.pen.contact.take() {
            at = Some(last);
            out.push(MacEvent::Pen {
                stroke: PenStroke::Up,
                at: last,
                pressure: 0.0,
                tilt,
            });
        }
        if std::mem::take(&mut self.pen.in_range) {
            out.push(MacEvent::TabletProximity {
                enter: false,
                at: at.unwrap_or(Point { x: 0.0, y: 0.0 }),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::neutral::resolve_intents;
    use crate::input::{
        KIND_HOVER, KIND_KEY, KIND_PENCIL, KIND_SCROLL, KIND_TEXT, KIND_TOUCH, PHASE_BEGAN,
        PHASE_CANCELLED, PHASE_ENDED, PHASE_MOVED,
    };
    use eternal_wire::v2::control::InputEvent;
    use keys::{FLAG_ALPHA_SHIFT, FLAG_COMMAND, FLAG_SHIFT};

    #[derive(Default)]
    struct Recorder {
        events: Vec<MacEvent>,
        blocked: Option<InputBlocked>,
    }

    impl MacInjector for Recorder {
        fn blocked(&self) -> Option<InputBlocked> {
            self.blocked
        }
        fn post(&mut self, event: &MacEvent) {
            self.events.push(event.clone());
        }
    }

    impl Recorder {
        fn take(&mut self) -> Vec<MacEvent> {
            std::mem::take(&mut self.events)
        }
    }

    /// The Duo inner screen at @2x, right of a 3440-pt main display.
    const VIRTUAL: MacTarget = MacTarget {
        display_id: 7,
        bounds: DisplayBounds {
            x: 3440.0,
            y: 250.0,
            width: 1335.0,
            height: 939.0,
        },
    };

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
            tilt_x: 0,
            tilt_y: 0,
        }
    }

    fn pen(phase: u8, buttons: u8, x: u16, pressure: u16) -> InputEvent {
        let mut e = event(KIND_PENCIL, phase, x, 32_768);
        e.input_ver = 2;
        e.buttons = buttons;
        e.pressure_x1000 = pressure;
        e.tilt_x = 30;
        e.tilt_y = -90;
        e
    }

    struct Session {
        input: MacInput,
        out: Recorder,
        now: Instant,
    }

    impl Session {
        fn new() -> Self {
            Self {
                input: MacInput::default(),
                out: Recorder::default(),
                now: Instant::now(),
            }
        }

        fn send(&mut self, e: &InputEvent) -> Vec<MacEvent> {
            self.input
                .deliver(&resolve_intents(e), VIRTUAL, self.now, &mut self.out);
            self.out.take()
        }

        fn key(&mut self, usage: u16, down: bool) -> Vec<MacEvent> {
            let mut e = event(KIND_KEY, if down { PHASE_BEGAN } else { PHASE_ENDED }, 0, 0);
            e.keycode = usage;
            self.send(&e)
        }
    }

    fn inside(at: Point) -> bool {
        VIRTUAL.bounds.contains(at.x, at.y)
    }

    #[test]
    fn every_pointer_event_lands_on_the_target_display_in_points() {
        let mut s = Session::new();
        for (x, y) in [
            (0, 0),
            (65_535, 0),
            (0, 65_535),
            (65_535, 65_535),
            (32_768, 1),
        ] {
            for phase in [PHASE_BEGAN, PHASE_MOVED, PHASE_ENDED] {
                for event in s.send(&event(KIND_TOUCH, phase, x, y)) {
                    let at = match event {
                        MacEvent::MouseMoved { at }
                        | MacEvent::MouseDragged { at, .. }
                        | MacEvent::MouseDown { at, .. }
                        | MacEvent::MouseUp { at, .. } => at,
                        other => panic!("unexpected {other:?}"),
                    };
                    assert!(inside(at), "({x},{y}) -> {at:?}");
                }
            }
        }
        // Full scale stays on the virtual display, short of whatever is to
        // its right, and is in points (1335 wide), not pixels (2670).
        let [MacEvent::MouseMoved { at }] = s.send(&event(KIND_HOVER, PHASE_MOVED, 65_535, 0))[..]
        else {
            panic!()
        };
        assert!(at.x < 3440.0 + 1335.0 && at.x > 3440.0 + 1334.9, "{at:?}");
    }

    #[test]
    fn a_tap_is_move_down_up_and_a_second_tap_double_clicks() {
        let mut s = Session::new();
        let down = s.send(&event(KIND_TOUCH, PHASE_BEGAN, 100, 100));
        let at = VIRTUAL.point(NormPoint { x: 100, y: 100 });
        assert_eq!(
            down,
            [
                MacEvent::MouseMoved { at },
                MacEvent::MouseDown {
                    button: Button::Left,
                    at,
                    clicks: 1
                }
            ]
        );
        let moved = s.send(&event(KIND_TOUCH, PHASE_MOVED, 101, 100));
        assert!(matches!(
            moved[..],
            [MacEvent::MouseDragged {
                button: Button::Left,
                ..
            }]
        ));
        s.send(&event(KIND_TOUCH, PHASE_ENDED, 100, 100));
        s.now += Duration::from_millis(200);
        let again = s.send(&event(KIND_TOUCH, PHASE_BEGAN, 100, 100));
        assert!(matches!(again[1], MacEvent::MouseDown { clicks: 2, .. }));
        assert!(matches!(
            s.send(&event(KIND_TOUCH, PHASE_ENDED, 100, 100))[1],
            MacEvent::MouseUp { clicks: 2, .. }
        ));
        // Too late, or too far: a fresh single click.
        s.now += Duration::from_millis(600);
        assert!(matches!(
            s.send(&event(KIND_TOUCH, PHASE_BEGAN, 100, 100))[1],
            MacEvent::MouseDown { clicks: 1, .. }
        ));
        s.send(&event(KIND_TOUCH, PHASE_ENDED, 100, 100));
        assert!(matches!(
            s.send(&event(KIND_TOUCH, PHASE_BEGAN, 9_000, 100))[1],
            MacEvent::MouseDown { clicks: 1, .. }
        ));
    }

    #[test]
    fn right_and_middle_buttons_and_a_stray_release() {
        let mut s = Session::new();
        let mut e = event(KIND_TOUCH, PHASE_BEGAN, 5, 5);
        e.buttons = 0b10;
        assert!(matches!(
            s.send(&e)[1],
            MacEvent::MouseDown {
                button: Button::Right,
                ..
            }
        ));
        e.buttons = 0b100;
        e.phase = PHASE_ENDED;
        // Middle was never pressed: only the move goes out.
        assert!(matches!(s.send(&e)[..], [MacEvent::MouseDragged { .. }]));
    }

    #[test]
    fn scroll_moves_first_and_converts_horizontal_sign() {
        let mut s = Session::new();
        let mut e = event(KIND_SCROLL, PHASE_MOVED, 0, 0);
        e.scroll_dy = -40;
        e.scroll_dx = 10;
        let out = s.send(&e);
        assert!(matches!(out[0], MacEvent::MouseMoved { .. }));
        assert_eq!(out[1], MacEvent::ScrollPixels { dx: -10, dy: -40 });
        e.scroll_dx = 0;
        e.scroll_dy = 0;
        assert_eq!(s.send(&e).len(), 1);
    }

    #[test]
    fn keys_carry_modifier_flags_and_modifiers_are_flags_changed() {
        let mut s = Session::new();
        let command = hid_to_kvk(0xe3).unwrap();
        let c = hid_to_kvk(0x06).unwrap();
        assert_eq!(
            s.key(0xe3, true),
            [MacEvent::FlagsChanged {
                kvk: command,
                down: true,
                flags: FLAG_COMMAND | 0x8
            }]
        );
        assert_eq!(
            s.key(0x06, true),
            [MacEvent::Key {
                kvk: c,
                down: true,
                flags: FLAG_COMMAND | 0x8
            }]
        );
        s.key(0x06, false);
        assert_eq!(
            s.key(0xe3, false),
            [MacEvent::FlagsChanged {
                kvk: command,
                down: false,
                flags: 0
            }]
        );
        // Unknown usages post nothing.
        assert!(s.key(0x66, true).is_empty());
        // Caps Lock toggles on its press.
        s.key(0x39, true);
        s.key(0x39, false);
        assert!(matches!(
            s.key(0x04, true)[..],
            [MacEvent::Key { flags, .. }] if flags == FLAG_ALPHA_SHIFT
        ));
    }

    #[test]
    fn text_joins_surrogate_pairs_and_drops_orphans() {
        let mut s = Session::new();
        let mut type_unit = |unit: u16| {
            let mut e = event(KIND_TEXT, PHASE_BEGAN, 0, 0);
            e.keycode = unit;
            s.send(&e)
        };
        assert_eq!(type_unit(0xe9), [MacEvent::Text { units: vec![0xe9] }]);
        assert!(type_unit(0xd83d).is_empty());
        assert_eq!(
            type_unit(0xde00),
            [MacEvent::Text {
                units: vec![0xd83d, 0xde00]
            }]
        );
        // A lone low half, then a high half followed by plain text.
        assert!(type_unit(0xde00).is_empty());
        assert!(type_unit(0xd83d).is_empty());
        assert_eq!(type_unit(0x41), [MacEvent::Text { units: vec![0x41] }]);
        // Two highs in a row keep only the second.
        type_unit(0xd83c);
        type_unit(0xd83d);
        assert_eq!(
            type_unit(0xdc4d),
            [MacEvent::Text {
                units: vec![0xd83d, 0xdc4d]
            }]
        );
    }

    #[test]
    fn a_pen_stroke_enters_proximity_draws_with_pressure_and_leaves() {
        let mut s = Session::new();
        let hover = s.send(&pen(PHASE_MOVED, 0, 100, 0));
        assert!(matches!(
            hover[..],
            [
                MacEvent::TabletProximity { enter: true, .. },
                MacEvent::Pen {
                    stroke: PenStroke::Hover,
                    ..
                }
            ]
        ));
        let down = s.send(&pen(PHASE_BEGAN, 1, 200, 750));
        let [MacEvent::Pen {
            stroke: PenStroke::Down,
            at,
            pressure,
            tilt,
        }] = down[..]
        else {
            panic!("{down:?}")
        };
        assert!(inside(at));
        assert_eq!((pressure, tilt), (0.75, (0.5, -1.0)));
        assert!(matches!(
            s.send(&pen(PHASE_MOVED, 1, 300, 1000))[..],
            [MacEvent::Pen {
                stroke: PenStroke::Drag,
                pressure: 1.0,
                ..
            }]
        ));
        let up = s.send(&pen(PHASE_ENDED, 1, 300, 0));
        assert!(matches!(
            up[..],
            [
                MacEvent::Pen {
                    stroke: PenStroke::Up,
                    pressure: 0.0,
                    ..
                },
                MacEvent::TabletProximity { enter: false, .. }
            ]
        ));
        // A replayed up after the stroke posts nothing.
        assert!(s.send(&pen(PHASE_ENDED, 1, 300, 0)).is_empty());
        // A Pencil that never hovers starts its next stroke in proximity.
        assert!(matches!(
            s.send(&pen(PHASE_BEGAN, 1, 300, 500))[..],
            [
                MacEvent::TabletProximity { enter: true, .. },
                MacEvent::Pen {
                    stroke: PenStroke::Down,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn lost_pen_edges_recover() {
        let mut s = Session::new();
        // A lost down: the first move starts the stroke.
        assert!(matches!(
            s.send(&pen(PHASE_MOVED, 1, 10, 400))[..],
            [
                MacEvent::TabletProximity { enter: true, .. },
                MacEvent::Pen {
                    stroke: PenStroke::Down,
                    ..
                }
            ]
        ));
        // A lost up: the next down ends the old stroke where it was first.
        let first = VIRTUAL.point(NormPoint { x: 10, y: 32_768 });
        let out = s.send(&pen(PHASE_BEGAN, 1, 900, 400));
        assert!(matches!(
            out[..],
            [
                MacEvent::Pen { stroke: PenStroke::Up, at, .. },
                MacEvent::TabletProximity { enter: false, .. },
                MacEvent::TabletProximity { enter: true, .. },
                MacEvent::Pen { stroke: PenStroke::Down, .. },
            ] if at == first
        ));
        // Hover after a lost up lifts first.
        assert!(matches!(
            s.send(&pen(PHASE_MOVED, 0, 900, 0))[..],
            [
                MacEvent::Pen {
                    stroke: PenStroke::Up,
                    ..
                },
                MacEvent::TabletProximity { enter: false, .. },
                MacEvent::TabletProximity { enter: true, .. },
                MacEvent::Pen {
                    stroke: PenStroke::Hover,
                    ..
                },
            ]
        ));
        // Leaving range ends proximity once.
        assert!(matches!(
            s.send(&pen(PHASE_ENDED, 0, 900, 0))[..],
            [MacEvent::TabletProximity { enter: false, .. }]
        ));
        assert!(s.send(&pen(PHASE_CANCELLED, 0, 900, 0)).is_empty());
    }

    #[test]
    fn reset_releases_everything_held_once() {
        let mut s = Session::new();
        s.key(0xe1, true); // left Shift
        s.key(0x04, true); // A
        s.send(&event(KIND_TOUCH, PHASE_BEGAN, 50, 50));
        s.send(&pen(PHASE_BEGAN, 1, 60, 300));
        s.input.reset(&mut s.out);
        let released = s.out.take();
        let shift = hid_to_kvk(0xe1).unwrap();
        assert_eq!(released.len(), 5, "{released:?}");
        assert!(matches!(
            released[0],
            MacEvent::Pen {
                stroke: PenStroke::Up,
                ..
            }
        ));
        assert!(matches!(
            released[1],
            MacEvent::TabletProximity { enter: false, .. }
        ));
        assert!(matches!(
            released[2],
            MacEvent::MouseUp {
                button: Button::Left,
                ..
            }
        ));
        assert_eq!(
            released[3],
            MacEvent::Key {
                kvk: hid_to_kvk(0x04).unwrap(),
                down: false,
                flags: FLAG_SHIFT | 0x2
            }
        );
        assert_eq!(
            released[4],
            MacEvent::FlagsChanged {
                kvk: shift,
                down: false,
                flags: 0
            }
        );
        s.input.reset(&mut s.out);
        assert!(s.out.take().is_empty());
    }

    #[test]
    fn nothing_is_posted_while_blocked_and_nothing_replays_after() {
        let mut s = Session::new();
        s.key(0xe3, true);
        s.send(&event(KIND_TOUCH, PHASE_BEGAN, 50, 50));
        for reason in [InputBlocked::ScreenLocked, InputBlocked::NoPermission] {
            s.out.blocked = Some(reason);
            assert!(s.key(0x06, true).is_empty());
            assert_eq!(s.input.blocked(), Some(reason));
            s.input.reset(&mut s.out);
            assert!(s.out.take().is_empty());
        }
        s.out.blocked = None;
        // Held state was forgotten: no Command on the next key, and the
        // next move is a plain move, not a drag.
        assert!(matches!(
            s.key(0x06, true)[..],
            [MacEvent::Key { flags: 0, .. }]
        ));
        assert!(matches!(
            s.send(&event(KIND_TOUCH, PHASE_MOVED, 60, 60))[..],
            [MacEvent::MouseMoved { .. }]
        ));
        assert_eq!(s.input.blocked(), None);
    }

    #[test]
    fn tilt_scale_matches_qt() {
        assert_eq!(tilt_unit(60), 1.0);
        assert_eq!(tilt_unit(-90), -1.0);
        assert_eq!(tilt_unit(30), 0.5);
        assert_eq!(tilt_unit(0), 0.0);
    }
}
