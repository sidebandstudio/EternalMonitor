// Spike c, the second-screen gate. A CGVirtualDisplay at the Duo's inner
// size, made the way Chromium's virtual_display_mac_util does: HiDPI on,
// modes at half the pixel size, maxPixels at the full size, and a vendor ID
// and serial of its own. On that display the gate checks:
//   1. the active mode is HiDPI with the right logical size and backing
//      pixels, at 60 and 120 Hz;
//   2. placement right of the main display, and whether it survives the
//      display being recreated;
//   3. capture (spike d) and input mapped onto its bounds (spike e);
//   4. removal within 2 s after mode changes (macOS 26 reportedly stops
//      removing a display whose mode was changed), recreation with another
//      mode, and a third creation after that;
//   5. that the display goes away when its owning process is killed or
//      exits without releasing it.
// Mode and placement changes are kCGConfigureForSession unless
// --persist-arrangement asks for a permanent placement (which writes
// devbox1's saved arrangement for this virtual display).
import AppKit
import CoreGraphics

struct VirtualDisplaySpec {
    let name: String
    let pixels: (UInt32, UInt32)
    let rates: [Double]
    var serial: UInt32 = VirtualDisplaySpike.serial
    var points: (UInt32, UInt32) { (pixels.0 / 2, pixels.1 / 2) }

    static let inner = VirtualDisplaySpec(name: "duo-inner", pixels: (2670, 1878), rates: [60, 120])
    static let outer = VirtualDisplaySpec(name: "duo-outer", pixels: (2034, 1398), rates: [120, 60])

    /// Created the way the host will create it.
    func make(queue: DispatchQueue) -> CGVirtualDisplay? {
        let descriptor = CGVirtualDisplayDescriptor()
        descriptor.queue = queue
        descriptor.name = name
        descriptor.maxPixelsWide = pixels.0
        descriptor.maxPixelsHigh = pixels.1
        // About 413 ppi, a phone panel.
        descriptor.sizeInMillimeters = CGSize(width: Double(pixels.0) / 413 * 25.4, height: Double(pixels.1) / 413 * 25.4)
        descriptor.vendorID = VirtualDisplaySpike.vendorID
        descriptor.productID = VirtualDisplaySpike.productID
        descriptor.serialNum = serial
        descriptor.terminationHandler = { _, display in
            Log.line("virtual display terminationHandler id=\(display.displayID)")
        }
        guard let display = CGVirtualDisplay(descriptor: descriptor) else { return nil }
        let settings = CGVirtualDisplaySettings()
        settings.hiDPI = 1
        settings.modes = rates.map { CGVirtualDisplayMode(width: points.0, height: points.1, refreshRate: $0) }
        return display.apply(settings) ? display : nil
    }
}

enum Displays {
    static func online() -> [CGDirectDisplayID] {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var count: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &count)
        return Array(ids.prefix(Int(count)))
    }

    static func screen(_ id: CGDirectDisplayID) -> NSScreen? {
        NSScreen.screens.first { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id }
    }

    static func describe(_ id: CGDirectDisplayID) -> String {
        let bounds = CGDisplayBounds(id)
        let mode = CGDisplayCopyDisplayMode(id)
        let mm = CGDisplayScreenSize(id)
        return String(format: "id=%u bounds=(%.0f,%.0f %.0fx%.0f) mode=%dx%dpt %dx%dpx %.2fHz scale=%@ mm=%.0fx%.0f vendor=0x%x model=0x%x serial=0x%x main=%d mirror=%u",
                      id, bounds.origin.x, bounds.origin.y, bounds.width, bounds.height,
                      mode?.width ?? 0, mode?.height ?? 0, mode?.pixelWidth ?? 0, mode?.pixelHeight ?? 0, mode?.refreshRate ?? 0,
                      screen(id).map { String(format: "%.1f", $0.backingScaleFactor) } ?? "none", mm.width, mm.height,
                      CGDisplayVendorNumber(id), CGDisplayModelNumber(id), CGDisplaySerialNumber(id),
                      CGDisplayIsMain(id) != 0 ? 1 : 0, CGDisplayMirrorsDisplay(id))
    }

    static func modes(_ id: CGDirectDisplayID) -> [CGDisplayMode] {
        let options = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
        return (CGDisplayCopyAllDisplayModes(id, options) as? [CGDisplayMode]) ?? []
    }
}

/// `--mode vd-child [--exit]`: creates a display, prints `READY <id>`, then
/// either waits to be killed or exits without releasing it.
enum VirtualDisplayChild {
    static func run() -> Never {
        var spec = VirtualDisplaySpec.inner
        spec.serial = VirtualDisplaySpike.serial + 1 // never collides with the parent's
        guard let display = spec.make(queue: .main) else {
            print("FAILED")
            exit(1)
        }
        let id = display.displayID
        let deadline = Date().addingTimeInterval(3)
        while !Displays.online().contains(id), Date() < deadline { usleep(10_000) }
        print("READY \(id)")
        fflush(stdout)
        if CommandLine.arguments.contains("--exit") {
            usleep(300_000)
            exit(0) // no release: does the display outlive the process?
        }
        withExtendedLifetime(display) { dispatchMain() }
    }
}

@MainActor
struct VirtualDisplaySpike {
    let app: ProbeApp
    nonisolated static let vendorID: UInt32 = 0x454d // "EM"
    nonisolated static let productID: UInt32 = 0x0d00
    nonisolated static let serial: UInt32 = 0x2670_1878

    private var placement: CGConfigureOption { app.options.persistArrangement ? .permanently : .forSession }

    private func waitFor(_ timeout: Double, _ condition: () -> Bool) async -> Double? {
        let start = Date()
        while Date().timeIntervalSince(start) < timeout {
            if condition() { return Date().timeIntervalSince(start) }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return condition() ? Date().timeIntervalSince(start) : nil
    }

    private func create(_ spec: VirtualDisplaySpec) async -> CGVirtualDisplay? {
        guard let display = spec.make(queue: .main) else {
            Log.check(false, "\(spec.name): CGVirtualDisplay creation or applySettings failed")
            return nil
        }
        let id = display.displayID
        let online = await waitFor(3) { Displays.online().contains(id) }
        Log.check(online != nil, "\(spec.name): display \(id) online" + (online.map { String(format: " after %.0f ms", $0 * 1000) } ?? ""))
        _ = await waitFor(2) { Displays.screen(id) != nil }
        Log.line("\(spec.name): created \(Displays.describe(id))")
        for mode in Displays.modes(id) {
            Log.line(String(format: "  mode %dx%dpt %dx%dpx %.2fHz flags=0x%x usable=%d", mode.width, mode.height, mode.pixelWidth, mode.pixelHeight, mode.refreshRate, mode.ioFlags, mode.isUsableForDesktopGUI() ? 1 : 0))
        }
        checkHiDPI(spec, id, "as created")
        return display
    }

    private func checkHiDPI(_ spec: VirtualDisplaySpec, _ id: CGDirectDisplayID, _ when: String) {
        let bounds = CGDisplayBounds(id)
        let mode = CGDisplayCopyDisplayMode(id)
        Log.check(Int(bounds.width) == Int(spec.points.0) && Int(bounds.height) == Int(spec.points.1),
                  "\(spec.name) \(when): logical size \(Int(bounds.width))x\(Int(bounds.height)) == \(spec.points.0)x\(spec.points.1)")
        Log.check(mode?.pixelWidth == Int(spec.pixels.0) && mode?.pixelHeight == Int(spec.pixels.1),
                  "\(spec.name) \(when): backing pixels \(mode?.pixelWidth ?? 0)x\(mode?.pixelHeight ?? 0) == \(spec.pixels.0)x\(spec.pixels.1)")
        Log.check(Displays.screen(id)?.backingScaleFactor == 2, "\(spec.name) \(when): NSScreen backingScaleFactor \(Displays.screen(id)?.backingScaleFactor ?? 0) == 2")
    }

    /// Switches to the HiDPI mode at `rate` for this session and checks it.
    private func select(_ spec: VirtualDisplaySpec, _ id: CGDirectDisplayID, rate: Double) async {
        guard let mode = Displays.modes(id).first(where: { $0.width == Int(spec.points.0) && $0.pixelWidth == Int(spec.pixels.0) && abs($0.refreshRate - rate) < 0.5 }) else {
            Log.check(false, "\(spec.name): no \(spec.points.0)x\(spec.points.1)pt / \(spec.pixels.0)x\(spec.pixels.1)px mode at \(rate) Hz")
            return
        }
        var config: CGDisplayConfigRef?
        CGBeginDisplayConfiguration(&config)
        CGConfigureDisplayWithDisplayMode(config, id, mode, nil)
        let error = CGCompleteDisplayConfiguration(config, .forSession)
        Log.check(error == .success, "\(spec.name): select \(rate) Hz (CGError \(error.rawValue))")
        _ = await waitFor(2) { abs((CGDisplayCopyDisplayMode(id)?.refreshRate ?? 0) - rate) < 0.5 }
        Log.line("\(spec.name) @\(rate): \(Displays.describe(id))")
        checkHiDPI(spec, id, "@\(rate)")
        let refresh = CGDisplayCopyDisplayMode(id)?.refreshRate ?? 0
        Log.check(abs(refresh - rate) < 0.5, "\(spec.name) @\(rate): refresh \(refresh)")
    }

    private func arrangeRightOfMain(_ id: CGDirectDisplayID) async {
        let main = CGDisplayBounds(CGMainDisplayID())
        var config: CGDisplayConfigRef?
        CGBeginDisplayConfiguration(&config)
        CGConfigureDisplayOrigin(config, id, Int32(main.maxX), Int32(main.minY))
        let error = CGCompleteDisplayConfiguration(config, placement)
        _ = await waitFor(2) { CGDisplayBounds(id).minX == main.maxX }
        let bounds = CGDisplayBounds(id)
        Log.check(error == .success && bounds.minX == main.maxX && bounds.minY == main.minY,
                  "placed right of main (\(app.options.persistArrangement ? "permanently" : "for session")): origin (\(bounds.minX), \(bounds.minY)), main ends at x=\(main.maxX) (CGError \(error.rawValue))")
    }

    private func logPlacement(_ id: CGDirectDisplayID, _ when: String) {
        let main = CGDisplayBounds(CGMainDisplayID())
        let bounds = CGDisplayBounds(id)
        let side = bounds.minX >= main.maxX ? "right" : bounds.maxX <= main.minX ? "left" : bounds.minY >= main.maxY ? "below" : bounds.maxY <= main.minY ? "above" : "overlapping"
        Log.result("placement", [("when", "\"\(when)\""), ("origin", "(\(Int(bounds.minX)),\(Int(bounds.minY)))"), ("side_of_main", side),
                                 ("right_of_main_at_top", bounds.minX == main.maxX && bounds.minY == main.minY)])
    }

    /// The probe window fills `id`'s visible frame (or goes back to the main
    /// display), so its animation drives capture there and input lands in it.
    private func moveWindow(to id: CGDirectDisplayID?) async {
        let screen = id.flatMap(Displays.screen) ?? NSScreen.screens.first
        guard let screen else { return }
        if id != nil {
            app.window.setFrame(screen.visibleFrame.insetBy(dx: 8, dy: 8), display: true)
        } else {
            app.window.setFrame(NSRect(x: screen.visibleFrame.minX + 200, y: screen.visibleFrame.minY + 200, width: 900, height: 620), display: true)
        }
        app.window.makeKeyAndOrderFront(nil)
        try? await Task.sleep(nanoseconds: 300_000_000)
    }

    /// Releases the display and checks it is gone within 2 s.
    private func remove(_ display: inout CGVirtualDisplay?, _ name: String) async {
        guard let id = display?.displayID else { return }
        await moveWindow(to: nil)
        display = nil
        let gone = await waitFor(5) { !Displays.online().contains(id) }
        Log.check(gone.map { $0 <= 2 } ?? false, "\(name): display \(id) removed " + (gone.map { String(format: "after %.0f ms", $0 * 1000) } ?? "— still online after 5 s"))
        let screens = await waitFor(2) { Displays.screen(id) == nil }
        Log.line("\(name): NSScreen gone \(screens.map { String(format: "after %.0f ms", $0 * 1000) } ?? "no"); online now \(Displays.online())")
    }

    /// A child process owns a display; it is killed, or exits without
    /// releasing it. The display must go with it.
    private func processDeath(exitNormally: Bool) async {
        let label = exitNormally ? "child exits without releasing" : "child killed (SIGKILL)"
        let child = Process()
        child.executableURL = Bundle.main.executableURL
        child.arguments = ["--mode", "vd-child"] + (exitNormally ? ["--exit"] : [])
        let pipe = Pipe()
        child.standardOutput = pipe
        do { try child.run() } catch {
            Log.check(false, "\(label): could not start child: \(error)")
            return
        }
        var output = ""
        let readDeadline = Date().addingTimeInterval(6)
        while !output.contains("\n"), Date() < readDeadline {
            if let chunk = try? pipe.fileHandleForReading.read(upToCount: 64), !chunk.isEmpty {
                output += String(decoding: chunk, as: UTF8.self)
            } else {
                try? await Task.sleep(nanoseconds: 20_000_000)
            }
        }
        guard output.hasPrefix("READY"), let id = CGDirectDisplayID(output.split(separator: " ").last?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "") else {
            Log.check(false, "\(label): child did not create a display (\(output.trimmingCharacters(in: .whitespacesAndNewlines)))")
            child.terminate()
            return
        }
        Log.check(Displays.online().contains(id), "\(label): child pid \(child.processIdentifier) owns display \(id)")
        let start = Date()
        if !exitNormally { kill(child.processIdentifier, SIGKILL) }
        let exited = await waitFor(5) { !child.isRunning }
        let gone = await waitFor(5) { !Displays.online().contains(id) }
        let sinceDeath = gone.map { $0 - (exited ?? 0) }
        Log.result("process_death", [("case", "\"\(label)\""), ("child_exited_ms", exited.map { String(format: "%.0f", $0 * 1000) } ?? "no"),
                                     ("display_gone_ms_after_exit", sinceDeath.map { String(format: "%.0f", $0 * 1000) } ?? "no"),
                                     ("total_ms", String(format: "%.0f", Date().timeIntervalSince(start) * 1000))])
        Log.check(gone != nil && (sinceDeath ?? 99) <= 2, "\(label): display \(id) went away with the process")
    }

    func run() async {
        Log.line("=== spike c: second-screen gate === online before: \(Displays.online().map(Displays.describe))")
        let inner = VirtualDisplaySpec.inner
        app.say("Second-screen gate: adding a virtual display at the Duo's inner size. It is removed again in a few minutes.")
        var display = await create(inner)
        guard let id = display?.displayID else { return }
        logPlacement(id, "inner, first creation (macOS default)")
        await arrangeRightOfMain(id)

        for (index, rate) in inner.rates.enumerated() {
            await select(inner, id, rate: rate)
            await moveWindow(to: id)
            app.say("Second-screen gate: capturing the virtual display at \(Int(rate)) Hz")
            await CaptureSpike(app: app).capture(displayID: id, name: "virtual_\(Int(rate))hz", rates: [Int(rate)],
                                                 idle: index == inner.rates.count - 1)
        }
        app.say("Second-screen gate: input mapped onto the virtual display. Please don't touch the mouse or keyboard.")
        await EventSpike(app: app, display: id).run()

        // The macOS 26 case: a mode change, then removal.
        await select(inner, id, rate: 60)
        await remove(&display, "\(inner.name) after mode changes 60→120→60")

        app.say("Second-screen gate: recreating the virtual display with another mode (Duo outer size).")
        var second = await create(VirtualDisplaySpec.outer)
        if let id2 = second?.displayID {
            logPlacement(id2, "outer, recreated after a placed inner (same vendor/product/serial)")
            await select(.outer, id2, rate: 120)
            await select(.outer, id2, rate: 60)
        }
        await remove(&second, "duo-outer after mode change 120→60")

        app.say("Second-screen gate: creating the inner display a third time.")
        var third = await create(inner)
        if let id3 = third?.displayID {
            logPlacement(id3, "inner, third creation")
            await select(inner, id3, rate: 120)
        }
        await remove(&third, "\(inner.name) third creation after mode change")

        app.say("Second-screen gate: checking the display goes away when its process dies.")
        await processDeath(exitNormally: false)
        await processDeath(exitNormally: true)
        Log.line("online after the gate: \(Displays.online().map(Displays.describe))")
        app.say("Second-screen gate: done; the virtual display is gone.")
    }
}
