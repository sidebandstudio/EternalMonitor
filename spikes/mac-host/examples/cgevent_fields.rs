//! Spike b: build every CGEvent the macOS input backend needs and read each
//! field back. Nothing is posted: creating an event needs no permission and
//! changes nothing. Also maps the wire's corners onto the live displays and
//! asks the French and German layouts, read-only, what each key position
//! types.
//!
//! cargo run -p eternal-mac-spikes --example cgevent_fields

fn main() {
    #[cfg(target_os = "macos")]
    imp::main();
}

#[cfg(target_os = "macos")]
mod imp {
    use eternal_mac_spikes::display_map::{self, CapturePixels, DisplayBounds};
    use eternal_mac_spikes::keymap::{self, ModifierState};
    use objc2_core_foundation::{CFRetained, CGPoint};
    use objc2_core_graphics::{
        CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGDisplayRotation, CGEvent,
        CGEventField, CGEventMouseSubtype, CGEventSource, CGEventSourceStateID, CGEventType,
        CGGetActiveDisplayList, CGMouseButton, CGScrollEventUnit,
    };
    use std::ffi::c_void;

    #[derive(Default)]
    struct Checks {
        passed: u32,
        failed: Vec<String>,
    }

    impl Checks {
        fn eq<T: PartialEq + std::fmt::Debug>(&mut self, what: &str, expected: T, actual: T) {
            if expected == actual {
                self.passed += 1;
            } else {
                let line = format!("{what}: expected {expected:?}, read {actual:?}");
                println!("FAIL {line}");
                self.failed.push(line);
            }
        }
        fn near(&mut self, what: &str, expected: f64, actual: f64, tolerance: f64) {
            if (expected - actual).abs() <= tolerance {
                self.passed += 1;
            } else {
                let line = format!("{what}: expected {expected}, read {actual}");
                println!("FAIL {line}");
                self.failed.push(line);
            }
        }
    }

    fn int(event: &CGEvent, field: CGEventField) -> i64 {
        CGEvent::integer_value_field(Some(event), field)
    }

    fn double(event: &CGEvent, field: CGEventField) -> f64 {
        CGEvent::double_value_field(Some(event), field)
    }

    fn unicode(event: &CGEvent) -> Vec<u16> {
        let mut buffer = [0u16; 64];
        let mut length = 0;
        unsafe {
            CGEvent::keyboard_get_unicode_string(
                Some(event),
                buffer.len() as _,
                &mut length,
                buffer.as_mut_ptr(),
            )
        };
        buffer[..length as usize].to_vec()
    }

    fn row(name: &str, event: &CGEvent, extra: &str) {
        let at = CGEvent::location(Some(event));
        println!(
            "| {name} | {} | ({:.2}, {:.2}) | {:#x} | {} |",
            CGEvent::r#type(Some(event)).0,
            at.x,
            at.y,
            CGEvent::flags(Some(event)).0,
            extra
        );
    }

    fn mouse(
        checks: &mut Checks,
        source: &CGEventSource,
        name: &str,
        kind: CGEventType,
        button: CGMouseButton,
        at: CGPoint,
    ) -> CFRetained<CGEvent> {
        let event = CGEvent::new_mouse_event(Some(source), kind, at, button).expect(name);
        if matches!(
            kind,
            CGEventType::LeftMouseDown
                | CGEventType::LeftMouseUp
                | CGEventType::RightMouseDown
                | CGEventType::RightMouseUp
                | CGEventType::OtherMouseDown
                | CGEventType::OtherMouseUp
        ) {
            CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventClickState, 1);
        }
        checks.eq(
            &format!("{name} type"),
            kind.0,
            CGEvent::r#type(Some(&event)).0,
        );
        let read = CGEvent::location(Some(&event));
        checks.eq(&format!("{name} location"), (at.x, at.y), (read.x, read.y));
        checks.eq(
            &format!("{name} button"),
            i64::from(button.0),
            int(&event, CGEventField::MouseEventButtonNumber),
        );
        row(
            name,
            &event,
            &format!(
                "button={} click={} subtype={} pressure={}",
                int(&event, CGEventField::MouseEventButtonNumber),
                int(&event, CGEventField::MouseEventClickState),
                int(&event, CGEventField::MouseEventSubtype),
                double(&event, CGEventField::MouseEventPressure)
            ),
        );
        event
    }

    /// Fields a proximity event carries. One device ID ties the pen's
    /// proximity and point events together; Qt drops points from a device
    /// it has not seen enter proximity. The fields are 16-bit: a first run
    /// with 0x454d01 read back 0x4d01.
    const DEVICE_ID: i64 = 0x4d01;
    /// NX_TABLET_POINTER_PEN.
    const POINTER_PEN: i64 = 1;
    // NX_TABLET_CAPABILITY_{DEVICEID,ABSX,ABSY,BUTTONS,TILTX,TILTY,PRESSURE,ROTATION}MASK
    const CAPABILITIES: i64 = 0x0001 | 0x0002 | 0x0004 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x2000;

    fn set_proximity(event: &CGEvent, enter: bool) {
        let set = |field, value| CGEvent::set_integer_value_field(Some(event), field, value);
        set(CGEventField::TabletProximityEventVendorID, 0x56a); // Wacom's USB vendor ID
        set(CGEventField::TabletProximityEventTabletID, 1);
        set(CGEventField::TabletProximityEventPointerID, 1);
        set(CGEventField::TabletProximityEventDeviceID, DEVICE_ID);
        set(CGEventField::TabletProximityEventSystemTabletID, 1);
        set(CGEventField::TabletProximityEventVendorPointerType, 0x0802);
        set(
            CGEventField::TabletProximityEventVendorPointerSerialNumber,
            1,
        );
        set(CGEventField::TabletProximityEventVendorUniqueID, 1);
        set(
            CGEventField::TabletProximityEventCapabilityMask,
            CAPABILITIES,
        );
        set(CGEventField::TabletProximityEventPointerType, POINTER_PEN);
        set(
            CGEventField::TabletProximityEventEnterProximity,
            i64::from(enter),
        );
    }

    fn check_proximity(checks: &mut Checks, name: &str, event: &CGEvent, enter: bool) {
        for (field, label, value) in [
            (CGEventField::TabletProximityEventVendorID, "vendor", 0x56a),
            (
                CGEventField::TabletProximityEventDeviceID,
                "device",
                DEVICE_ID,
            ),
            (
                CGEventField::TabletProximityEventCapabilityMask,
                "caps",
                CAPABILITIES,
            ),
            (
                CGEventField::TabletProximityEventPointerType,
                "pointer",
                POINTER_PEN,
            ),
            (
                CGEventField::TabletProximityEventEnterProximity,
                "enter",
                i64::from(enter),
            ),
        ] {
            checks.eq(&format!("{name} {label}"), value, int(event, field));
        }
    }

    /// The wire's tilt is in degrees; CGEvent's is -1..1. Qt reads ±1 as
    /// ±60°, so this is the scale Krita and other Qt apps expect.
    fn tilt_unit(degrees: i16) -> f64 {
        (f64::from(degrees) / 60.0).clamp(-1.0, 1.0)
    }

    struct PenPoint {
        pressure: f64,
        tilt_x: f64,
        tilt_y: f64,
        rotation: f64,
    }

    fn set_point(event: &CGEvent, at: CGPoint, pen: &PenPoint, buttons: i64) {
        let set_int = |field, value| CGEvent::set_integer_value_field(Some(event), field, value);
        let set_double = |field, value| CGEvent::set_double_value_field(Some(event), field, value);
        set_int(CGEventField::TabletEventPointX, at.x.round() as i64);
        set_int(CGEventField::TabletEventPointY, at.y.round() as i64);
        set_int(CGEventField::TabletEventPointButtons, buttons);
        set_double(CGEventField::TabletEventPointPressure, pen.pressure);
        set_double(CGEventField::MouseEventPressure, pen.pressure);
        set_double(CGEventField::TabletEventTiltX, pen.tilt_x);
        set_double(CGEventField::TabletEventTiltY, pen.tilt_y);
        set_double(CGEventField::TabletEventRotation, pen.rotation);
        set_int(CGEventField::TabletEventDeviceID, DEVICE_ID);
    }

    fn check_point(checks: &mut Checks, name: &str, event: &CGEvent, pen: &PenPoint) {
        checks.near(
            &format!("{name} pressure"),
            pen.pressure,
            double(event, CGEventField::TabletEventPointPressure),
            1e-4,
        );
        // A pure tablet event is not a mouse event and has no mouse
        // pressure; on a mouse event it is 8-bit (n/255).
        if CGEvent::r#type(Some(event)) != CGEventType::TabletPointer {
            checks.near(
                &format!("{name} mouse pressure"),
                pen.pressure,
                double(event, CGEventField::MouseEventPressure),
                0.5 / 255.0,
            );
        }
        checks.near(
            &format!("{name} tilt x"),
            pen.tilt_x,
            double(event, CGEventField::TabletEventTiltX),
            1e-4,
        );
        checks.near(
            &format!("{name} tilt y"),
            pen.tilt_y,
            double(event, CGEventField::TabletEventTiltY),
            1e-4,
        );
        checks.near(
            &format!("{name} rotation"),
            pen.rotation,
            double(event, CGEventField::TabletEventRotation),
            1e-3,
        );
        checks.eq(
            &format!("{name} device"),
            DEVICE_ID,
            int(event, CGEventField::TabletEventDeviceID),
        );
    }

    fn describe_point(event: &CGEvent) -> String {
        format!(
            "subtype={} pressure={:.6} mouse_pressure={:.6} tilt=({:.6},{:.6}) rotation={:.4} point=({},{}) buttons={} device={:#x}",
            int(event, CGEventField::MouseEventSubtype),
            double(event, CGEventField::TabletEventPointPressure),
            double(event, CGEventField::MouseEventPressure),
            double(event, CGEventField::TabletEventTiltX),
            double(event, CGEventField::TabletEventTiltY),
            double(event, CGEventField::TabletEventRotation),
            int(event, CGEventField::TabletEventPointX),
            int(event, CGEventField::TabletEventPointY),
            int(event, CGEventField::TabletEventPointButtons),
            int(event, CGEventField::TabletEventDeviceID),
        )
    }

    // Carbon text input sources, read-only: list a layout without enabling
    // it, and ask what a key position types under it.
    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn TISCreateInputSourceList(properties: *const c_void, include_all: u8) -> *const c_void;
        fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
        fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
        static kTISPropertyInputSourceID: *const c_void;
        static kTISPropertyUnicodeKeyLayoutData: *const c_void;
        fn UCKeyTranslate(
            layout: *const c_void,
            virtual_key: u16,
            action: u16,
            modifier_state: u32,
            keyboard_type: u32,
            options: u32,
            dead_key_state: *mut u32,
            max_length: usize,
            actual_length: *mut usize,
            chars: *mut u16,
        ) -> i32;
        fn LMGetKbdType() -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFRelease(cf: *const c_void);
    }

    fn layout_data(id: &str) -> Option<*const c_void> {
        use core_foundation::base::TCFType;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::string::CFString;
        unsafe {
            let filter = CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kTISPropertyInputSourceID.cast()),
                CFString::new(id),
            )]);
            let list = TISCreateInputSourceList(filter.as_concrete_TypeRef().cast(), 1);
            if list.is_null() || CFArrayGetCount(list) == 0 {
                return None;
            }
            let source = CFArrayGetValueAtIndex(list, 0);
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            // The list keeps the source and its data alive; leak it for the
            // life of this probe.
            (!data.is_null()).then(|| CFDataGetBytePtr(data).cast())
        }
    }

    fn translate(layout: *const c_void, kvk: u16, shift: bool) -> String {
        let mut dead = 0;
        let mut chars = [0u16; 8];
        let mut length = 0;
        let status = unsafe {
            UCKeyTranslate(
                layout,
                kvk,
                0,                            // kUCKeyActionDown
                if shift { 0x02 } else { 0 }, // (shiftKey >> 8) & 0xff
                u32::from(LMGetKbdType()),
                1, // kUCKeyTranslateNoDeadKeysMask
                &mut dead,
                chars.len(),
                &mut length,
                chars.as_mut_ptr(),
            )
        };
        if status != 0 {
            return format!("<{status}>");
        }
        String::from_utf16_lossy(&chars[..length])
    }

    pub fn main() {
        let mut checks = Checks::default();
        let source = CGEventSource::new(CGEventSourceStateID::Private).expect("private source");
        println!(
            "source keyboard_type={} pid={}",
            CGEventSource::keyboard_type(Some(&source)),
            std::process::id()
        );

        println!(
            "\n## Mouse\n\n| event | type | location | flags | fields |\n|---|---|---|---|---|"
        );
        let at = CGPoint::new(-1016.25, 12.5);
        let left = CGMouseButton::Left;
        let right = CGMouseButton::Right;
        mouse(
            &mut checks,
            &source,
            "move",
            CGEventType::MouseMoved,
            left,
            at,
        );
        mouse(
            &mut checks,
            &source,
            "left down",
            CGEventType::LeftMouseDown,
            left,
            at,
        );
        mouse(
            &mut checks,
            &source,
            "left drag",
            CGEventType::LeftMouseDragged,
            left,
            at,
        );
        mouse(
            &mut checks,
            &source,
            "left up",
            CGEventType::LeftMouseUp,
            left,
            at,
        );
        mouse(
            &mut checks,
            &source,
            "right down",
            CGEventType::RightMouseDown,
            right,
            at,
        );
        mouse(
            &mut checks,
            &source,
            "right drag",
            CGEventType::RightMouseDragged,
            right,
            at,
        );
        mouse(
            &mut checks,
            &source,
            "right up",
            CGEventType::RightMouseUp,
            right,
            at,
        );
        let middle = CGMouseButton::Center;
        mouse(
            &mut checks,
            &source,
            "middle down",
            CGEventType::OtherMouseDown,
            middle,
            at,
        );
        mouse(
            &mut checks,
            &source,
            "middle up",
            CGEventType::OtherMouseUp,
            middle,
            at,
        );
        let with_flags = mouse(
            &mut checks,
            &source,
            "cmd click",
            CGEventType::LeftMouseDown,
            left,
            at,
        );
        CGEvent::set_flags(
            Some(&with_flags),
            objc2_core_graphics::CGEventFlags(keymap::FLAG_COMMAND | 0x8),
        );
        row("cmd click, flags set", &with_flags, "");
        checks.eq(
            "cmd click flags",
            keymap::FLAG_COMMAND | 0x8,
            CGEvent::flags(Some(&with_flags)).0,
        );

        println!(
            "\n## Scroll\n\n| event | type | location | flags | fields |\n|---|---|---|---|---|"
        );
        for (name, unit, dy, dx) in [
            ("pixels", CGScrollEventUnit::Pixel, -40, 10),
            ("lines", CGScrollEventUnit::Line, 3, -1),
        ] {
            let event = CGEvent::new_scroll_wheel_event2(Some(&source), unit, 2, dy, dx, 0)
                .expect("scroll");
            CGEvent::set_location(Some(&event), at);
            checks.eq(
                &format!("scroll {name} type"),
                CGEventType::ScrollWheel.0,
                CGEvent::r#type(Some(&event)).0,
            );
            let fields = [
                (
                    "delta1",
                    int(&event, CGEventField::ScrollWheelEventDeltaAxis1),
                ),
                (
                    "delta2",
                    int(&event, CGEventField::ScrollWheelEventDeltaAxis2),
                ),
                (
                    "point1",
                    int(&event, CGEventField::ScrollWheelEventPointDeltaAxis1),
                ),
                (
                    "point2",
                    int(&event, CGEventField::ScrollWheelEventPointDeltaAxis2),
                ),
                (
                    "fixed1",
                    int(&event, CGEventField::ScrollWheelEventFixedPtDeltaAxis1),
                ),
                (
                    "fixed2",
                    int(&event, CGEventField::ScrollWheelEventFixedPtDeltaAxis2),
                ),
                (
                    "continuous",
                    int(&event, CGEventField::ScrollWheelEventIsContinuous),
                ),
                (
                    "phase",
                    int(&event, CGEventField::ScrollWheelEventScrollPhase),
                ),
                (
                    "momentum",
                    int(&event, CGEventField::ScrollWheelEventMomentumPhase),
                ),
            ];
            let text: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
            row(&format!("scroll {name}"), &event, &text.join(" "));
            if name == "pixels" {
                checks.eq("scroll pixels point1", i64::from(dy), fields[2].1);
                checks.eq("scroll pixels point2", i64::from(dx), fields[3].1);
                checks.eq("scroll pixels continuous", 1, fields[6].1);
            } else {
                checks.eq("scroll lines delta1", i64::from(dy), fields[0].1);
                checks.eq("scroll lines delta2", i64::from(dx), fields[1].1);
                checks.eq("scroll lines continuous", 0, fields[6].1);
            }
        }

        println!("\n## Keys\n");
        let mut modifiers = ModifierState::default();
        let mut key_rows = Vec::new();
        for usage in (0x04..=0x65).chain(0xe0..=0xe7) {
            let kvk = keymap::hid_to_kvk(usage).unwrap();
            for down in [true, false] {
                let is_modifier = modifiers.update(kvk, down);
                // Modifiers and Caps Lock are created as FlagsChanged.
                let event = CGEvent::new_keyboard_event(Some(&source), kvk, down).expect("key");
                let created_type = CGEvent::r#type(Some(&event)).0;
                if is_modifier {
                    CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
                }
                let flags = modifiers.flags_for(kvk);
                CGEvent::set_flags(Some(&event), objc2_core_graphics::CGEventFlags(flags));
                checks.eq(
                    &format!("usage {usage:#04x} keycode"),
                    i64::from(kvk),
                    int(&event, CGEventField::KeyboardEventKeycode),
                );
                checks.eq(
                    &format!("usage {usage:#04x} flags"),
                    flags,
                    CGEvent::flags(Some(&event)).0,
                );
                let expected_type = match (is_modifier, down) {
                    (true, _) => CGEventType::FlagsChanged.0,
                    (false, true) => CGEventType::KeyDown.0,
                    (false, false) => CGEventType::KeyUp.0,
                };
                checks.eq(
                    &format!("usage {usage:#04x} type"),
                    expected_type,
                    CGEvent::r#type(Some(&event)).0,
                );
                if down {
                    key_rows.push(format!(
                        "| {usage:#04x} | {kvk:#04x} | {created_type} | {} | {flags:#x} | {:?} |",
                        CGEvent::r#type(Some(&event)).0,
                        String::from_utf16_lossy(&unicode(&event))
                    ));
                }
            }
            if kvk == keymap::KVK_CAPS_LOCK {
                // Toggle Caps Lock back off for the keys after it.
                modifiers.update(kvk, true);
                modifiers.update(kvk, false);
            }
        }
        println!(
            "Every usage: keycode, flags and type read back as set; failures, if any, print above."
        );
        println!(
            "Held modifiers at the end (all released): {:#x}",
            modifiers.flags_for(0)
        );
        println!("\n| usage | kVK | created type | final type | flags | auto unicode |\n|---|---|---|---|---|---|");
        for line in key_rows.iter().filter(|l| {
            [
                "0x04 ", "0x14 ", "0x1e ", "0x28 ", "0x2a ", "0x3a ", "0x4c ", "0x52 ", "0x58 ",
                "0x64 ", "0xe1 ", "0xe3 ", "0xe7 ",
            ]
            .iter()
            .any(|u| l.starts_with(&format!("| {u}")))
        }) {
            println!("{line}");
        }

        // A chord read back: Command+Shift+Z down.
        let mut chord = ModifierState::default();
        chord.update(keymap::hid_to_kvk(0xe3).unwrap(), true);
        chord.update(keymap::hid_to_kvk(0xe1).unwrap(), true);
        let z = keymap::hid_to_kvk(0x1d).unwrap();
        let event = CGEvent::new_keyboard_event(Some(&source), z, true).unwrap();
        CGEvent::set_flags(
            Some(&event),
            objc2_core_graphics::CGEventFlags(chord.flags_for(z)),
        );
        println!(
            "\nCmd+Shift+Z: keycode={:#x} flags={:#x} unicode={:?}",
            int(&event, CGEventField::KeyboardEventKeycode),
            CGEvent::flags(Some(&event)).0,
            String::from_utf16_lossy(&unicode(&event))
        );

        println!("\n## Unicode text\n\n| input | UTF-16 set | read back | ok |\n|---|---|---|---|");
        for (name, units) in [
            ("é", vec![0x00e9u16]),
            ("€", vec![0x20ac]),
            ("😀 as one event", vec![0xd83d, 0xde00]),
            ("😀 high half alone", vec![0xd83d]),
            ("😀 low half alone", vec![0xde00]),
            ("👍🏽 (4 units)", "👍🏽".encode_utf16().collect()),
            (
                "25 units",
                "abcdefghijklmnopqrstuvwxy".encode_utf16().collect(),
            ),
        ] {
            let event = CGEvent::new_keyboard_event(Some(&source), 0, true).unwrap();
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr())
            };
            let read = unicode(&event);
            println!(
                "| {name} | {:x?} | {:x?} | {} |",
                units,
                read,
                if read == units { "yes" } else { "NO" }
            );
        }

        println!("\n## Tablet\n");
        let pen = PenPoint {
            pressure: 0.734,
            tilt_x: tilt_unit(25),
            tilt_y: tilt_unit(-40),
            rotation: 47.5,
        };
        println!(
            "wire tilt (25°, -40°) -> CGEvent ({:.4}, {:.4}) at ±60° full scale",
            pen.tilt_x, pen.tilt_y
        );
        let pen_at = CGPoint::new(3440.0 + 667.25, 250.0 + 469.75);

        // Proximity as a pure tablet event and as a mouse event subtype.
        let pure = CGEvent::new(Some(&source)).unwrap();
        CGEvent::set_type(Some(&pure), CGEventType::TabletProximity);
        CGEvent::set_location(Some(&pure), pen_at);
        set_proximity(&pure, true);
        check_proximity(&mut checks, "pure proximity enter", &pure, true);
        checks.eq(
            "pure proximity type",
            CGEventType::TabletProximity.0,
            CGEvent::r#type(Some(&pure)).0,
        );
        println!(
            "pure proximity enter: type={} enter={} pointer={} caps={:#x} device={:#x}",
            CGEvent::r#type(Some(&pure)).0,
            int(&pure, CGEventField::TabletProximityEventEnterProximity),
            int(&pure, CGEventField::TabletProximityEventPointerType),
            int(&pure, CGEventField::TabletProximityEventCapabilityMask),
            int(&pure, CGEventField::TabletProximityEventDeviceID)
        );
        let as_mouse =
            CGEvent::new_mouse_event(Some(&source), CGEventType::MouseMoved, pen_at, left).unwrap();
        CGEvent::set_integer_value_field(
            Some(&as_mouse),
            CGEventField::MouseEventSubtype,
            i64::from(CGEventMouseSubtype::TabletProximity.0),
        );
        set_proximity(&as_mouse, true);
        check_proximity(&mut checks, "mouse proximity enter", &as_mouse, true);
        println!(
            "mouse proximity enter: type={} subtype={} enter={} device={:#x}",
            CGEvent::r#type(Some(&as_mouse)).0,
            int(&as_mouse, CGEventField::MouseEventSubtype),
            int(&as_mouse, CGEventField::TabletProximityEventEnterProximity),
            int(&as_mouse, CGEventField::TabletProximityEventDeviceID)
        );

        // Points as mouse events with the tablet-point subtype, set first.
        for (name, kind, buttons) in [
            ("pen down", CGEventType::LeftMouseDown, 1),
            ("pen drag", CGEventType::LeftMouseDragged, 1),
            ("pen up", CGEventType::LeftMouseUp, 0),
        ] {
            let event = CGEvent::new_mouse_event(Some(&source), kind, pen_at, left).unwrap();
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::MouseEventSubtype,
                i64::from(CGEventMouseSubtype::TabletPoint.0),
            );
            set_point(&event, pen_at, &pen, buttons);
            check_point(&mut checks, name, &event, &pen);
            checks.eq(
                &format!("{name} subtype"),
                i64::from(CGEventMouseSubtype::TabletPoint.0),
                int(&event, CGEventField::MouseEventSubtype),
            );
            println!("{name}: type={} {}", kind.0, describe_point(&event));
        }

        // The same point as a pure tablet-pointer event.
        let pointer = CGEvent::new(Some(&source)).unwrap();
        CGEvent::set_type(Some(&pointer), CGEventType::TabletPointer);
        CGEvent::set_location(Some(&pointer), pen_at);
        set_point(&pointer, pen_at, &pen, 1);
        check_point(&mut checks, "pure tablet pointer", &pointer, &pen);
        println!(
            "pure tablet pointer: type={} {}",
            CGEvent::r#type(Some(&pointer)).0,
            describe_point(&pointer)
        );

        // Order matters? Fields first, subtype last.
        let late =
            CGEvent::new_mouse_event(Some(&source), CGEventType::LeftMouseDragged, pen_at, left)
                .unwrap();
        set_point(&late, pen_at, &pen, 1);
        CGEvent::set_integer_value_field(
            Some(&late),
            CGEventField::MouseEventSubtype,
            i64::from(CGEventMouseSubtype::TabletPoint.0),
        );
        println!("subtype set last: {}", describe_point(&late));

        // Fields on a mouse event that never gets the tablet subtype.
        let plain =
            CGEvent::new_mouse_event(Some(&source), CGEventType::LeftMouseDragged, pen_at, left)
                .unwrap();
        set_point(&plain, pen_at, &pen, 1);
        println!("no subtype: {}", describe_point(&plain));

        // Pressure resolution.
        let probe =
            CGEvent::new_mouse_event(Some(&source), CGEventType::LeftMouseDragged, pen_at, left)
                .unwrap();
        CGEvent::set_integer_value_field(
            Some(&probe),
            CGEventField::MouseEventSubtype,
            i64::from(CGEventMouseSubtype::TabletPoint.0),
        );
        let mut worst: f64 = 0.0;
        for step in 0..=1000 {
            let wanted = f64::from(step) / 1000.0;
            CGEvent::set_double_value_field(
                Some(&probe),
                CGEventField::TabletEventPointPressure,
                wanted,
            );
            let got = double(&probe, CGEventField::TabletEventPointPressure);
            worst = worst.max((got - wanted).abs());
        }
        println!("pressure 0..1 in 1/1000 steps: worst readback error {worst:.3e}");
        let mut worst_tilt: f64 = 0.0;
        for degrees in -90..=90 {
            let wanted = tilt_unit(degrees);
            CGEvent::set_double_value_field(Some(&probe), CGEventField::TabletEventTiltX, wanted);
            worst_tilt =
                worst_tilt.max((double(&probe, CGEventField::TabletEventTiltX) - wanted).abs());
        }
        println!("tilt -90..90 degrees: worst readback error {worst_tilt:.3e}");

        let exit = CGEvent::new(Some(&source)).unwrap();
        CGEvent::set_type(Some(&exit), CGEventType::TabletProximity);
        set_proximity(&exit, false);
        check_proximity(&mut checks, "pure proximity exit", &exit, false);

        println!("\n## Displays\n\n| id | bounds (points) | mode points | mode pixels | rotation | corners map inside |\n|---|---|---|---|---|---|");
        let mut ids = [0u32; 16];
        let mut count = 0;
        let status = unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) };
        println!("CGGetActiveDisplayList status={} count={count}\n", status.0);
        for &id in &ids[..count as usize] {
            let rect = CGDisplayBounds(id);
            let mode = CGDisplayCopyDisplayMode(id);
            let (points, pixels) = mode.as_deref().map_or(((0, 0), (0, 0)), |m| {
                (
                    (
                        CGDisplayMode::width(Some(m)),
                        CGDisplayMode::height(Some(m)),
                    ),
                    (
                        CGDisplayMode::pixel_width(Some(m)),
                        CGDisplayMode::pixel_height(Some(m)),
                    ),
                )
            });
            let bounds = DisplayBounds {
                x: rect.origin.x,
                y: rect.origin.y,
                width: rect.size.width,
                height: rect.size.height,
            };
            let capture = CapturePixels {
                width: pixels.0 as u32,
                height: pixels.1 as u32,
            };
            let mut inside = true;
            for (nx, ny) in [
                (0, 0),
                (65_535, 0),
                (0, 65_535),
                (65_535, 65_535),
                (32_767, 32_767),
            ] {
                let (x, y) = display_map::norm_to_display_point(nx, ny, bounds);
                inside &= bounds.contains(x, y);
                if capture.width > 0 {
                    checks.eq(
                        &format!("display {id} corner ({nx},{ny}) pixel"),
                        display_map::norm_to_capture_pixel(nx, ny, capture),
                        display_map::point_to_capture_pixel(x, y, bounds, capture),
                    );
                }
            }
            checks.eq(&format!("display {id} corners inside"), true, inside);
            println!(
                "| {id} | ({}, {}) {}x{} | {}x{} | {}x{} | {}° | {} |",
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                points.0,
                points.1,
                pixels.0,
                pixels.1,
                CGDisplayRotation(id),
                if inside { "yes" } else { "NO" }
            );
        }

        println!("\n## Non-US layouts (UCKeyTranslate, read-only)\n");
        let current = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
        if !current.is_null() {
            let id = unsafe { TISGetInputSourceProperty(current, kTISPropertyInputSourceID) };
            let id = unsafe {
                use core_foundation::base::TCFType;
                core_foundation::string::CFString::wrap_under_get_rule(id.cast())
            };
            println!("current layout: {id}");
            unsafe { CFRelease(current) };
        }
        let positions = [
            (0x04u16, "A"),
            (0x14, "Q"),
            (0x1a, "W"),
            (0x1c, "Y"),
            (0x1d, "Z"),
            (0x1e, "1"),
            (0x33, ";"),
            (0x2d, "-"),
            (0x35, "`"),
            (0x64, "ISO §"),
        ];
        print!("| layout |");
        for (_, name) in positions {
            print!(" {name} |");
        }
        println!("\n|---|{}", "---|".repeat(positions.len()));
        for id in [
            "com.apple.keylayout.US",
            "com.apple.keylayout.French",
            "com.apple.keylayout.German",
            "com.apple.keylayout.Dvorak",
        ] {
            let Some(layout) = layout_data(id) else {
                println!("| {id} | not found |");
                continue;
            };
            print!("| {} |", id.trim_start_matches("com.apple.keylayout."));
            for (usage, _) in positions {
                let kvk = keymap::hid_to_kvk(usage).unwrap();
                print!(
                    " {} / {} |",
                    translate(layout, kvk, false),
                    translate(layout, kvk, true)
                );
            }
            println!();
        }

        println!(
            "\nRESULT checks_passed={} checks_failed={}",
            checks.passed,
            checks.failed.len()
        );
        for line in &checks.failed {
            println!("failed: {line}");
        }
    }
}
