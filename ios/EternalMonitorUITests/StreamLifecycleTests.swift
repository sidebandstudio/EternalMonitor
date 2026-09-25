import XCTest

final class StreamLifecycleTests: XCTestCase {
    private var hostLog: URL {
        URL(fileURLWithPath: ProcessInfo.processInfo.environment["EM_INPUT_HOST_LOG"]!)
    }

    private func launch() -> XCUIApplication {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "NO",
            "-playPCaudio", "NO", "-autoReconnect", "YES", "-autoResumeOnForeground", "YES", "-targetFPS", "60"]
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

    private func waitForSignal(_ label: String, app: XCUIApplication, timeout: TimeInterval) {
        let signal = app.descendants(matching: .any)["display.signal"].firstMatch
        expectation(for: NSPredicate(format: "exists == true AND label == %@", label), evaluatedWith: signal)
        waitForExpectations(timeout: timeout)
    }

    private func disconnect(_ app: XCUIApplication) {
        if !app.buttons["display.disconnect"].exists { app.tap() }
        app.buttons["display.disconnect"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
        app.terminate()
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
