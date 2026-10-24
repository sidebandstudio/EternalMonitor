// Spike e: CGEvents posted at this app's own window, and what arrives. In
// the second-screen gate the window covers the whole virtual display at
// shielding level, and every position comes from a wire coordinate
// (0...65535) mapped onto that display's bounds, as the host will map touches.
//
// Two paths. CGEventPostToPid, which only this process can receive. Then the
// system (HID) path the host will use, guarded by the probe's own event tap
// (ProbeTap), the last check before macOS delivers an event:
//   - keys are recorded by the tap and swallowed there, so no app gets them;
//   - mouse and pen events pass only inside the virtual display the window
//     covers, with the probe frontmost and not annotated for another
//     process; anything else is swallowed and is a failure;
//   - no ⌘, ⌃, ⌥ or fn, and no modifier key events, on this path;
//   - without the tap nothing is posted on this path, and if macOS turns
//     the tap off, posting stops;
//   - real input (someone moving the mouse) stops the run at once.
// Every event carries a tag and sequence number, and what arrives is matched
// against what was posted: type, location within 1 pt, and order.
import AppKit
import Carbon.HIToolbox
import CoreGraphics

/// Logs every event AppKit delivers to it.
final class EventView: NSView {
    /// What arrived, for matching against what was posted.
    struct Record {
        let userData: Int64
        let type: String
        let point: CGPoint?
        let keyCode: Int?
        let chars: String?
    }

    private(set) var received: [String] = []
    private(set) var records: [Record] = []
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

    func seen(run: UInt64) -> [SeenEvent] {
        records.map { SeenEvent(seq: ProbeTag.seq($0.userData, run: run), type: $0.type, point: $0.point, keyCode: $0.keyCode, chars: $0.chars) }
    }

    func has(run: UInt64, seq: Int) -> Bool { records.contains { ProbeTag.seq($0.userData, run: run) == seq } }

    private func record(_ event: NSEvent, _ detail: String = "") {
        let p = convert(event.locationInWindow, from: nil)
        let userData = event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0
        var line = "\(event.type.rawValue):\(eventName(event.type.rawValue)) tag=0x\(String(UInt64(bitPattern: userData), radix: 16)) at=(\(String(format: "%.1f,%.1f", p.x, p.y)))"
        let isKey = [.keyDown, .keyUp, .flagsChanged].contains(event.type)
        var global: CGPoint?
        if let window, !isKey {
            let screen = window.convertPoint(toScreen: event.locationInWindow)
            global = CGPoint(x: screen.x, y: (NSScreen.screens.first?.frame.height ?? 0) - screen.y)
            line += String(format: " global=(%.2f,%.2f)", global!.x, global!.y)
        }
        switch event.type {
        case .leftMouseDown, .leftMouseUp, .leftMouseDragged, .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp, .mouseMoved:
            line += " button=\(event.buttonNumber) clicks=\(event.clickCount) pressure=\(String(format: "%.6f", event.pressure)) subtype=\(event.subtype.rawValue)"
            if event.subtype == .tabletPoint {
                line += " tilt=(\(String(format: "%.4f,%.4f", event.tilt.x, event.tilt.y))) rotation=\(String(format: "%.2f", event.rotation)) device=\(event.deviceID)"
            }
        default: break
        }
        if !detail.isEmpty { line += " " + detail }
        records.append(Record(userData: userData, type: eventName(event.type.rawValue), point: global,
                              keyCode: isKey ? Int(event.keyCode) : nil,
                              chars: [.keyDown, .keyUp].contains(event.type) ? event.characters : nil))
        received.append(line)
        Log.line("RECV \(line)")
        needsDisplay = true
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

/// The probe's own event tap: at the annotated session tap, appended last,
/// so it sees each event just before macOS delivers it, after the target
/// process is known. It runs on a thread of its own, so it always answers in
/// time. TapPolicy decides; this records what it saw.
final class ProbeTap: @unchecked Sendable {
    struct Seen {
        let userData: Int64
        let type: CGEventType
        let location: CGPoint
        let keyCode: Int
        let chars: String
        let targetPID: Int32
        let decision: TapDecision
    }

    private let lock = NSLock()
    private var state = TapState(ownPID: getpid())
    private var seenEvents: [Seen] = []
    private var firstInput: String?
    private var disabledBy: String?
    private var port: CFMachPort?
    private var runLoop: CFRunLoop?

    /// Installs the tap, or returns nil if macOS refuses it (then nothing is
    /// posted on the system path).
    static func install() -> ProbeTap? {
        let tap = ProbeTap()
        let callback: CGEventTapCallBack = { _, type, event, info in
            Unmanaged<ProbeTap>.fromOpaque(info!).takeUnretainedValue().handle(type, event)
        }
        guard let port = CGEvent.tapCreate(tap: .cgAnnotatedSessionEventTap, place: .tailAppendEventTap, options: .defaultTap,
                                           eventsOfInterest: ~CGEventMask(0), callback: callback,
                                           userInfo: Unmanaged.passUnretained(tap).toOpaque()) else { return nil }
        tap.port = port
        let ready = DispatchSemaphore(value: 0)
        let thread = Thread {
            let source = CFMachPortCreateRunLoopSource(nil, port, 0)
            tap.lock.lock()
            tap.runLoop = CFRunLoopGetCurrent()
            tap.lock.unlock()
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .commonModes)
            CGEvent.tapEnable(tap: port, enable: true)
            ready.signal()
            CFRunLoopRun()
        }
        thread.name = "probe.tap"
        thread.qualityOfService = .userInteractive
        thread.start()
        guard ready.wait(timeout: .now() + 2) == .success, CGEvent.tapIsEnabled(tap: port) else {
            tap.remove()
            return nil
        }
        return tap
    }

    func remove() {
        lock.lock()
        let loop = runLoop
        lock.unlock()
        if let port {
            CGEvent.tapEnable(tap: port, enable: false)
            CFMachPortInvalidate(port)
        }
        if let loop { CFRunLoopStop(loop) }
    }

    func update(_ change: (inout TapState) -> Void) {
        lock.lock()
        change(&state)
        lock.unlock()
    }

    /// Why posting must stop now: the tap is off, or someone used the Mac.
    var problem: String? {
        lock.lock()
        let input = firstInput
        lock.unlock()
        return offReason ?? input.map { "real input: \($0)" }
    }

    /// Set once the tap can no longer stop anything; then nothing is posted.
    var offReason: String? {
        lock.lock()
        let disabled = disabledBy
        lock.unlock()
        if let disabled { return "macOS turned the tap off (\(disabled))" }
        guard let port, CGEvent.tapIsEnabled(tap: port) else { return "the tap is not enabled" }
        return nil
    }

    func seen(run: UInt64) -> [(seq: Int, event: Seen)] {
        lock.lock()
        defer { lock.unlock() }
        return seenEvents.compactMap { event in ProbeTag.seq(event.userData, run: run).map { ($0, event) } }
    }

    private func handle(_ type: CGEventType, _ event: CGEvent) -> Unmanaged<CGEvent>? {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            lock.lock()
            disabledBy = type == .tapDisabledByTimeout ? "timeout" : "user input"
            lock.unlock()
            return Unmanaged.passUnretained(event)
        }
        let userData = event.getIntegerValueField(.eventSourceUserData)
        let target = Int32(truncatingIfNeeded: event.getIntegerValueField(.eventTargetUnixProcessID))
        lock.lock()
        defer { lock.unlock() }
        let decision = TapPolicy.decide(type: type, userData: userData, location: event.location, flags: event.flags,
                                        targetPID: target, state: state)
        if decision == .passThrough {
            if TapPolicy.isInput(type), firstInput == nil {
                firstInput = String(format: "%@ at (%.0f,%.0f)", eventName(UInt(type.rawValue)), event.location.x, event.location.y)
            }
        } else {
            var chars = ""
            if TapPolicy.keyTypes.contains(type) {
                var length = 0
                var units = [UniChar](repeating: 0, count: 8)
                event.keyboardGetUnicodeString(maxStringLength: units.count, actualStringLength: &length, unicodeString: &units)
                chars = String(utf16CodeUnits: units, count: length)
            }
            seenEvents.append(Seen(userData: userData, type: type, location: event.location,
                                   keyCode: Int(event.getIntegerValueField(.keyboardEventKeycode)), chars: chars,
                                   targetPID: target, decision: decision))
        }
        return decision.swallowed ? nil : Unmanaged.passUnretained(event)
    }
}

@MainActor
struct EventSpike {
    let app: ProbeApp
    /// The virtual display input is mapped onto, or nil for the main-display baseline.
    let display: CGDirectDisplayID?
    private let deviceID: Int64 = 0x4d01
    // NX_TABLET_CAPABILITY_{DEVICEID,ABSX,ABSY,BUTTONS,TILTX,TILTY,PRESSURE,ROTATION}MASK
    private let capabilities: Int64 = 0x0001 | 0x0002 | 0x0004 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x2000

    init(app: ProbeApp, display: CGDirectDisplayID? = nil) {
        self.app = app
        self.display = display
    }

    private var where_: String { display == nil ? "main" : "virtual" }

    /// spike b's `norm_to_display_point`: each wire value maps to the centre
    /// of its 1/65536 slice, so 65535 stays inside the display.
    static func wireToPoint(_ x: UInt16, _ y: UInt16, _ bounds: CGRect) -> CGPoint {
        CGPoint(x: bounds.minX + (Double(x) + 0.5) / 65536 * bounds.width,
                y: bounds.minY + (Double(y) + 0.5) / 65536 * bounds.height)
    }

    static func wire(_ f: CGFloat) -> UInt16 { UInt16((f * 65535).rounded()) }

    /// A point for the sequence, in CG global coordinates (top-left origin):
    /// a wire coordinate on the virtual display, or a spot in the event view.
    private func globalPoint(_ fx: CGFloat, _ fy: CGFloat) -> CGPoint {
        if let display {
            return Self.wireToPoint(Self.wire(fx), Self.wire(fy), CGDisplayBounds(display))
        }
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

    private func proximity(_ enter: Bool, at: CGPoint, _ source: CGEventSource) -> CGEvent {
        let event = CGEvent(source: source)!
        event.type = .tabletProximity
        event.location = at
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

    /// One event of the sequence.
    private struct Step {
        let name: String
        let event: CGEvent
        /// Also sent on the system path. Modifier keys only go to this process.
        var systemPath = true
        /// The characters a keyDown must deliver (only for Unicode-string
        /// events; keycode events depend on the keyboard layout).
        var chars: String?
        /// Whether its location is checked (pointer events).
        var located = true

        var isKey: Bool { [.keyDown, .keyUp, .flagsChanged].contains(event.type) }

        func expected(_ seq: Int) -> PostedEvent {
            PostedEvent(seq: seq, name: name, type: eventName(UInt(event.type.rawValue)),
                        point: located && !isKey ? event.location : nil,
                        keyCode: isKey ? Int(event.getIntegerValueField(.keyboardEventKeycode)) : nil,
                        chars: event.type == .keyDown ? chars : nil)
        }
    }

    private func sequence(_ source: CGEventSource) -> [Step] {
        // Inside the event view either way: on the virtual display the window
        // covers the screen and the view fills it below the status strip.
        let a = globalPoint(0.3, 0.55), b = globalPoint(0.5, 0.6), c = globalPoint(0.7, 0.45)
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
            Step(name: "move", event: mouse(.mouseMoved, a, .left, source)),
            Step(name: "left down", event: mouse(.leftMouseDown, a, .left, source)),
            Step(name: "left drag", event: mouse(.leftMouseDragged, b, .left, source)),
            Step(name: "left up", event: mouse(.leftMouseUp, b, .left, source)),
            Step(name: "right down", event: mouse(.rightMouseDown, b, .right, source)),
            Step(name: "right up", event: mouse(.rightMouseUp, b, .right, source)),
            Step(name: "scroll pixels dy=-40 dx=10", event: scrollPixels),
            Step(name: "scroll lines dy=3 dx=-1", event: scrollLines),
            Step(name: "key a down", event: key(CGKeyCode(kVK_ANSI_A), true, [], source)),
            Step(name: "key a up", event: key(CGKeyCode(kVK_ANSI_A), false, [], source)),
            Step(name: "shift down", event: key(CGKeyCode(kVK_Shift), true, shift, source), systemPath: false),
            Step(name: "key A (shift) down", event: key(CGKeyCode(kVK_ANSI_A), true, shift, source)),
            Step(name: "key A (shift) up", event: key(CGKeyCode(kVK_ANSI_A), false, shift, source)),
            Step(name: "shift up", event: key(CGKeyCode(kVK_Shift), false, [], source), systemPath: false),
            Step(name: "unicode é", event: unicode("é", true, source), chars: "é"),
            Step(name: "unicode é up", event: unicode("é", false, source)),
            Step(name: "unicode 😀 (one event)", event: unicode("😀", true, source), chars: "😀"),
            Step(name: "unicode 😀 up", event: unicode("😀", false, source)),
            Step(name: "pen proximity enter", event: proximity(true, at: a, source), located: false),
            Step(name: "pen down p=0.25 tilt=(0.4,-0.6) rot=47.5", event: pen(.leftMouseDown, a, pressure: 0.25, tilt: (0.4, -0.6), rotation: 47.5, source)),
            Step(name: "pen drag p=0.5", event: pen(.leftMouseDragged, b, pressure: 0.5, tilt: (0.2, -0.3), rotation: 50, source)),
            Step(name: "pen drag p=0.733989", event: pen(.leftMouseDragged, c, pressure: 0.733989, tilt: (0.416639, -0.666646), rotation: 47.5, source)),
            Step(name: "pen up", event: pen(.leftMouseUp, c, pressure: 0, tilt: (0, 0), rotation: 0, source)),
            Step(name: "pure tablet point p=0.6 tilt=(-0.25,0.5)", event: pure),
            Step(name: "pen proximity exit", event: proximity(false, at: c, source), located: false),
        ]
    }

    private func waitFor(_ seconds: Double, _ condition: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if Date() >= deadline { return false }
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        return true
    }

    /// This app active, its window key, and macOS agreeing it is frontmost.
    private func frontmost() -> Bool {
        NSApp.isActive && app.window.isKeyWindow && NSWorkspace.shared.frontmostApplication?.processIdentifier == getpid()
    }

    private func report(_ label: String, _ match: MatchReport) {
        Log.result("events_\(label.replacingOccurrences(of: " ", with: "_"))", [
            ("expected", match.expected), ("matched", match.matched), ("missing", match.missing.count),
            ("wrong", match.mismatched.count), ("out_of_order", match.outOfOrder.count),
            ("max_pointer_error_pt", String(format: "%.2f", match.maxErrorPt)), ("untagged", match.untagged),
        ])
        Log.check(match.passed, "\(label): every posted event arrived, in order, of the right type, within 1 pt — \(match.summary)")
    }

    /// CGEventPostToPid: only this process can receive these.
    private func postToPid(_ steps: [Step], tap: ProbeTap?) async {
        let run = ProbeTag.newRun()
        tap?.update { $0.pidRun = run }
        for (seq, step) in steps.enumerated() {
            if let problem = tap?.problem {
                Log.check(false, "\(where_) pid: stopped before #\(seq) \"\(step.name)\": \(problem)")
                break
            }
            step.event.setIntegerValueField(.eventSourceUserData, value: ProbeTag.make(run: run, seq: seq))
            step.event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(app.window.windowNumber))
            step.event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(app.window.windowNumber))
            step.event.postToPid(getpid())
            Log.line("SENT pid #\(seq) \(step.name) type=\(step.event.type.rawValue)")
            let got = await waitFor(0.3) { app.events.has(run: run, seq: seq) }
            Log.line("\(got ? "GOT" : "MISSING") pid #\(seq) \(step.name)")
        }
        // Against the whole sequence: anything not sent is missing.
        report("\(where_) pid", SequenceMatcher.match(steps.enumerated().map { $1.expected($0) }, app.events.seen(run: run)))
    }

    /// The system path, only through the tap.
    private func postToSystem(_ all: [Step], tap: ProbeTap?, source: CGEventSource) async {
        let label = "\(where_) hid"
        guard let tap else {
            Log.check(false, "\(label): skipped; the probe's event tap could not be installed, and nothing is posted on the system path without it")
            return
        }
        var bounds: CGRect?
        if let display {
            let covered = app.covers(display)
            Log.check(covered, "\(label): the probe window covers virtual display \(display) at shielding level")
            if covered { bounds = CGDisplayBounds(display) }
        } else {
            Log.line("INFO \(label): mouse and pen events go on the system path only on the virtual display (spike c); this baseline sends keys only")
        }
        let steps = all.filter { $0.systemPath && (bounds != nil || $0.isKey) }
        let run = ProbeTag.newRun()
        tap.update {
            $0.systemRun = run
            $0.display = bounds
        }
        let observers = [
            NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main) { note in
                let pid = (note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication)?.processIdentifier
                tap.update { $0.frontmost = pid == getpid() }
            },
            NotificationCenter.default.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { _ in
                tap.update { $0.frontmost = false }
            },
        ]
        defer {
            NSWorkspace.shared.notificationCenter.removeObserver(observers[0])
            NotificationCenter.default.removeObserver(observers[1])
            tap.update {
                $0.systemRun = nil
                $0.frontmost = false
            }
        }

        var stopped: String?
        var held: [String: CGEvent] = [:] // the release for each button or key still down
        var seq = 0
        for step in steps {
            if let problem = tap.problem { stopped = "before #\(seq) \"\(step.name)\": \(problem)"; break }
            let front = frontmost()
            tap.update { $0.frontmost = front }
            guard front else { stopped = "before #\(seq) \"\(step.name)\": the probe is no longer frontmost"; break }
            let event = step.event
            let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
            switch event.type {
            case .leftMouseDown: held["left"] = mouse(.leftMouseUp, event.location, .left, source)
            case .rightMouseDown: held["right"] = mouse(.rightMouseUp, event.location, .right, source)
            case .keyDown: held["key \(keyCode)"] = key(CGKeyCode(keyCode), false, [], source)
            case .leftMouseUp: held["left"] = nil
            case .rightMouseUp: held["right"] = nil
            case .keyUp: held["key \(keyCode)"] = nil
            default: break
            }
            post(event, step.name, run: run, seq: seq)
            seq += 1
            guard let seen = await waitForTap(tap, run: run, seq: seq - 1) else {
                stopped = "the tap never saw #\(seq - 1) \"\(step.name)\""
                break
            }
            Log.line("TAP hid #\(seq - 1) \(step.name): \(seen.decision) target_pid=\(seen.targetPID)")
            if case .swallow(let why) = seen.decision { stopped = "#\(seq - 1) \"\(step.name)\" was swallowed: \(why)"; break }
            if seen.decision == .deliver {
                let got = await waitFor(0.3) { app.events.has(run: run, seq: seq - 1) }
                Log.line("\(got ? "GOT" : "MISSING") hid #\(seq - 1) \(step.name)")
            }
        }
        if let stopped {
            Log.check(false, "\(label): stopped \(stopped)")
            // Release whatever is still down, through the same tap and checks,
            // unless the tap is off (then nothing more is posted).
            for (what, release) in held where tap.offReason == nil {
                post(release, "release \(what)", run: run, seq: seq)
                let seen = await waitForTap(tap, run: run, seq: seq)
                Log.line("TAP hid #\(seq) release \(what): \(seen.map { "\($0.decision)" } ?? "not seen")")
                seq += 1
            }
        }

        let expected = steps.enumerated().map { $1.expected($0) }
        let seen = tap.seen(run: run)
        let unsafe = seen.filter { if case .swallow = $0.event.decision { true } else { false } }
        for item in unsafe { Log.line("UNSAFE hid #\(item.seq): \(item.event.decision)") }
        Log.check(unsafe.isEmpty, "\(label): no posted event was swallowed as unsafe (\(unsafe.count))")
        Log.check(seen.allSatisfy { $0.event.decision != .deliver || !TapPolicy.keyTypes.contains($0.event.type) },
                  "\(label): no key was let through the tap")
        // Keys stop at the tap; pointer events must reach the window.
        let keysAtTap = seen.filter { $0.event.decision == .swallowKey }.map {
            SeenEvent(seq: $0.seq, type: eventName(UInt($0.event.type.rawValue)), point: nil, keyCode: $0.event.keyCode, chars: $0.event.chars)
        }
        report("\(where_) hid keys (recorded and swallowed by the tap)", SequenceMatcher.match(expected.filter { $0.keyCode != nil }, keysAtTap))
        if bounds != nil {
            report("\(where_) hid pointer", SequenceMatcher.match(expected.filter { $0.keyCode == nil }, app.events.seen(run: run)))
        }
    }

    /// Tags and posts one event on the system path. Callers check the tap first.
    private func post(_ event: CGEvent, _ name: String, run: UInt64, seq: Int) {
        event.setIntegerValueField(.eventSourceUserData, value: ProbeTag.make(run: run, seq: seq))
        event.post(tap: .cghidEventTap)
        Log.line("SENT hid #\(seq) \(name) type=\(event.type.rawValue)")
    }

    private func waitForTap(_ tap: ProbeTap, run: UInt64, seq: Int) async -> ProbeTap.Seen? {
        var found: ProbeTap.Seen?
        _ = await waitFor(0.5) {
            found = tap.seen(run: run).first { $0.seq == seq }?.event
            return found != nil
        }
        return found
    }

    /// The edges of the wire range land on the virtual display and never on
    /// its neighbour. The cursor is warped (no event is posted) and read back.
    private func checkEdges(_ display: CGDirectDisplayID) {
        let bounds = CGDisplayBounds(display)
        let scale = Double(CGDisplayCopyDisplayMode(display)?.pixelWidth ?? 0) / bounds.width
        for (x, y) in [(0, 0), (65535, 0), (0, 65535), (65535, 65535), (32768, 32768), (65535, 32768)] as [(UInt16, UInt16)] {
            let point = Self.wireToPoint(x, y, bounds)
            CGWarpMouseCursorPosition(point)
            let readBack = CGEvent(source: nil)?.location ?? .zero
            var ids = [CGDirectDisplayID](repeating: 0, count: 4)
            var count: UInt32 = 0
            CGGetDisplaysWithPoint(readBack, 4, &ids, &count)
            let pixel = (Int((point.x - bounds.minX) * scale), Int((point.y - bounds.minY) * scale))
            Log.check(count == 1 && ids[0] == display,
                      String(format: "wire (%u,%u) -> (%.3f,%.3f) pt, pixel (%d,%d); cursor at (%.2f,%.2f) on display %@",
                             x, y, point.x, point.y, pixel.0, pixel.1, readBack.x, readBack.y,
                             ids.prefix(Int(count)).map { "\($0)" }.joined(separator: ",")))
        }
    }

    func run() async {
        Log.line("=== spike e: CGEvent into the probe window on the \(where_) display === \(Permissions.describe())")
        guard AXIsProcessTrusted() || CGPreflightPostEventAccess() else {
            Log.check(false, "spike e needs Accessibility; run --mode grant first")
            return
        }
        guard let source = CGEventSource(stateID: .privateState) else {
            Log.check(false, "spike e: no private event source")
            return
        }
        app.window.makeKeyAndOrderFront(nil)
        NSApp.activate()
        app.window.makeFirstResponder(app.events)
        try? await Task.sleep(nanoseconds: 500_000_000)
        let cursor = CGEvent(source: nil)?.location ?? .zero
        Cleanup.saveCursor(cursor)
        defer {
            CGWarpMouseCursorPosition(cursor) // back where Ali left it, however the run ends
            Cleanup.saveCursor(nil)
            Log.line("cursor restored to (\(cursor.x), \(cursor.y))")
        }
        if let display {
            checkEdges(display)
            try? await Task.sleep(nanoseconds: 300_000_000) // let the warps settle before the tap watches for input
        }
        let tap = ProbeTap.install()
        Log.check(tap != nil, "the probe's event tap is installed (annotated session, appended last)")
        defer { tap?.remove() }
        app.say("Spike e: posting events into this window (pid). Please don't touch the mouse or keyboard.")
        await postToPid(sequence(source), tap: tap)
        app.say("Spike e: posting events through the system path, guarded by the probe's tap. Please don't touch the mouse or keyboard.")
        await postToSystem(sequence(source), tap: tap, source: source)
    }
}
