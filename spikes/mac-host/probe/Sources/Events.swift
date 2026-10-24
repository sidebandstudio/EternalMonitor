// Spike e: CGEvents posted into this app's own window, and what AppKit
// delivers for them. Two paths: CGEventPostToPid (only this process can
// receive it), then the HID tap the host will use, each event sent only
// while this app is active, its window is key, and the location is inside
// that window. Keys are plain characters; no ⌘ shortcut is ever sent.
import AppKit
import Carbon.HIToolbox
import CoreGraphics

/// Logs every event AppKit delivers to it.
final class EventView: NSView {
    private(set) var received: [String] = []
    private var tracking: NSTrackingArea?

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        tracking = NSTrackingArea(rect: bounds, options: [.mouseMoved, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(tracking!)
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor(white: 0.12, alpha: 1).setFill()
        bounds.fill()
        let text = received.suffix(22).joined(separator: "\n") as NSString
        text.draw(in: bounds.insetBy(dx: 8, dy: 8), withAttributes: [
            .foregroundColor: NSColor.white, .font: NSFont.monospacedSystemFont(ofSize: 11, weight: .regular),
        ])
    }

    private func record(_ event: NSEvent, _ detail: String = "") {
        let p = convert(event.locationInWindow, from: nil)
        var line = "\(event.type.rawValue):\(Self.name(event.type)) at=(\(String(format: "%.1f,%.1f", p.x, p.y)))"
        switch event.type {
        case .leftMouseDown, .leftMouseUp, .leftMouseDragged, .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp, .mouseMoved:
            line += " button=\(event.buttonNumber) clicks=\(event.clickCount) pressure=\(String(format: "%.6f", event.pressure)) subtype=\(event.subtype.rawValue)"
            if event.subtype == .tabletPoint {
                line += " tilt=(\(String(format: "%.4f,%.4f", event.tilt.x, event.tilt.y))) rotation=\(String(format: "%.2f", event.rotation)) device=\(event.deviceID)"
            }
        default: break
        }
        if !detail.isEmpty { line += " " + detail }
        received.append(line)
        Log.line("RECV \(line)")
        needsDisplay = true
    }

    static func name(_ type: NSEvent.EventType) -> String {
        switch type {
        case .leftMouseDown: "leftDown"; case .leftMouseUp: "leftUp"; case .leftMouseDragged: "leftDragged"
        case .rightMouseDown: "rightDown"; case .rightMouseUp: "rightUp"; case .otherMouseDown: "otherDown"
        case .otherMouseUp: "otherUp"; case .mouseMoved: "moved"; case .scrollWheel: "scroll"
        case .keyDown: "keyDown"; case .keyUp: "keyUp"; case .flagsChanged: "flagsChanged"
        case .tabletPoint: "tabletPoint"; case .tabletProximity: "tabletProximity"
        default: "type\(type.rawValue)"
        }
    }

    override func mouseDown(with e: NSEvent) { record(e) }
    override func mouseUp(with e: NSEvent) { record(e) }
    override func mouseDragged(with e: NSEvent) { record(e) }
    override func mouseMoved(with e: NSEvent) { record(e) }
    override func rightMouseDown(with e: NSEvent) { record(e) }
    override func rightMouseUp(with e: NSEvent) { record(e) }
    override func otherMouseDown(with e: NSEvent) { record(e) }
    override func otherMouseUp(with e: NSEvent) { record(e) }
    override func scrollWheel(with e: NSEvent) {
        record(e, String(format: "dx=%.2f dy=%.2f precise=%d phase=%lu momentum=%lu", e.scrollingDeltaX, e.scrollingDeltaY,
                         e.hasPreciseScrollingDeltas ? 1 : 0, e.phase.rawValue, e.momentumPhase.rawValue))
    }
    override func keyDown(with e: NSEvent) { record(e, keyDetail(e)) }
    override func keyUp(with e: NSEvent) { record(e, keyDetail(e)) }
    override func flagsChanged(with e: NSEvent) { record(e, keyDetail(e, characters: false)) }
    override func tabletPoint(with e: NSEvent) {
        record(e, String(format: "pressure=%.6f tilt=(%.4f,%.4f) rotation=%.2f buttons=%lu device=%lu",
                         e.pressure, e.tilt.x, e.tilt.y, e.rotation, e.buttonMask.rawValue, e.deviceID))
    }
    override func tabletProximity(with e: NSEvent) {
        record(e, "entering=\(e.isEnteringProximity) pointer=\(e.pointingDeviceType.rawValue) device=\(e.deviceID) vendor=\(e.vendorID) caps=0x\(String(e.capabilityMask, radix: 16))")
    }

    private func keyDetail(_ e: NSEvent, characters: Bool = true) -> String {
        var detail = "keyCode=0x\(String(e.keyCode, radix: 16)) flags=0x\(String(e.modifierFlags.rawValue, radix: 16))"
        if characters {
            let chars = e.characters ?? ""
            detail += " chars=\"\(chars)\" utf16=\(chars.utf16.map { String(format: "%04x", $0) }.joined(separator: " "))"
        }
        return detail
    }
}

@MainActor
struct EventSpike {
    let app: ProbeApp
    private let deviceID: Int64 = 0x4d01
    // NX_TABLET_CAPABILITY_{DEVICEID,ABSX,ABSY,BUTTONS,TILTX,TILTY,PRESSURE,ROTATION}MASK
    private let capabilities: Int64 = 0x0001 | 0x0002 | 0x0004 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x2000

    init(app: ProbeApp) { self.app = app }

    /// A point in the event view, in CG global coordinates (top-left origin).
    private func globalPoint(_ fx: CGFloat, _ fy: CGFloat) -> CGPoint {
        let view = app.events
        let local = NSPoint(x: view.bounds.width * fx, y: view.bounds.height * fy)
        let screen = app.window.convertPoint(toScreen: view.convert(local, to: nil))
        let mainHeight = NSScreen.screens.first?.frame.height ?? 0
        return CGPoint(x: screen.x, y: mainHeight - screen.y)
    }

    private func mouse(_ type: CGEventType, _ at: CGPoint, _ button: CGMouseButton = .left, _ source: CGEventSource) -> CGEvent {
        let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: at, mouseButton: button)!
        if type != .mouseMoved { event.setIntegerValueField(.mouseEventClickState, value: 1) }
        return event
    }

    private func key(_ code: CGKeyCode, _ down: Bool, _ flags: CGEventFlags = [], _ source: CGEventSource) -> CGEvent {
        let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)!
        event.flags = flags
        return event
    }

    private func unicode(_ text: String, _ down: Bool, _ source: CGEventSource) -> CGEvent {
        // keyCode 0 with a Unicode string, one event for the whole string,
        // so a surrogate pair stays together.
        let event = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)!
        let units = Array(text.utf16)
        event.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
        return event
    }

    private func proximity(_ enter: Bool, _ source: CGEventSource) -> CGEvent {
        let event = CGEvent(source: source)!
        event.type = .tabletProximity
        let fields: [(CGEventField, Int64)] = [
            (.tabletProximityEventVendorID, 0x56a), (.tabletProximityEventTabletID, 1), (.tabletProximityEventPointerID, 1),
            (.tabletProximityEventDeviceID, deviceID), (.tabletProximityEventSystemTabletID, 1),
            (.tabletProximityEventVendorPointerType, 0x0802), (.tabletProximityEventVendorPointerSerialNumber, 1),
            (.tabletProximityEventVendorUniqueID, 1), (.tabletProximityEventCapabilityMask, capabilities),
            (.tabletProximityEventPointerType, 1), (.tabletProximityEventEnterProximity, enter ? 1 : 0),
        ]
        for (field, value) in fields { event.setIntegerValueField(field, value: value) }
        return event
    }

    /// A mouse event carrying a pen point. The subtype goes first: setting
    /// it after the tablet fields wipes them (spike b).
    private func pen(_ type: CGEventType, _ at: CGPoint, pressure: Double, tilt: (Double, Double), rotation: Double, _ source: CGEventSource) -> CGEvent {
        let event = mouse(type, at, .left, source)
        event.setIntegerValueField(.mouseEventSubtype, value: Int64(CGEventMouseSubtype.tabletPoint.rawValue))
        event.setIntegerValueField(.tabletEventPointX, value: Int64(at.x.rounded()))
        event.setIntegerValueField(.tabletEventPointY, value: Int64(at.y.rounded()))
        event.setIntegerValueField(.tabletEventPointButtons, value: type == .leftMouseUp ? 0 : 1)
        event.setDoubleValueField(.tabletEventPointPressure, value: pressure)
        event.setDoubleValueField(.mouseEventPressure, value: pressure)
        event.setDoubleValueField(.tabletEventTiltX, value: tilt.0)
        event.setDoubleValueField(.tabletEventTiltY, value: tilt.1)
        event.setDoubleValueField(.tabletEventRotation, value: rotation)
        event.setIntegerValueField(.tabletEventDeviceID, value: deviceID)
        return event
    }

    private func sequence(_ source: CGEventSource) -> [(String, CGEvent)] {
        let a = globalPoint(0.3, 0.5), b = globalPoint(0.5, 0.5), c = globalPoint(0.7, 0.4)
        let scrollPixels = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2, wheel1: -40, wheel2: 10, wheel3: 0)!
        scrollPixels.location = b
        let scrollLines = CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2, wheel1: 3, wheel2: -1, wheel3: 0)!
        scrollLines.location = b
        let shift = CGEventFlags.maskShift
        let pure = CGEvent(source: source)!
        pure.type = .tabletPointer
        pure.location = c
        for (field, value) in [(CGEventField.tabletEventPointX, Int64(c.x)), (.tabletEventPointY, Int64(c.y)), (.tabletEventPointButtons, 1), (.tabletEventDeviceID, deviceID)] {
            pure.setIntegerValueField(field, value: value)
        }
        pure.setDoubleValueField(.tabletEventPointPressure, value: 0.6)
        pure.setDoubleValueField(.tabletEventTiltX, value: -0.25)
        pure.setDoubleValueField(.tabletEventTiltY, value: 0.5)
        return [
            ("move", mouse(.mouseMoved, a, .left, source)),
            ("left down", mouse(.leftMouseDown, a, .left, source)),
            ("left drag", mouse(.leftMouseDragged, b, .left, source)),
            ("left up", mouse(.leftMouseUp, b, .left, source)),
            ("right down", mouse(.rightMouseDown, b, .right, source)),
            ("right up", mouse(.rightMouseUp, b, .right, source)),
            ("scroll pixels dy=-40 dx=10", scrollPixels),
            ("scroll lines dy=3 dx=-1", scrollLines),
            ("key a down", key(CGKeyCode(kVK_ANSI_A), true, [], source)),
            ("key a up", key(CGKeyCode(kVK_ANSI_A), false, [], source)),
            ("shift down", key(CGKeyCode(kVK_Shift), true, shift, source)),
            ("key A (shift) down", key(CGKeyCode(kVK_ANSI_A), true, shift, source)),
            ("key A (shift) up", key(CGKeyCode(kVK_ANSI_A), false, shift, source)),
            ("shift up", key(CGKeyCode(kVK_Shift), false, [], source)),
            ("unicode é", unicode("é", true, source)),
            ("unicode é up", unicode("é", false, source)),
            ("unicode 😀 (one event)", unicode("😀", true, source)),
            ("unicode 😀 up", unicode("😀", false, source)),
            ("pen proximity enter", proximity(true, source)),
            ("pen down p=0.25 tilt=(0.4,-0.6) rot=47.5", pen(.leftMouseDown, a, pressure: 0.25, tilt: (0.4, -0.6), rotation: 47.5, source)),
            ("pen drag p=0.5", pen(.leftMouseDragged, b, pressure: 0.5, tilt: (0.2, -0.3), rotation: 50, source)),
            ("pen drag p=0.733989", pen(.leftMouseDragged, c, pressure: 0.733989, tilt: (0.416639, -0.666646), rotation: 47.5, source)),
            ("pen up", pen(.leftMouseUp, c, pressure: 0, tilt: (0, 0), rotation: 0, source)),
            ("pure tablet point p=0.6 tilt=(-0.25,0.5)", pure),
            ("pen proximity exit", proximity(false, source)),
        ]
    }

    /// True while an event may go to the HID tap: this app active, its
    /// window key, and the event's location inside that window.
    private func safeForHID(_ event: CGEvent) -> Bool {
        guard NSApp.isActive, app.window.isKeyWindow, NSWorkspace.shared.frontmostApplication?.processIdentifier == getpid() else { return false }
        let mainHeight = NSScreen.screens.first?.frame.height ?? 0
        let cocoa = NSPoint(x: event.location.x, y: mainHeight - event.location.y)
        let isKey = [.keyDown, .keyUp, .flagsChanged].contains(event.type)
        return isKey || app.window.frame.contains(cocoa)
    }

    private func post(_ path: String) async -> (sent: Int, delivered: Int) {
        guard let source = CGEventSource(stateID: .privateState) else { return (0, 0) }
        var sent = 0, delivered = 0
        for (name, event) in sequence(source) {
            if path == "pid" {
                event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(app.window.windowNumber))
                event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(app.window.windowNumber))
            }
            let before = app.events.received.count
            if path == "pid" {
                event.postToPid(getpid())
            } else {
                guard safeForHID(event) else {
                    Log.check(false, "hid: stopped before \"\(name)\": the probe is no longer frontmost with its window key")
                    break
                }
                event.post(tap: .cghidEventTap)
            }
            sent += 1
            Log.line("SENT \(path) \(name) type=\(event.type.rawValue)")
            for _ in 0..<15 where app.events.received.count == before {
                try? await Task.sleep(nanoseconds: 20_000_000)
            }
            let got = app.events.received.count - before
            if got > 0 { delivered += 1 }
            Log.line("\(got > 0 ? "GOT" : "MISSING") \(path) \(name)\(got > 1 ? " (+\(got - 1) more)" : "")")
        }
        return (sent, delivered)
    }

    func run() async {
        Log.line("=== spike e: CGEvent into the probe window === \(Permissions.describe())")
        guard AXIsProcessTrusted() || CGPreflightPostEventAccess() else {
            Log.check(false, "spike e needs Accessibility; run --mode grant first")
            return
        }
        app.window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        app.window.makeFirstResponder(app.events)
        try? await Task.sleep(nanoseconds: 500_000_000)
        let cursor = CGEvent(source: nil)?.location ?? .zero
        for path in ["pid", "hid"] {
            app.say("Spike e: posting events into this window (\(path)). Please don't click elsewhere for a few seconds.")
            let (sent, delivered) = await post(path)
            Log.result("events_\(path)", [("sent", sent), ("delivered", delivered)])
        }
        CGWarpMouseCursorPosition(cursor) // back where Ali left it
        Log.line("cursor restored to (\(cursor.x), \(cursor.y))")
    }
}
