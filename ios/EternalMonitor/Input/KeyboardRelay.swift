import Foundation

/// Pure keyboard state. The connection assigns event IDs to all input kinds.
struct KeyboardRelayMachine {
    private(set) var sticky = Set<UInt16>()
    private var pressed = Set<UInt16>()
    private var hardwareModifiers: [UInt16: UInt16] = [:]
    private var inferredModifiers = Set<UInt16>()

    static func mappedUsage(_ usage: UInt16, commandAsControl: Bool) -> UInt16 {
        if commandAsControl && usage == 0xe3 { return 0xe0 }
        if commandAsControl && usage == 0xe7 { return 0xe4 }
        return usage
    }

    static func isSupported(_ usage: UInt16) -> Bool {
        (4...0x65).contains(usage) || (0xe0...0xe7).contains(usage)
    }

    static func modifierBit(_ usage: UInt16) -> UInt8 {
        switch usage {
        case 0xe1, 0xe5: return 1
        case 0xe0, 0xe4: return 2
        case 0xe2, 0xe6: return 4
        case 0xe3, 0xe7: return 8
        default: return 0
        }
    }

    private func event(_ usage: UInt16, down: Bool) -> WireInputEvent {
        WireInputEvent(kind: 4, phase: down ? 0 : 2, buttons: 0, eventId: 0,
            xNorm: 0, yNorm: 0, keycode: usage,
            modifiers: (Array(sticky) + Array(hardwareModifiers.values)).reduce(0) { $0 | Self.modifierBit($1) },
            clientTimeUs: 0)
    }

    mutating func hardware(usage: UInt16, down: Bool, modifiers: UInt8,
                           commandAsControl: Bool) -> [WireInputEvent] {
        guard Self.isSupported(usage) else { return [] }
        var result: [WireInputEvent] = []
        let before = Set(hardwareModifiers.values).union(sticky)
        let bit = Self.modifierBit(usage)
        var flags = modifiers
        if bit != 0 {
            if down {
                for key in inferredModifiers.filter({ Self.modifierBit($0) == bit }) {
                    hardwareModifiers.removeValue(forKey: key)
                    inferredModifiers.remove(key)
                }
                if hardwareModifiers[usage] == nil {
                    hardwareModifiers[usage] = Self.mappedUsage(usage, commandAsControl: commandAsControl)
                }
                flags |= bit
            } else {
                hardwareModifiers.removeValue(forKey: usage)
                inferredModifiers.remove(usage)
                if !hardwareModifiers.keys.contains(where: { Self.modifierBit($0) == bit }) { flags &= ~bit }
            }
        }
        // UIKit may report a modifier only through another key's flags.
        // Translate it into a real edge so Shift/Ctrl chords work on Windows.
        for (mask, fallback): (UInt8, UInt16) in [(1, 0xe1), (2, 0xe0), (4, 0xe2), (8, 0xe3)] {
            let keys = hardwareModifiers.keys.filter { Self.modifierBit($0) == mask }
            if flags & mask != 0 && keys.isEmpty {
                hardwareModifiers[fallback] = Self.mappedUsage(fallback, commandAsControl: commandAsControl)
                inferredModifiers.insert(fallback)
            } else if flags & mask == 0 {
                for key in keys {
                    hardwareModifiers.removeValue(forKey: key)
                    inferredModifiers.remove(key)
                }
            }
        }
        let after = Set(hardwareModifiers.values).union(sticky)
        result += before.subtracting(after).sorted().reversed().map { event($0, down: false) }
        result += after.subtracting(before).sorted().map { event($0, down: true) }
        guard bit == 0 else { return result }
        if down {
            pressed.insert(usage)
            result.append(event(usage, down: true))
        } else if pressed.remove(usage) != nil {
            result.append(event(usage, down: false))
            result += releaseSticky()
        }
        return result
    }

    mutating func toggleSticky(_ usage: UInt16) -> [WireInputEvent] {
        guard Self.modifierBit(usage) != 0 else { return [] }
        let down = !sticky.contains(usage)
        if down { sticky.insert(usage) } else { sticky.remove(usage) }
        return hardwareModifiers.values.contains(usage) ? [] : [event(usage, down: down)]
    }

    private mutating func releaseSticky() -> [WireInputEvent] {
        let held = sticky.sorted().reversed()
        sticky.removeAll()
        return held.filter { !hardwareModifiers.values.contains($0) }.map { event($0, down: false) }
    }

    mutating func tap(_ usage: UInt16) -> [WireInputEvent] {
        guard Self.isSupported(usage) else { return [] }
        return [event(usage, down: true), event(usage, down: false)] + releaseSticky()
    }

    /// ASCII physical keys for shortcuts from the software keyboard.
    static func asciiKey(_ unit: UInt16) -> (usage: UInt16, shift: Bool)? {
        switch unit {
        case 65...90: return (unit - 65 + 4, true)
        case 97...122: return (unit - 97 + 4, false)
        case 49...57: return (unit - 49 + 0x1e, false)
        case 48: return (0x27, false)
        case 32: return (0x2c, false)
        default:
            let plain = Array("-=[]\\;'`,./".utf16)
            let shifted = Array("_+{}|:\"~<>?".utf16)
            let codes: [UInt16] = [0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38]
            if let i = plain.firstIndex(of: unit) { return (codes[i], false) }
            if let i = shifted.firstIndex(of: unit) { return (codes[i], true) }
            let digits = Array("!@#$%^&*()".utf16)
            if let i = digits.firstIndex(of: unit) { return (UInt16(0x1e + i), true) }
            return nil
        }
    }

    mutating func text(_ text: String) -> [WireInputEvent] {
        var result: [WireInputEvent] = []
        for unit in text.replacingOccurrences(of: "\r\n", with: "\n").utf16 {
            if unit == 10 || unit == 13 { result += tap(0x28) }
            else if unit == 9 { result += tap(0x2b) }
            else if !sticky.isEmpty, let key = Self.asciiKey(unit) {
                let shiftHeld = sticky.union(hardwareModifiers.values).contains { Self.modifierBit($0) == 1 }
                let addShift = key.shift && !shiftHeld
                if addShift { result.append(event(0xe1, down: true)) }
                result += [event(key.usage, down: true), event(key.usage, down: false)]
                if addShift { result.append(event(0xe1, down: false)) }
                result += releaseSticky()
            } else {
                result.append(WireInputEvent(kind: 5, phase: 0, buttons: 0, eventId: 0,
                    xNorm: 0, yNorm: 0, keycode: unit, clientTimeUs: 0))
                result += releaseSticky()
            }
        }
        return result
    }

    mutating func cancel() -> [WireInputEvent] {
        let keys = pressed.union(sticky).union(hardwareModifiers.values).sorted().reversed()
        pressed.removeAll(); sticky.removeAll(); hardwareModifiers.removeAll(); inferredModifiers.removeAll()
        return keys.map { event($0, down: false) }
    }
}

/// One ID sequence across touch, keyboard and pointer sources, including
/// view recreation during a session. Retransmitted edges keep the same ID.
struct InputEventSequencer {
    private var next: UInt32 = 0
    mutating func packets(_ events: [WireInputEvent], timeUs: UInt64, reliable: Bool = false) -> [WireInputEvent] {
        events.flatMap { source in
            next &+= 1
            var event = source
            event.eventId = next
            if event.kind != 1 || event.inputVer != 2 || event.clientTimeUs == 0 { event.clientTimeUs = timeUs }
            return reliable || event.phase == 1 ? [event] : [event, event]
        }
    }
}
