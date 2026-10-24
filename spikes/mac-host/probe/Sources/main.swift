// EternalMonitor Mac Probe: phase 0 spikes c (virtual display), d
// (ScreenCaptureKit into the direct VideoToolbox encoder) and e (CGEvent
// posting into this app's own window). Development only; see ../README.md.
//
//   EternalMonitorMacProbe --mode grant|status|selftest|d|e|c|all [--seconds N]
//                          [--experimental] [--h264] [--confirm-display]
//                          [--persist-arrangement] [--no-tone]
//                          [--deadline S] [--run-dir DIR]
// Spike c is the second-screen gate and the one that matters; d and e on
// the main display are baselines. --mode displays lists the probe's virtual
// displays for probe.sh and creates nothing.
//
// Every run has a hard self-deadline (--deadline, or a default per mode):
// when it passes, the probe restores the pointer, kills its children by PID
// and exits, which releases any virtual display it owns. SIGTERM does the
// same. --run-dir is where probe.sh finds the PIDs to stop.
import AppKit
import ApplicationServices
import CoreGraphics

struct Options {
    var mode = "grant"
    var seconds = 8.0
    var experimental = false
    var hevc = true
    var confirmDisplay = false
    var tone = true
    var persistArrangement = false
    var deadline: Double?
    var runDir: String?

    init(_ args: [String]) {
        var it = args.dropFirst().makeIterator()
        while let arg = it.next() {
            switch arg {
            case "--mode": mode = it.next() ?? mode
            case "--seconds": seconds = Double(it.next() ?? "") ?? seconds
            case "--experimental": experimental = true
            case "--h264": hevc = false
            case "--confirm-display": confirmDisplay = true
            case "--no-tone": tone = false
            case "--persist-arrangement": persistArrangement = true
            case "--deadline": deadline = Double(it.next() ?? "")
            case "--run-dir": runDir = it.next()
            default: break // LaunchServices adds its own arguments (-psn_…)
            }
        }
        if ProcessInfo.processInfo.environment["ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER"] == "1" { experimental = true }
    }

    /// Seconds before the probe exits by itself. probe.sh uses the same table.
    var hardDeadline: Double {
        if let deadline { return deadline }
        switch mode {
        case "grant": return 1800
        case "status", "displays": return 30
        case "selftest": return 120
        case "d": return 240
        case "e": return 120
        case "c": return 600
        case "all": return 900
        default: return 60
        }
    }
}

/// What a deadline or a signal must undo before the process exits: the
/// pointer goes back and the probe's children are killed by PID. Runs on
/// background queues, whatever the main thread is doing.
enum Cleanup {
    private static let lock = NSLock()
    private static var cursor: CGPoint?
    private static var signalSources: [DispatchSourceSignal] = []

    static func saveCursor(_ point: CGPoint?) {
        lock.lock()
        cursor = point
        lock.unlock()
    }

    static func emergency(_ why: String) {
        Log.line("ABORT \(why)")
        lock.lock()
        let point = cursor
        lock.unlock()
        if let point { CGWarpMouseCursorPosition(point) }
        ChildRegistry.killAll()
    }

    static func start(_ options: Options) {
        if let dir = options.runDir {
            try? "\(getpid())\n".write(toFile: dir + "/probe.pid", atomically: true, encoding: .utf8)
            ChildRegistry.file = dir + "/children"
        }
        let seconds = options.hardDeadline
        Watchdog.arm(seconds: seconds) {
            emergency("hard deadline of \(String(format: "%g", seconds)) s reached; exiting, which releases any virtual display this process owns")
        }
        for number in [SIGTERM, SIGINT, SIGHUP] {
            signal(number, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: number, queue: .global(qos: .userInteractive))
            source.setEventHandler {
                emergency("signal \(number); exiting, which releases any virtual display this process owns")
                _exit(128 + number)
            }
            source.resume()
            signalSources.append(source)
        }
    }
}

/// Borderless windows can't become key unless they say so.
final class ProbeWindow: NSWindow {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

enum Permissions {
    static func describe() -> String {
        "screen_recording=\(CGPreflightScreenCaptureAccess()) accessibility=\(AXIsProcessTrusted()) post_events=\(CGPreflightPostEventAccess())"
    }
    static var allGranted: Bool {
        CGPreflightScreenCaptureAccess() && AXIsProcessTrusted() && CGPreflightPostEventAccess()
    }
}

/// Moving content, redrawn at the screen's refresh rate, so capture has new
/// frames to deliver; it stops for the idle measurement.
final class AnimationView: NSView {
    var animating = true
    private var count = 0
    private var link: CADisplayLink?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        link?.invalidate()
        link = displayLink(target: self, selector: #selector(tick))
        link?.add(to: .main, forMode: .common)
    }

    @objc private func tick() {
        guard animating else { return }
        count += 1
        needsDisplay = true
    }

    /// Where the content marker is drawn: the top-left corner of this view.
    var markerRect: NSRect {
        NSRect(x: 0, y: bounds.height - Marker.blockPt, width: Double(Marker.count) * Marker.blockPt, height: Marker.blockPt)
    }

    override func draw(_ dirtyRect: NSRect) {
        NSColor.black.setFill()
        bounds.fill()
        let x = CGFloat(count % 240) / 240 * (bounds.width - 40)
        NSColor.systemGreen.setFill()
        NSRect(x: x, y: 0, width: 40, height: bounds.height).fill()
        ("frame \(count)" as NSString).draw(at: NSPoint(x: 8, y: 8), withAttributes: [
            .foregroundColor: NSColor.white, .font: NSFont.monospacedSystemFont(ofSize: 14, weight: .regular),
        ])
        // The display this window is on and the frame count, for capture to
        // read back from captured and decoded frames.
        let display = (window?.screen?.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value ?? 0
        let rect = markerRect
        for (index, on) in Marker.bits(display: display, counter: UInt32(truncatingIfNeeded: count)).enumerated() {
            (on ? NSColor.white : NSColor.black).setFill()
            NSRect(x: rect.minX + Double(index) * Marker.blockPt, y: rect.minY, width: Marker.blockPt, height: Marker.blockPt).fill()
        }
    }
}

final class ProbeApp: NSObject, NSApplicationDelegate {
    let options = probeOptions
    var window: NSWindow!
    let animation = AnimationView()
    let events = EventView()
    let status = NSTextField(wrappingLabelWithString: "")

    func applicationDidFinishLaunching(_ notification: Notification) {
        let logURL = Log.open(mode: options.mode)
        Log.line("EternalMonitor Mac Probe mode=\(options.mode) pid=\(getpid()) bundle=\(Bundle.main.bundleIdentifier ?? "?") log=\(logURL.path)")
        Log.line("macOS \(ProcessInfo.processInfo.operatingSystemVersionString); \(Permissions.describe())")
        Log.line("hard deadline \(String(format: "%g", options.hardDeadline)) s; experimental HEVC High tier \(options.experimental ? "on (informational)" : "off")")
        buildWindow()
        Task { @MainActor in
            await run()
        }
    }

    private func buildWindow() {
        window = ProbeWindow(contentRect: NSRect(x: 200, y: 200, width: 900, height: 620),
                             styleMask: Self.titled, backing: .buffered, defer: false)
        window.acceptsMouseMovedEvents = true
        window.title = "EternalMonitor Mac Probe — \(options.mode)"
        let root = NSView(frame: window.contentRect(forFrameRect: window.frame))
        status.frame = NSRect(x: 16, y: 520, width: 868, height: 90)
        status.autoresizingMask = [.width, .minYMargin]
        animation.autoresizingMask = [.width, .minYMargin]
        events.autoresizingMask = [.width, .height]
        status.font = .systemFont(ofSize: 15)
        animation.frame = NSRect(x: 16, y: 440, width: 868, height: 70)
        events.frame = NSRect(x: 16, y: 16, width: 868, height: 410)
        [status, animation, events].forEach(root.addSubview)
        window.contentView = root
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(events)
        NSApp.activate()
    }

    private static let titled: NSWindow.StyleMask = [.titled, .closable, .miniaturizable, .resizable]

    /// Covers `screen` (a virtual display) with the probe window at shielding
    /// level, so nothing else can be under the pointer there.
    func cover(_ screen: NSScreen) {
        window.styleMask = [.borderless]
        window.setFrame(screen.frame, display: true)
        window.level = NSWindow.Level(rawValue: Int(CGShieldingWindowLevel()))
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(events)
    }

    /// Back to a normal titled window on the main display. The level drops
    /// first, so a shielding window never sits on Ali's screen.
    func uncover() {
        window.level = .normal
        window.styleMask = Self.titled
        if let main = NSScreen.screens.first?.visibleFrame {
            window.setFrame(NSRect(x: main.minX + 200, y: main.minY + 200, width: 900, height: 620), display: true)
        }
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(events)
    }

    /// The window covers all of display `id` at shielding level.
    func covers(_ id: CGDirectDisplayID) -> Bool {
        guard let screen = Displays.screen(id) else { return false }
        return window.isVisible && window.level.rawValue >= Int(CGShieldingWindowLevel()) && window.frame == screen.frame
    }

    func say(_ text: String) {
        status.stringValue = text
        Log.line("STATUS \(text)")
    }

    @MainActor private func run() async {
        switch options.mode {
        case "status":
            Log.line("PERMISSIONS \(Permissions.describe())")
            finish()
        case "grant":
            await grant()
        case "selftest":
            SelfTest().run(options: options)
            finish()
        case "d":
            await CaptureSpike(app: self).run()
            finish()
        case "e":
            await EventSpike(app: self).run()
            finish()
        case "c":
            await runDisplaySpike()
            finish()
        case "all":
            await runDisplaySpike()
            await CaptureSpike(app: self).run()
            await EventSpike(app: self).run()
            finish()
        default:
            Log.line("unknown mode \(options.mode)")
            finish()
        }
    }

    @MainActor private func runDisplaySpike() async {
        guard options.confirmDisplay else {
            Log.check(false, "spike c needs --confirm-display: it adds a display to the Mac Ali is viewing")
            return
        }
        await VirtualDisplaySpike(app: self).run()
    }

    /// First run: asks for Screen Recording and Accessibility, so the app is
    /// listed in Privacy & Security, then waits until both are on.
    @MainActor private func grant() async {
        say("Asking for Screen Recording…")
        if !CGPreflightScreenCaptureAccess() {
            Log.line("CGRequestScreenCaptureAccess -> \(CGRequestScreenCaptureAccess())")
        }
        say("Asking for Accessibility…")
        if !AXIsProcessTrusted() {
            let prompt = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
            Log.line("AXIsProcessTrustedWithOptions(prompt) -> \(AXIsProcessTrustedWithOptions([prompt: true] as CFDictionary))")
        }
        if !CGPreflightPostEventAccess() {
            Log.line("CGRequestPostEventAccess -> \(CGRequestPostEventAccess())")
        }
        var last = ""
        while true {
            let now = Permissions.describe()
            if now != last {
                Log.line("PERMISSIONS \(now)")
                last = now
            }
            if Permissions.allGranted {
                say("All permissions are on. If Screen Recording was just switched on, macOS may ask to quit and reopen the probe: click Quit & Reopen (or just quit it). You can close this window.")
            } else {
                say("Waiting for permissions — \(now.replacingOccurrences(of: "_", with: " "))\nTurn on EternalMonitor Mac Probe in System Settings › Privacy & Security › Screen & System Audio Recording, and › Accessibility.")
            }
            try? await Task.sleep(nanoseconds: 1_000_000_000)
        }
    }

    func finish() {
        Log.line("DONE mode=\(options.mode) failures=\(Log.failures)")
        NSApp.terminate(nil)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

let probeOptions = Options(CommandLine.arguments)
switch probeOptions.mode {
case "vd-child": VirtualDisplayChild.run()
case "displays": Displays.list()
default: Cleanup.start(probeOptions)
}

let app = NSApplication.shared
let delegate = ProbeApp()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
