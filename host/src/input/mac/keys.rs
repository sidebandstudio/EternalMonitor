//! USB keyboard page 0x07 to macOS virtual key codes (`kVK_*` in Carbon's
//! `HIToolbox/Events.h`), and the `CGEventFlags` a synthesized key event
//! carries.
//!
//! The table covers exactly the usages the client sends
//! (`KeyboardRelayMachine.isSupported`): 0x04..=0x65 and the modifiers
//! 0xe0..=0xe7. Keys that Mac keyboards lack take the code macOS itself gives
//! a PC keyboard: Insert is Help, Num Lock is keypad Clear, and Print Screen,
//! Scroll Lock and Pause are F13, F14 and F15.

/// Returns the `kVK_*` code for a HID usage.
pub fn hid_to_kvk(usage: u16) -> Option<u16> {
    const KEYS: [u16; 98] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06, // A..Z
        0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19, 0x1d, // 1..0
        0x24, 0x35, 0x33, 0x30, 0x31, 0x1b, 0x18, 0x21, 0x1e, 0x2a, 0x2a, 0x29, 0x27, 0x32, 0x2b,
        0x2f, 0x2c, 0x39, // Enter..Caps Lock
        0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, // F1..F12
        0x69, 0x6b, 0x71, // Print Screen, Scroll Lock, Pause as F13..F15
        0x72, 0x73, 0x74, 0x75, 0x77, 0x79, 0x7c, 0x7b, 0x7d, 0x7e, // Insert..Up
        0x47, 0x4b, 0x43, 0x4e, 0x45, 0x4c, // Num Lock, keypad operators, keypad Enter
        0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5b, 0x5c, 0x52, 0x41, // keypad 1..9, 0, .
        0x0a, 0x6e, // Non-US backslash (ISO section), Application (contextual menu)
    ];
    // Left Control, Shift, Option, Command, then the right-hand four.
    const MODIFIERS: [u16; 8] = [0x3b, 0x38, 0x3a, 0x37, 0x3e, 0x3c, 0x3d, 0x36];
    match usage {
        0x04..=0x65 => Some(KEYS[(usage - 4) as usize]),
        0xe0..=0xe7 => Some(MODIFIERS[(usage - 0xe0) as usize]),
        _ => None,
    }
}

/// `kCGEventFlagMask*` values from `CGEventTypes.h`.
pub const FLAG_SHIFT: u64 = 0x0002_0000;
pub const FLAG_CONTROL: u64 = 0x0004_0000;
pub const FLAG_ALTERNATE: u64 = 0x0008_0000;
pub const FLAG_COMMAND: u64 = 0x0010_0000;
pub const FLAG_NUMERIC_PAD: u64 = 0x0020_0000;
pub const FLAG_SECONDARY_FN: u64 = 0x0080_0000;

/// The flag a held modifier key contributes: the device-independent mask
/// plus its side bit (`NX_DEVICE*KEYMASK` in `IOLLEvent.h`), which apps use
/// to tell left from right. Caps Lock is a toggle, not a held modifier, so it
/// has none.
pub fn modifier_flag(kvk: u16) -> Option<u64> {
    Some(match kvk {
        0x3b => FLAG_CONTROL | 0x0001,
        0x38 => FLAG_SHIFT | 0x0002,
        0x3c => FLAG_SHIFT | 0x0004,
        0x37 => FLAG_COMMAND | 0x0008,
        0x36 => FLAG_COMMAND | 0x0010,
        0x3a => FLAG_ALTERNATE | 0x0020,
        0x3d => FLAG_ALTERNATE | 0x0040,
        0x3e => FLAG_CONTROL | 0x2000,
        _ => return None,
    })
}

/// Flags a hardware keyboard sets on a key's own events regardless of
/// modifiers: keypad keys and arrows report `NumericPad`; F-keys and the
/// navigation keys (Help, Home, End, Page Up/Down, Forward Delete, arrows)
/// report `SecondaryFn`, as `NSEvent.ModifierFlags.function` documents.
pub fn key_flags(kvk: u16) -> u64 {
    let numeric_pad = matches!(
        kvk,
        0x41 | 0x43 | 0x45 | 0x47 | 0x4b | 0x4c | 0x4e | 0x51..=0x59 | 0x5b | 0x5c | 0x7b..=0x7e
    );
    let function = matches!(
        kvk,
        0x60..=0x65 | 0x67 | 0x69 | 0x6b | 0x6d | 0x6f | 0x71..=0x7e
    );
    (if numeric_pad { FLAG_NUMERIC_PAD } else { 0 })
        | (if function { FLAG_SECONDARY_FN } else { 0 })
}

/// `kCGEventFlagMaskAlphaShift`: Caps Lock is on.
pub const FLAG_ALPHA_SHIFT: u64 = 0x0001_0000;
pub const KVK_CAPS_LOCK: u16 = 0x39;

/// Held modifiers and the Caps Lock toggle, so every synthesized event
/// carries the flags a real keyboard would. Events from a private
/// `CGEventSource` start with none.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ModifierState {
    /// Flags of the modifier keys held now, side bits included.
    held: u64,
    caps_lock: bool,
}

impl ModifierState {
    /// Records a key edge. Returns true for a modifier or Caps Lock, whose
    /// event is `kCGEventFlagsChanged` (`CGEventCreateKeyboardEvent` already
    /// gives those keys that type) rather than a key down/up.
    pub fn update(&mut self, kvk: u16, down: bool) -> bool {
        if kvk == KVK_CAPS_LOCK {
            if down {
                self.caps_lock = !self.caps_lock;
            }
            return true;
        }
        let Some(flag) = modifier_flag(kvk) else {
            return false;
        };
        let side = flag & 0xffff;
        if down {
            self.held |= side;
        } else {
            self.held &= !side;
        }
        true
    }

    /// Flags for an event of `kvk` given the modifiers held now. A shared
    /// mask stays set while either side's key is held.
    pub fn flags_for(&self, kvk: u16) -> u64 {
        let mut flags = self.held | key_flags(kvk);
        for (sides, mask) in [
            (0x0001 | 0x2000, FLAG_CONTROL),
            (0x0002 | 0x0004, FLAG_SHIFT),
            (0x0008 | 0x0010, FLAG_COMMAND),
            (0x0020 | 0x0040, FLAG_ALTERNATE),
        ] {
            if self.held & sides != 0 {
                flags |= mask;
            }
        }
        if self.caps_lock {
            flags |= FLAG_ALPHA_SHIFT;
        }
        flags
    }

    /// Release every held key when a session ends. Caps Lock is a toggle
    /// on the Mac, not a held key, so it is left as it is.
    pub fn clear(&mut self) {
        self.held = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_usage_and_only_supported_usages_map() {
        for usage in 0..=u16::MAX {
            let code = hid_to_kvk(usage);
            assert_eq!(
                code.is_some(),
                (4..=0x65).contains(&usage) || (0xe0..=0xe7).contains(&usage),
                "usage {usage:#x}"
            );
            if let Some(kvk) = code {
                assert!(kvk <= 0x7e, "usage {usage:#x} gave {kvk:#x}");
            }
        }
    }

    #[test]
    fn codes_are_distinct_except_the_two_backslash_positions() {
        let mut seen = std::collections::HashMap::new();
        for usage in (0x04..=0x65).chain(0xe0..=0xe7) {
            let kvk = hid_to_kvk(usage).unwrap();
            if let Some(previous) = seen.insert(kvk, usage) {
                // The ANSI backslash and the ISO key beside Enter are one
                // position; Windows maps both to scan 0x2b as well.
                assert_eq!((previous, usage), (0x31, 0x32), "{kvk:#x}");
            }
        }
        assert_eq!(seen.len(), 98 + 8 - 1);
    }

    #[test]
    fn known_positions_match_events_h() {
        for (usage, kvk) in [
            (0x04, 0x00), // A
            (0x16, 0x01), // S
            (0x1d, 0x06), // Z
            (0x1e, 0x12), // 1
            (0x22, 0x17), // 5
            (0x23, 0x16), // 6
            (0x27, 0x1d), // 0
            (0x28, 0x24), // Return
            (0x29, 0x35), // Escape
            (0x2a, 0x33), // Backspace is kVK_Delete
            (0x2b, 0x30), // Tab
            (0x2c, 0x31), // Space
            (0x35, 0x32), // Grave
            (0x39, 0x39), // Caps Lock
            (0x3a, 0x7a), // F1
            (0x3d, 0x76), // F4
            (0x45, 0x6f), // F12
            (0x46, 0x69), // Print Screen as F13
            (0x49, 0x72), // Insert as Help
            (0x4c, 0x75), // Delete is kVK_ForwardDelete
            (0x4f, 0x7c), // Right
            (0x50, 0x7b), // Left
            (0x51, 0x7d), // Down
            (0x52, 0x7e), // Up
            (0x53, 0x47), // Num Lock as keypad Clear
            (0x58, 0x4c), // keypad Enter
            (0x59, 0x53), // keypad 1
            (0x60, 0x5b), // keypad 8
            (0x62, 0x52), // keypad 0
            (0x63, 0x41), // keypad .
            (0x64, 0x0a), // ISO section
            (0x65, 0x6e), // contextual menu
            (0xe0, 0x3b), // left Control
            (0xe1, 0x38), // left Shift
            (0xe2, 0x3a), // left Option
            (0xe3, 0x37), // left Command
            (0xe4, 0x3e), // right Control
            (0xe5, 0x3c), // right Shift
            (0xe6, 0x3d), // right Option
            (0xe7, 0x36), // right Command
        ] {
            assert_eq!(hid_to_kvk(usage), Some(kvk), "usage {usage:#x}");
        }
    }

    #[test]
    fn keypad_and_main_row_keys_stay_distinct() {
        for (main, keypad) in [
            (0x1e, 0x59),
            (0x27, 0x62),
            (0x28, 0x58),
            (0x2d, 0x56),
            (0x37, 0x63),
        ] {
            assert_ne!(hid_to_kvk(main), hid_to_kvk(keypad));
            assert_eq!(key_flags(hid_to_kvk(main).unwrap()) & FLAG_NUMERIC_PAD, 0);
            assert_ne!(key_flags(hid_to_kvk(keypad).unwrap()) & FLAG_NUMERIC_PAD, 0);
        }
    }

    #[test]
    fn navigation_and_function_keys_carry_fn_and_letters_carry_nothing() {
        for usage in (0x3a..=0x48).chain(0x49..=0x52) {
            let kvk = hid_to_kvk(usage).unwrap();
            assert_ne!(key_flags(kvk) & FLAG_SECONDARY_FN, 0, "usage {usage:#x}");
        }
        for usage in 0x4f..=0x52 {
            assert_ne!(key_flags(hid_to_kvk(usage).unwrap()) & FLAG_NUMERIC_PAD, 0);
        }
        for usage in (0x04..=0x39).chain(0xe0..=0xe7) {
            assert_eq!(key_flags(hid_to_kvk(usage).unwrap()), 0, "usage {usage:#x}");
        }
    }

    #[test]
    fn only_the_eight_held_modifiers_have_flags() {
        let with_flags: Vec<_> = (0x04..=0x65)
            .chain(0xe0..=0xe7)
            .filter(|&usage| modifier_flag(hid_to_kvk(usage).unwrap()).is_some())
            .collect();
        assert_eq!(with_flags, (0xe0..=0xe7).collect::<Vec<_>>());
    }

    #[test]
    fn modifier_state_tracks_sides_and_shared_masks() {
        let mut state = ModifierState::default();
        let left_shift = hid_to_kvk(0xe1).unwrap();
        let right_shift = hid_to_kvk(0xe5).unwrap();
        let command = hid_to_kvk(0xe3).unwrap();
        let c = hid_to_kvk(0x06).unwrap();

        assert!(!state.update(c, true));
        assert_eq!(state.flags_for(c), 0);

        assert!(state.update(command, true));
        assert_eq!(state.flags_for(c), FLAG_COMMAND | 0x0008);

        state.update(left_shift, true);
        state.update(right_shift, true);
        state.update(left_shift, false);
        assert_eq!(
            state.flags_for(c),
            FLAG_COMMAND | 0x0008 | FLAG_SHIFT | 0x0004,
            "the right Shift still holds the shared Shift mask"
        );
        state.update(right_shift, false);
        assert_eq!(state.flags_for(c), FLAG_COMMAND | 0x0008);

        let up = hid_to_kvk(0x52).unwrap();
        assert_eq!(
            state.flags_for(up),
            FLAG_COMMAND | 0x0008 | FLAG_NUMERIC_PAD | FLAG_SECONDARY_FN
        );

        state.clear();
        assert_eq!(state.flags_for(c), 0);
    }

    #[test]
    fn caps_lock_toggles_on_press_and_survives_a_session_reset() {
        let mut state = ModifierState::default();
        let caps = hid_to_kvk(0x39).unwrap();
        let a = hid_to_kvk(0x04).unwrap();
        assert!(state.update(caps, true));
        assert!(state.update(caps, false));
        assert_eq!(state.flags_for(a), FLAG_ALPHA_SHIFT);
        state.clear();
        assert_eq!(state.flags_for(a), FLAG_ALPHA_SHIFT);
        state.update(caps, true);
        state.update(caps, false);
        assert_eq!(state.flags_for(a), 0);
    }

    #[test]
    fn a_repeated_release_cannot_clear_the_other_side() {
        let mut state = ModifierState::default();
        let left = hid_to_kvk(0xe0).unwrap();
        let right = hid_to_kvk(0xe4).unwrap();
        state.update(right, true);
        state.update(left, false);
        state.update(left, false);
        assert_eq!(state.flags_for(0x00), FLAG_CONTROL | 0x2000);
    }
}
