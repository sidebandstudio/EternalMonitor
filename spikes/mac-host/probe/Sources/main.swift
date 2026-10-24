// EternalMonitor Mac Probe: phase 0 spikes c (virtual display), d
// (ScreenCaptureKit into the direct VideoToolbox encoder) and e (CGEvent
// posting into this app's own window). Development only; see ../README.md.
//
//   EternalMonitorMacProbe --mode grant|status|selftest|d|e|c|all [--seconds N]
//                          [--experimental] [--h264] [--confirm-display]
//                          [--persist-arrangement] [--no-tone]
// Spike c is the second-screen gate and the one that matters; d and e on
// the main display are baselines.
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
            default: break // LaunchServices adds its own arguments (-psn_…)
            }
        }
    }
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

    override func draw(_ dirtyRect: NSRect) {
        NSColor.black.setFill()
        bounds.fill()
        let x = CGFloat(count % 240) / 240 * (bounds.width - 40)
        NSColor.systemGreen.setFill()
        NSRect(x: x, y: 0, width: 40, height: bounds.height).fill()
        ("frame \(count)" as NSString).draw(at: NSPoint(x: 8, y: 8), withAttributes: [
            .foregroundColor: NSColor.white, .font: NSFont.monospacedSystemFont(ofSize: 14, weight: .regular),
        ])
    }
}

final class ProbeApp: NSObject, NSApplicationDelegate {
    let options = Options(CommandLine.arguments)
    var window: NSWindow!
    let animation = AnimationView()
    let events = EventView()
    let status = NSTextField(wrappingLabelWithString: "")

    func applicationDidFinishLaunching(_ notification: Notification) {
        let logURL = Log.open(mode: options.mode)
        Log.line("EternalMonitor Mac Probe mode=\(options.mode) pid=\(getpid()) bundle=\(Bundle.main.bundleIdentifier ?? "?") log=\(logURL.path)")
        Log.line("macOS \(ProcessInfo.processInfo.operatingSystemVersionString); \(Permissions.describe())")
        buildWindow()
        Task { @MainActor in
            await run()
        }
    }

    private func buildWindow() {
        window = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 900, height: 620),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
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

if CommandLine.arguments.contains("vd-child") { VirtualDisplayChild.run() }

let app = NSApplication.shared
let delegate = ProbeApp()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
