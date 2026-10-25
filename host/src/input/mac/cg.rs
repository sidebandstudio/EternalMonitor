//! CoreGraphics side of the macOS input backend: builds a CGEvent for each
//! [`MacEvent`] the way spike b verified field by field, and posts it.
//!
//! Building an event needs no permission and changes nothing, so the tests
//! build every kind and read it back. Only [`CgPoster::post`] posts, and
//! only when Accessibility is granted and the Mac is not locked; tests never
//! create a `CgPoster`.

use objc2_core_foundation::{CFRetained, CGPoint, CGRect};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventField, CGEventFlags, CGEventMouseSubtype, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
    CGPreflightPostEventAccess, CGScrollEventUnit, CGSessionCopyCurrentDictionary,
};

use super::display::DisplayBounds;
use super::{InputBlocked, MacEvent, MacInjector, MacTarget, PenStroke, Point};
use crate::input::neutral::Button;

impl From<CGRect> for DisplayBounds {
    fn from(rect: CGRect) -> Self {
        Self {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        }
    }
}

impl MacTarget {
    /// The live bounds of `display_id`.
    pub fn of_display(display_id: u32) -> Self {
        Self {
            display_id,
            bounds: CGDisplayBounds(display_id).into(),
        }
    }
}

/// One 16-bit device ID ties a pen's proximity and point events together:
/// Qt drops points from a device it has not seen enter proximity, and the
/// fields keep only 16 bits.
pub const PEN_DEVICE_ID: i64 = 0x4d01;
/// `NX_TABLET_POINTER_PEN`.
const POINTER_PEN: i64 = 1;
/// `NX_TABLET_CAPABILITY_{DEVICEID,ABSX,ABSY,BUTTONS,TILTX,TILTY,PRESSURE,ROTATION}MASK`.
const PEN_CAPABILITIES: i64 = 0x0001 | 0x0002 | 0x0004 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x2000;
/// Wacom's USB vendor ID, which tablet-aware apps expect to see.
const PEN_VENDOR_ID: i64 = 0x56a;

fn cg_point(at: Point) -> CGPoint {
    CGPoint { x: at.x, y: at.y }
}

fn cg_button(button: Button) -> CGMouseButton {
    match button {
        Button::Left => CGMouseButton::Left,
        Button::Right => CGMouseButton::Right,
        Button::Middle => CGMouseButton::Center,
    }
}

fn mouse_type(button: Button, kind: MouseKind) -> CGEventType {
    match (button, kind) {
        (Button::Left, MouseKind::Down) => CGEventType::LeftMouseDown,
        (Button::Left, MouseKind::Up) => CGEventType::LeftMouseUp,
        (Button::Left, MouseKind::Dragged) => CGEventType::LeftMouseDragged,
        (Button::Right, MouseKind::Down) => CGEventType::RightMouseDown,
        (Button::Right, MouseKind::Up) => CGEventType::RightMouseUp,
        (Button::Right, MouseKind::Dragged) => CGEventType::RightMouseDragged,
        (Button::Middle, MouseKind::Down) => CGEventType::OtherMouseDown,
        (Button::Middle, MouseKind::Up) => CGEventType::OtherMouseUp,
        (Button::Middle, MouseKind::Dragged) => CGEventType::OtherMouseDragged,
    }
}

#[derive(Clone, Copy)]
enum MouseKind {
    Down,
    Up,
    Dragged,
}

fn set_int(event: &CGEvent, field: CGEventField, value: i64) {
    CGEvent::set_integer_value_field(Some(event), field, value);
}

fn set_double(event: &CGEvent, field: CGEventField, value: f64) {
    CGEvent::set_double_value_field(Some(event), field, value);
}

fn mouse(
    source: &CGEventSource,
    kind: CGEventType,
    at: Point,
    button: CGMouseButton,
) -> Option<CFRetained<CGEvent>> {
    CGEvent::new_mouse_event(Some(source), kind, cg_point(at), button)
}

fn keyboard(
    source: &CGEventSource,
    kvk: u16,
    down: bool,
    flags: u64,
) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new_keyboard_event(Some(source), kvk, down)?;
    CGEvent::set_flags(Some(&event), CGEventFlags(flags));
    Some(event)
}

/// The CGEvents for one [`MacEvent`]: one each, except text, which is a key
/// down and up carrying the string. `None` if CoreGraphics refused one.
pub fn build(event: &MacEvent, source: &CGEventSource) -> Option<Vec<CFRetained<CGEvent>>> {
    let one = |event: Option<CFRetained<CGEvent>>| event.map(|event| vec![event]);
    match event {
        MacEvent::MouseMoved { at } => one(mouse(
            source,
            CGEventType::MouseMoved,
            *at,
            CGMouseButton::Left,
        )),
        MacEvent::MouseDragged { button, at } => one(mouse(
            source,
            mouse_type(*button, MouseKind::Dragged),
            *at,
            cg_button(*button),
        )),
        MacEvent::MouseDown { button, at, clicks } | MacEvent::MouseUp { button, at, clicks } => {
            let kind = if matches!(event, MacEvent::MouseDown { .. }) {
                MouseKind::Down
            } else {
                MouseKind::Up
            };
            let built = mouse(source, mouse_type(*button, kind), *at, cg_button(*button))?;
            set_int(&built, CGEventField::MouseEventClickState, *clicks);
            Some(vec![built])
        }
        MacEvent::ScrollPixels { dx, dy } => {
            // Pixel units make a continuous, phaseless scroll, as a
            // trackpad's momentum-free deltas look to apps.
            one(CGEvent::new_scroll_wheel_event2(
                Some(source),
                CGScrollEventUnit::Pixel,
                2,
                *dy,
                *dx,
                0,
            ))
        }
        MacEvent::Key { kvk, down, flags } | MacEvent::FlagsChanged { kvk, down, flags } => {
            // CGEventCreateKeyboardEvent already types modifier and Caps
            // Lock edges as kCGEventFlagsChanged.
            one(keyboard(source, *kvk, *down, *flags))
        }
        MacEvent::Text { units } => {
            let mut events = Vec::with_capacity(2);
            for down in [true, false] {
                let built = keyboard(source, 0, down, 0)?;
                // SAFETY: `units` outlives the call, which copies them.
                unsafe {
                    CGEvent::keyboard_set_unicode_string(
                        Some(&built),
                        units.len() as _,
                        units.as_ptr(),
                    )
                };
                events.push(built);
            }
            Some(events)
        }
        MacEvent::TabletProximity { enter, at } => {
            let built = CGEvent::new(Some(source))?;
            CGEvent::set_type(Some(&built), CGEventType::TabletProximity);
            CGEvent::set_location(Some(&built), cg_point(*at));
            for (field, value) in [
                (CGEventField::TabletProximityEventVendorID, PEN_VENDOR_ID),
                (CGEventField::TabletProximityEventTabletID, 1),
                (CGEventField::TabletProximityEventPointerID, 1),
                (CGEventField::TabletProximityEventDeviceID, PEN_DEVICE_ID),
                (CGEventField::TabletProximityEventSystemTabletID, 1),
                (CGEventField::TabletProximityEventVendorPointerType, 0x0802),
                (
                    CGEventField::TabletProximityEventVendorPointerSerialNumber,
                    1,
                ),
                (CGEventField::TabletProximityEventVendorUniqueID, 1),
                (
                    CGEventField::TabletProximityEventCapabilityMask,
                    PEN_CAPABILITIES,
                ),
                (CGEventField::TabletProximityEventPointerType, POINTER_PEN),
                (
                    CGEventField::TabletProximityEventEnterProximity,
                    i64::from(*enter),
                ),
            ] {
                set_int(&built, field, value);
            }
            Some(vec![built])
        }
        MacEvent::Pen {
            stroke,
            at,
            pressure,
            tilt,
        } => {
            let (kind, buttons, clicks) = match stroke {
                PenStroke::Hover => (CGEventType::MouseMoved, 0, 0),
                PenStroke::Down => (CGEventType::LeftMouseDown, 1, 1),
                PenStroke::Drag => (CGEventType::LeftMouseDragged, 1, 0),
                PenStroke::Up => (CGEventType::LeftMouseUp, 0, 1),
            };
            let built = mouse(source, kind, *at, CGMouseButton::Left)?;
            // The subtype goes first: setting it later wipes the tablet
            // fields, and without it they are not stored at all.
            set_int(
                &built,
                CGEventField::MouseEventSubtype,
                CGEventMouseSubtype::TabletPoint.0 as i64,
            );
            if clicks != 0 {
                set_int(&built, CGEventField::MouseEventClickState, clicks);
            }
            set_int(&built, CGEventField::TabletEventPointX, at.x.round() as i64);
            set_int(&built, CGEventField::TabletEventPointY, at.y.round() as i64);
            set_int(&built, CGEventField::TabletEventPointButtons, buttons);
            set_double(&built, CGEventField::TabletEventPointPressure, *pressure);
            // Apps reading NSEvent.pressure off a mouse event get this one
            // (8-bit).
            set_double(&built, CGEventField::MouseEventPressure, *pressure);
            set_double(&built, CGEventField::TabletEventTiltX, tilt.0);
            set_double(&built, CGEventField::TabletEventTiltY, tilt.1);
            set_int(&built, CGEventField::TabletEventDeviceID, PEN_DEVICE_ID);
            Some(vec![built])
        }
    }
}

/// Whether the console session's screen is locked. Read from the session
/// dictionary, which needs no permission.
pub fn screen_locked() -> bool {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    let Some(session) = CGSessionCopyCurrentDictionary() else {
        // No console session at all (a bare SSH login): nothing to post to.
        return true;
    };
    // SAFETY: a +1 CFDictionary handed over to the wrapper, which releases
    // it once.
    let session: CFDictionary<CFString, CFType> = unsafe {
        CFDictionary::wrap_under_create_rule(CFRetained::into_raw(session).as_ptr().cast())
    };
    session
        .find(CFString::from_static_string("CGSSessionScreenIsLocked"))
        .and_then(|value| value.downcast::<CFBoolean>())
        .is_some_and(bool::from)
}

/// Posts at the HID tap from a private event source, so the host's events
/// never inherit the state of a real keyboard attached to the Mac.
pub struct CgPoster {
    source: CFRetained<CGEventSource>,
}

impl CgPoster {
    pub fn new() -> Option<Self> {
        CGEventSource::new(CGEventSourceStateID::Private).map(|source| Self { source })
    }
}

impl MacInjector for CgPoster {
    fn blocked(&self) -> Option<InputBlocked> {
        if !CGPreflightPostEventAccess() {
            Some(InputBlocked::NoPermission)
        } else if screen_locked() {
            Some(InputBlocked::ScreenLocked)
        } else {
            None
        }
    }

    fn post(&mut self, event: &MacEvent) {
        match build(event, &self.source) {
            Some(events) => {
                for built in events {
                    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&built));
                }
            }
            None => tracing::warn!(?event, "CoreGraphics could not build an input event"),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Builds every event and reads it back. Nothing is posted.
    use super::*;
    use crate::input::mac::keys::{FLAG_COMMAND, FLAG_SHIFT};

    fn source() -> CFRetained<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::Private).expect("private event source")
    }

    fn int(event: &CGEvent, field: CGEventField) -> i64 {
        CGEvent::integer_value_field(Some(event), field)
    }

    fn double(event: &CGEvent, field: CGEventField) -> f64 {
        CGEvent::double_value_field(Some(event), field)
    }

    fn built(event: MacEvent) -> Vec<CFRetained<CGEvent>> {
        build(&event, &source()).expect("built")
    }

    fn only(event: MacEvent) -> CFRetained<CGEvent> {
        let mut events = built(event);
        assert_eq!(events.len(), 1);
        events.remove(0)
    }

    const AT: Point = Point {
        x: 4774.5,
        y: -12.25,
    };

    #[test]
    fn mouse_events_keep_type_button_location_and_clicks() {
        for (button, number, down, up, dragged) in [
            (
                Button::Left,
                0,
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGEventType::LeftMouseDragged,
            ),
            (
                Button::Right,
                1,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGEventType::RightMouseDragged,
            ),
            (
                Button::Middle,
                2,
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGEventType::OtherMouseDragged,
            ),
        ] {
            for (event, kind, clicks) in [
                (
                    MacEvent::MouseDown {
                        button,
                        at: AT,
                        clicks: 2,
                    },
                    down,
                    2,
                ),
                (
                    MacEvent::MouseUp {
                        button,
                        at: AT,
                        clicks: 2,
                    },
                    up,
                    2,
                ),
                (MacEvent::MouseDragged { button, at: AT }, dragged, 0),
            ] {
                let event = only(event);
                assert_eq!(CGEvent::r#type(Some(&event)), kind);
                let at = CGEvent::location(Some(&event));
                assert_eq!((at.x, at.y), (AT.x, AT.y));
                assert_eq!(int(&event, CGEventField::MouseEventButtonNumber), number);
                if clicks != 0 {
                    assert_eq!(int(&event, CGEventField::MouseEventClickState), clicks);
                }
            }
        }
        let moved = only(MacEvent::MouseMoved { at: AT });
        assert_eq!(CGEvent::r#type(Some(&moved)), CGEventType::MouseMoved);
    }

    #[test]
    fn pixel_scroll_is_continuous_with_both_axes() {
        let event = only(MacEvent::ScrollPixels { dx: 10, dy: -40 });
        assert_eq!(CGEvent::r#type(Some(&event)), CGEventType::ScrollWheel);
        assert_eq!(
            int(&event, CGEventField::ScrollWheelEventPointDeltaAxis1),
            -40
        );
        assert_eq!(
            int(&event, CGEventField::ScrollWheelEventPointDeltaAxis2),
            10
        );
        assert_eq!(int(&event, CGEventField::ScrollWheelEventIsContinuous), 1);
    }

    #[test]
    fn keys_and_modifiers_carry_their_flags() {
        let flags = FLAG_COMMAND | 0x8 | FLAG_SHIFT | 0x2;
        let key = only(MacEvent::Key {
            kvk: 0x08,
            down: true,
            flags,
        });
        assert_eq!(CGEvent::r#type(Some(&key)), CGEventType::KeyDown);
        assert_eq!(int(&key, CGEventField::KeyboardEventKeycode), 0x08);
        assert_eq!(CGEvent::flags(Some(&key)).0, flags);
        let up = only(MacEvent::Key {
            kvk: 0x08,
            down: false,
            flags: 0,
        });
        assert_eq!(CGEvent::r#type(Some(&up)), CGEventType::KeyUp);
        let command = only(MacEvent::FlagsChanged {
            kvk: 0x37,
            down: true,
            flags: FLAG_COMMAND | 0x8,
        });
        assert_eq!(CGEvent::r#type(Some(&command)), CGEventType::FlagsChanged);
        assert_eq!(CGEvent::flags(Some(&command)).0, FLAG_COMMAND | 0x8);
    }

    #[test]
    fn text_is_a_key_pair_carrying_the_whole_code_point() {
        let events = built(MacEvent::Text {
            units: vec![0xd83d, 0xde00],
        });
        assert_eq!(events.len(), 2);
        for (event, kind) in events
            .iter()
            .zip([CGEventType::KeyDown, CGEventType::KeyUp])
        {
            assert_eq!(CGEvent::r#type(Some(event)), kind);
            let mut buffer = [0u16; 8];
            let mut length = 0;
            // SAFETY: the buffer holds `buffer.len()` units.
            unsafe {
                CGEvent::keyboard_get_unicode_string(
                    Some(event),
                    buffer.len() as _,
                    &mut length,
                    buffer.as_mut_ptr(),
                )
            };
            assert_eq!(&buffer[..length as usize], &[0xd83d, 0xde00]);
        }
    }

    #[test]
    fn proximity_names_a_pen_with_one_device_id() {
        for enter in [true, false] {
            let event = only(MacEvent::TabletProximity { enter, at: AT });
            assert_eq!(CGEvent::r#type(Some(&event)), CGEventType::TabletProximity);
            assert_eq!(
                int(&event, CGEventField::TabletProximityEventDeviceID),
                PEN_DEVICE_ID
            );
            assert_eq!(
                int(&event, CGEventField::TabletProximityEventPointerType),
                POINTER_PEN
            );
            assert_eq!(
                int(&event, CGEventField::TabletProximityEventCapabilityMask),
                PEN_CAPABILITIES
            );
            assert_eq!(
                int(&event, CGEventField::TabletProximityEventEnterProximity),
                i64::from(enter)
            );
        }
    }

    #[test]
    fn pen_points_keep_pressure_tilt_and_device() {
        for (stroke, kind, buttons) in [
            (PenStroke::Hover, CGEventType::MouseMoved, 0),
            (PenStroke::Down, CGEventType::LeftMouseDown, 1),
            (PenStroke::Drag, CGEventType::LeftMouseDragged, 1),
            (PenStroke::Up, CGEventType::LeftMouseUp, 0),
        ] {
            let event = only(MacEvent::Pen {
                stroke,
                at: AT,
                pressure: 0.734,
                tilt: (0.5, -1.0),
            });
            assert_eq!(CGEvent::r#type(Some(&event)), kind);
            assert_eq!(
                int(&event, CGEventField::MouseEventSubtype),
                CGEventMouseSubtype::TabletPoint.0 as i64
            );
            assert!((double(&event, CGEventField::TabletEventPointPressure) - 0.734).abs() < 1e-4);
            // Mouse pressure is 8-bit.
            assert!(
                (double(&event, CGEventField::MouseEventPressure) - 0.734).abs() <= 0.5 / 255.0
            );
            assert!((double(&event, CGEventField::TabletEventTiltX) - 0.5).abs() < 1e-4);
            assert!((double(&event, CGEventField::TabletEventTiltY) + 1.0).abs() < 1e-4);
            assert_eq!(
                int(&event, CGEventField::TabletEventDeviceID),
                PEN_DEVICE_ID
            );
            assert_eq!(int(&event, CGEventField::TabletEventPointButtons), buttons);
            let at = CGEvent::location(Some(&event));
            assert_eq!((at.x, at.y), (AT.x, AT.y));
        }
    }

    #[test]
    fn display_bounds_come_from_a_cg_rect() {
        let rect = CGRect {
            origin: CGPoint {
                x: -1017.0,
                y: -699.5,
            },
            size: objc2_core_foundation::CGSize {
                width: 1017.0,
                height: 699.0,
            },
        };
        assert_eq!(
            DisplayBounds::from(rect),
            DisplayBounds {
                x: -1017.0,
                y: -699.5,
                width: 1017.0,
                height: 699.0
            }
        );
    }
}
