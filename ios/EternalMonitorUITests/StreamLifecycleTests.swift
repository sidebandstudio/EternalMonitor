import XCTest

final class StreamLifecycleTests: XCTestCase {
    private var hostLog: URL {
        URL(fileURLWithPath: ProcessInfo.processInfo.environment["EM_INPUT_HOST_LOG"]!)
    }

    private func launch() -> XCUIApplication {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "NO",
            "-playPCaudio", "NO", "-autoReconnect", "YES", "-autoResumeOnForeground", "YES", "-targetFPS", "60", "-pictureFit", "fit"]
        app.launchEnvironment["EM_AUTOCONNECT"] = ProcessInfo.processInfo.environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        app.launch()
        waitForSignal("On air", app: app, timeout: 15)
        return app
    }

    func testHostRestart() throws {
        let directory = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["EM_LIFECYCLE_DIR"]))
        let before = readHostLog().count
        let app = launch()
        // Wait for multiple reports so the liveness test starts after a real heartbeat.
        expectation(for: NSPredicate { _, _ in
            self.readHostLog().dropFirst(before).components(separatedBy: "Receiver report").count >= 3
        }, evaluatedWith: nil)
        waitForExpectations(timeout: 5)
        let started = ProcessInfo.processInfo.systemUptime
        try Data().write(to: directory.appendingPathComponent("stop.request"))
        waitForSignal("Signal lost, reconnecting", app: app, timeout: 7)
        capture("signal-lost", app: app)
        try Data().write(to: directory.appendingPathComponent("start.request"))
        waitForSignal("On air", app: app, timeout: 15)
        let elapsed = ProcessInfo.processInfo.systemUptime - started
        XCTAssertLessThan(elapsed, 15, "Host restart must recover within fifteen seconds")
        XCTAssertTrue(FileManager.default.fileExists(atPath: directory.appendingPathComponent("restarted.json").path))
        print("E2E_LIFECYCLE_RESUMED elapsed=\(elapsed)")
        capture("stream-resumed", app: app)
        disconnect(app)
    }

    func testBackgroundResume() {
        let app = launch()
        // Repeat in one process to cover task cleanup and the next assertion.
        for cycle in 1...3 {
            let before = readHostLog().count
            XCUIDevice.shared.press(.home)
            XCTAssertTrue(app.wait(for: .runningBackground, timeout: 5))
            expectation(for: NSPredicate { _, _ in
                self.readHostLog().dropFirst(before).contains("AppBackground")
            }, evaluatedWith: nil)
            waitForExpectations(timeout: 5)
            print("E2E_LIFECYCLE_BACKGROUND bye=AppBackground cycle=\(cycle)")
            let started = ProcessInfo.processInfo.systemUptime
            app.activate()
            waitForSignal("On air", app: app, timeout: 15)
            print("E2E_LIFECYCLE_FOREGROUND elapsed=\(ProcessInfo.processInfo.systemUptime - started) cycle=\(cycle)")
        }
        capture("background-resumed", app: app)
        disconnect(app)
    }

    private func readHostLog() -> String { (try? String(contentsOf: hostLog, encoding: .utf8)) ?? "" }

    /// The signal pill is part of the HUD, which hides itself after five
    /// seconds, so the stream can be on air with the pill gone. Read one
    /// accessibility snapshot per check, and once the display is up (the
    /// connect screen's settings button is gone) with the pill off screen,
    /// reveal the HUD, at most every two seconds so a reveal that is still
    /// animating in isn't toggled off again.
    private func waitForSignal(_ label: String, app: XCUIApplication, timeout: TimeInterval,
                               file: StaticString = #filePath, line: UInt = #line) {
        let deadline = Date().addingTimeInterval(timeout)
        var revealed = Date.distantPast
        while Date() < deadline {
            if let tree = try? app.snapshot() {
                if let signal = Self.find("display.signal", in: tree), Self.onScreen(signal) {
                    if signal.label == label { return }
                } else if Self.find("settings.button", in: tree) == nil, Date().timeIntervalSince(revealed) > 2 {
                    app.revealHUD(.tap)
                    revealed = Date()
                }
            }
            Thread.sleep(forTimeInterval: 0.25)
        }
        XCTFail("The display never showed \"\(label)\"", file: file, line: line)
    }

    private static func find(_ identifier: String, in element: XCUIElementSnapshot) -> XCUIElementSnapshot? {
        if element.identifier == identifier { return element }
        for child in element.children {
            if let found = find(identifier, in: child) { return found }
        }
        return nil
    }

    private static func onScreen(_ element: XCUIElementSnapshot) -> Bool {
        element.frame.minX.isFinite && element.frame.minY.isFinite && !element.frame.isEmpty
    }

    private func disconnect(_ app: XCUIApplication) {
        // The HUD can hide between a check and the tap (CI: checked at
        // 25.03 s, gone by the tap at 25.09 s).
        app.tapHUDControl("display.disconnect", until: app.textFields["connect.host"])
        app.terminate()
    }

    private func capture(_ name: String, app: XCUIApplication) {
        // "On air" marks the first decoded frame. Its display view can still
        // be fading in, which darkens the pattern in an immediate screenshot.
        Thread.sleep(forTimeInterval: 1)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
