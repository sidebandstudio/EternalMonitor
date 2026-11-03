import XCTest

/// The HUD slides away five seconds after it appears. Once it starts leaving,
/// a tap anywhere along its way out must reach the stream, so the
/// three-finger reveal works there instead of pressing a HUD button.
final class HUDHideTests: UITestCase {
    func testLeavingHUDPassesTapsToTheStream() throws {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let environment = ProcessInfo.processInfo.environment
        let app = XCUIApplication()
        // A ten-second slide keeps the HUD leaving while every tap lands.
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES",
                               "-playPCaudio", "NO", "-hudHideSeconds", "10", "-pictureFit", "fit",
                               "-uiTestProbe", "YES"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        launchCleanly(app)
        defer { app.terminate() }
        let keyboard = app.buttons["display.keyboard"]
        // The HUD can come and go while a starved runner waits for the app
        // to idle; the probe still shows that it appeared.
        app.waitForDisplayAndHUD()
        let shown = app.hudControls(["display.keyboard", "display.disconnect"], reveal: .threeFingersTogether)
        let keyboardFrame = try XCTUnwrap(shown["display.keyboard"]).frame
        let disconnectFrame = try XCTUnwrap(shown["display.disconnect"]).frame
        let middle = center(app.frame)
        let bottom = app.frame.maxY - 5
        // An expectation polls about once a second; this loop sees the HUD
        // leave within a few milliseconds.
        let deadline = Date().addingTimeInterval(10)
        while keyboard.exists && Date() < deadline {}
        XCTAssertFalse(keyboard.exists, "The HUD did not start hiding")
        // iOS 18 lets a leaving button take touches where its slide ends,
        // not where it is drawn, so tap every point down to the screen edge.
        for frame in [disconnectFrame, keyboardFrame] {
            for y in stride(from: frame.midY, to: bottom, by: 12) + [bottom] {
                tap([CGPoint(x: frame.midX, y: y)])
            }
        }
        // XCTest's own three-finger tap puts a finger at the bottom edge.
        tap([CGPoint(x: keyboardFrame.midX, y: bottom),
             CGPoint(x: middle.x - 60, y: middle.y), CGPoint(x: middle.x + 60, y: middle.y)])
        // Every touch reached the stream: the HUD comes back, and neither
        // the keyboard nor Disconnect was pressed.
        if !keyboard.waitForExistence(timeout: 5) {
            XCTFail(app.textFields["connect.host"].exists ? "A tap pressed Disconnect"
                : "The three-finger tap did not bring the HUD back")
        }
        let back = app.hudControls(["display.keyboard", "display.disconnect"], reveal: .threeFingersTogether)
        XCTAssertEqual(back["display.keyboard"]?.label, "Keyboard", "A tap pressed the Keyboard button")
        XCTAssertFalse(app.keyboards.firstMatch.exists)
    }

    /// A VoiceOver user reads the HUD at their own pace: it must not hide
    /// while VoiceOver runs.
    func testHUDStaysWhileVoiceOverRuns() {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES",
                               "-playPCaudio", "NO", "-assumeVoiceOver", "YES", "-pictureFit", "fit"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        launchCleanly(app)
        defer { app.terminate() }
        let keyboard = app.buttons["display.keyboard"]
        XCTAssertTrue(keyboard.waitForExistence(timeout: 15))
        // Past the five-second timer and the slide out.
        let gone = expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: keyboard)
        XCTAssertEqual(XCTWaiter().wait(for: [gone], timeout: 8), .timedOut, "The HUD hid while VoiceOver ran")
        XCTAssertTrue(app.buttons["display.disconnect"].exists)
    }

    private func center(_ frame: CGRect) -> CGPoint {
        CGPoint(x: frame.midX, y: frame.midY)
    }

    private func tap(_ points: [CGPoint]) {
        let tapped = expectation(description: "Tap")
        EMTapFingers(points.map { NSValue(cgPoint: $0) }) { success, error in
            XCTAssertTrue(success, error?.localizedDescription ?? "Touch synthesis failed")
            tapped.fulfill()
        }
        wait(for: [tapped], timeout: 10)
    }
}
