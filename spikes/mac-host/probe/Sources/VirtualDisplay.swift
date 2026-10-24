// Spike c: a CGVirtualDisplay at the Duo's inner size, made the way
// Chromium's virtual_display_mac_util does: HiDPI on, modes at half the
// pixel size, maxPixels at the full size, and a vendor ID and serial of its
// own. Checks the logical size and backing pixels at 60 and 120 Hz,
// captures it (spike d), arranges it right of the main display for this
// session only, removes it (gone within 2 s) and recreates it with a new
// mode. Every configuration change is kCGConfigureForSession.
import AppKit
import CoreGraphics

@MainActor
struct VirtualDisplaySpike {
    let app: ProbeApp
    static let vendorID: UInt32 = 0x454d // "EM"
    static let productID: UInt32 = 0x0d00
    static let serial: UInt32 = 0x2670_1878

    struct Spec {
        let name: String
        let pixels: (UInt32, UInt32)
        let rates: [Double]
        var points: (UInt32, UInt32) { (pixels.0 / 2, pixels.1 / 2) }
    }

    static func onlineDisplays() -> [CGDirectDisplayID] {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var count: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &count)
        return Array(ids.prefix(Int(count)))
    }

    static func describe(_ id: CGDirectDisplayID) -> String {
        let bounds = CGDisplayBounds(id)
        let mode = CGDisplayCopyDisplayMode(id)
        let screen = NSScreen.screens.first { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id }
        let mm = CGDisplayScreenSize(id)
        return String(format: "id=%u bounds=(%.0f,%.0f %.0fx%.0f) mode=%dx%dpt %dx%dpx %.2fHz scale=%@ mm=%.0fx%.0f vendor=0x%x model=0x%x serial=0x%x main=%d mirror=%u",
                      id, bounds.origin.x, bounds.origin.y, bounds.width, bounds.height,
                      mode?.width ?? 0, mode?.height ?? 0, mode?.pixelWidth ?? 0, mode?.pixelHeight ?? 0, mode?.refreshRate ?? 0,
                      screen.map { String(format: "%.1f", $0.backingScaleFactor) } ?? "none", mm.width, mm.height,
                      CGDisplayVendorNumber(id), CGDisplayModelNumber(id), CGDisplaySerialNumber(id),
                      CGDisplayIsMain(id) != 0 ? 1 : 0, CGDisplayMirrorsDisplay(id))
    }

    static func modes(_ id: CGDirectDisplayID) -> [CGDisplayMode] {
        let options = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
        return (CGDisplayCopyAllDisplayModes(id, options) as? [CGDisplayMode]) ?? []
    }

    private func waitFor(_ timeout: Double, _ condition: () -> Bool) async -> Double? {
        let start = Date()
        while Date().timeIntervalSince(start) < timeout {
            if condition() { return Date().timeIntervalSince(start) }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return condition() ? Date().timeIntervalSince(start) : nil
    }

    private func create(_ spec: Spec) async -> CGVirtualDisplay? {
        let descriptor = CGVirtualDisplayDescriptor()
        descriptor.queue = DispatchQueue.main
        descriptor.name = spec.name
        descriptor.maxPixelsWide = spec.pixels.0
        descriptor.maxPixelsHigh = spec.pixels.1
        // About 413 ppi, a phone panel.
        descriptor.sizeInMillimeters = CGSize(width: Double(spec.pixels.0) / 413 * 25.4, height: Double(spec.pixels.1) / 413 * 25.4)
        descriptor.vendorID = Self.vendorID
        descriptor.productID = Self.productID
        descriptor.serialNum = Self.serial
        descriptor.terminationHandler = { _, display in
            Log.line("virtual display terminationHandler id=\(display.displayID)")
        }
        guard let display = CGVirtualDisplay(descriptor: descriptor) else {
            Log.check(false, "CGVirtualDisplay(descriptor:) returned nil")
            return nil
        }
        let settings = CGVirtualDisplaySettings()
        settings.hiDPI = 1
        settings.modes = spec.rates.map { CGVirtualDisplayMode(width: spec.points.0, height: spec.points.1, refreshRate: $0) }
        let applied = display.apply(settings)
        Log.check(applied, "\(spec.name): applySettings(hiDPI=1, modes \(spec.points.0)x\(spec.points.1) @ \(spec.rates))")
        let id = display.displayID
        let online = await waitFor(3) { Self.onlineDisplays().contains(id) }
        Log.check(online != nil, "\(spec.name): display \(id) online" + (online.map { String(format: " after %.0f ms", $0 * 1000) } ?? ""))
        _ = await waitFor(2) { NSScreen.screens.contains { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id } }
        Log.line("\(spec.name): \(Self.describe(id))")
        for mode in Self.modes(id) {
            Log.line(String(format: "  mode %dx%dpt %dx%dpx %.2fHz flags=0x%x usable=%d", mode.width, mode.height, mode.pixelWidth, mode.pixelHeight, mode.refreshRate, mode.ioFlags, mode.isUsableForDesktopGUI() ? 1 : 0))
        }
        return display
    }

    /// Switches the display to its HiDPI mode at `rate` for this session and
    /// checks the logical size and the backing pixels.
    private func select(_ spec: Spec, _ id: CGDirectDisplayID, rate: Double) async {
        guard let mode = Self.modes(id).first(where: { $0.width == Int(spec.points.0) && $0.pixelWidth == Int(spec.pixels.0) && abs($0.refreshRate - rate) < 0.5 }) else {
            Log.check(false, "\(spec.name): no \(spec.points.0)x\(spec.points.1)pt / \(spec.pixels.0)x\(spec.pixels.1)px mode at \(rate) Hz")
            return
        }
        var config: CGDisplayConfigRef?
        CGBeginDisplayConfiguration(&config)
        CGConfigureDisplayWithDisplayMode(config, id, mode, nil)
        let error = CGCompleteDisplayConfiguration(config, .forSession)
        Log.check(error == .success, "\(spec.name): select \(rate) Hz mode (CGError \(error.rawValue))")
        _ = await waitFor(2) { abs((CGDisplayCopyDisplayMode(id)?.refreshRate ?? 0) - rate) < 0.5 }
        let current = CGDisplayCopyDisplayMode(id)
        let bounds = CGDisplayBounds(id)
        Log.line("\(spec.name) @\(rate): \(Self.describe(id))")
        Log.check(Int(bounds.width) == Int(spec.points.0) && Int(bounds.height) == Int(spec.points.1),
                  "\(spec.name) @\(rate): logical size \(Int(bounds.width))x\(Int(bounds.height)) == \(spec.points.0)x\(spec.points.1)")
        Log.check(current?.pixelWidth == Int(spec.pixels.0) && current?.pixelHeight == Int(spec.pixels.1),
                  "\(spec.name) @\(rate): backing pixels \(current?.pixelWidth ?? 0)x\(current?.pixelHeight ?? 0) == \(spec.pixels.0)x\(spec.pixels.1)")
        Log.check(abs((current?.refreshRate ?? 0) - rate) < 0.5, "\(spec.name) @\(rate): refresh \(current?.refreshRate ?? 0)")
    }

    private func arrangeRightOfMain(_ id: CGDirectDisplayID) async {
        let main = CGDisplayBounds(CGMainDisplayID())
        var config: CGDisplayConfigRef?
        CGBeginDisplayConfiguration(&config)
        CGConfigureDisplayOrigin(config, id, Int32(main.maxX), Int32(main.minY))
        let error = CGCompleteDisplayConfiguration(config, .forSession)
        _ = await waitFor(2) { CGDisplayBounds(id).minX == main.maxX }
        let bounds = CGDisplayBounds(id)
        Log.check(error == .success && bounds.minX == main.maxX && bounds.minY == main.minY,
                  "arranged right of main: origin (\(bounds.minX), \(bounds.minY)), main ends at x=\(main.maxX) (CGError \(error.rawValue))")
    }

    /// Moves the probe window (and its animation) onto `id` so capture has
    /// changing content there, or back to the main display.
    private func moveWindow(to id: CGDirectDisplayID?) {
        let target = id.flatMap { id in NSScreen.screens.first { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id } } ?? NSScreen.main
        guard let screen = target else { return }
        let frame = screen.visibleFrame
        let size = app.window.frame.size
        app.window.setFrameOrigin(NSPoint(x: frame.minX + max(0, (frame.width - size.width) / 2), y: frame.minY + max(0, (frame.height - size.height) / 2)))
    }

    private func remove(_ display: inout CGVirtualDisplay?, _ name: String) async {
        guard let id = display?.displayID else { return }
        moveWindow(to: nil)
        display = nil // releasing the object removes the display
        let gone = await waitFor(5) { !Self.onlineDisplays().contains(id) }
        Log.check(gone.map { $0 <= 2 } ?? false, "\(name): display \(id) gone " + (gone.map { String(format: "after %.0f ms", $0 * 1000) } ?? "after more than 5 s"))
        let screens = await waitFor(2) { !NSScreen.screens.contains { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id } }
        Log.line("\(name): NSScreen gone \(screens.map { String(format: "after %.0f ms", $0 * 1000) } ?? "no"); online now \(Self.onlineDisplays())")
    }

    func run() async {
        Log.line("=== spike c: virtual display === online before: \(Self.onlineDisplays().map { Self.describe($0) })")
        let inner = Spec(name: "duo-inner", pixels: (2670, 1878), rates: [60, 120])
        app.say("Spike c: adding a virtual display (Duo inner size). It will be removed again in about a minute.")
        var display = await create(inner)
        guard let id = display?.displayID else { return }
        await arrangeRightOfMain(id)
        for rate in inner.rates {
            await select(inner, id, rate: rate)
            moveWindow(to: id)
            try? await Task.sleep(nanoseconds: 500_000_000)
            await CaptureSpike(app: app).capture(displayID: id, name: "virtual_\(Int(rate))hz", rates: [Int(rate)])
        }
        Log.line("after capture: \(Self.describe(id))")
        await remove(&display, inner.name)

        let outer = Spec(name: "duo-outer", pixels: (2034, 1398), rates: [120, 60])
        app.say("Spike c: recreating the virtual display with a new mode (Duo outer size).")
        var second = await create(outer)
        if let id2 = second?.displayID {
            let main = CGDisplayBounds(CGMainDisplayID())
            let bounds = CGDisplayBounds(id2)
            Log.line("recreated id=\(id2) (first was \(id)); origin (\(bounds.minX), \(bounds.minY)); right of main remembered: \(bounds.minX == main.maxX)")
            await select(outer, id2, rate: 120)
        }
        await remove(&second, outer.name)
        app.say("Spike c: done; the virtual display is gone.")
    }
}
