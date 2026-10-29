import XCTest

final class KeyboardRelayTests: UITestCase {
    func testKeyboardRelay() throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["EM_INPUT_HOST_LOG"])
        let before = (try? Data(contentsOf: URL(fileURLWithPath: path)).count) ?? 0
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES", "-playPCaudio", "NO", "-pictureFit", "fit"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        launchCleanly(app)
        let keyboard = app.buttons["display.keyboard"]
        XCTAssertTrue(keyboard.waitForExistence(timeout: 15))
        app.tapHUDControl("display.keyboard", until: app.keyboards.firstMatch, reveal: .threeFingersTogether)
        app.typeText("Hi!\n")
        let commands = ["Unicode(72)", "Unicode(105)", "Unicode(33)", "KeyDown { scan: 28, extended: false }", "KeyUp { scan: 28, extended: false }"]
        let relayed = NSPredicate { _, _ in
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)), data.count >= before else { return false }
            let log = String(decoding: data.dropFirst(before), as: UTF8.self)
            return commands.allSatisfy { log.contains($0) }
        }
        expectation(for: relayed, evaluatedWith: nil)
        waitForExpectations(timeout: 10)
        capture("keyboard-relay", app: app)
        let control = app.buttons["keyboard.key.224"]
        control.tap()
        XCTAssertEqual(control.value as? String, "On")
        app.buttons["keyboard.key.80"].tap()
        XCTAssertEqual(control.value as? String, "Off")
        app.keyboards.buttons["Hide keyboard"].tap()
        verifyDismissalAndRestoreHUD(app: app, keyboard: keyboard)
        capture("keyboard-dismissed", app: app)
        // A slow screenshot can use up the HUD's 5 s timer (CI took 2.3 s
        // here while the simulator's decoder stalled), and on a starved runner
        // XCTest's own idle wait and hit test outlasted it (5 s on #75), so the
        // button vanished mid-tap. Let the HUD hide, then reveal and tap in
        // one step, again if it hides first.
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: keyboard)
        waitForExpectations(timeout: 6)
        app.tapHUDControl("display.keyboard", until: app.buttons["keyboard.done"], reveal: .threeFingersTogether)
        app.buttons["keyboard.done"].tap()
        verifyDismissalAndRestoreHUD(app: app, keyboard: keyboard)
        app.buttons["display.disconnect"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
        app.terminate()
    }

    private func verifyDismissalAndRestoreHUD(app: XCUIApplication, keyboard: XCUIElement) {
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: app.keyboards.firstMatch)
        waitForExpectations(timeout: 5)
        // Accessibility queries may outlast the normal HUD timer. Prove the
        // timeout first, then bring the controls back before checking state.
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: keyboard)
        waitForExpectations(timeout: 6)
        revealHUD(app: app)
        XCTAssertTrue(keyboard.waitForExistence(timeout: 5))
        XCTAssertEqual(keyboard.label, "Keyboard")
        XCTAssertTrue(app.buttons["display.disconnect"].exists)
    }

    /// XCTest spreads a three-finger tap from the top edge to the bottom
    /// edge. Its bottom finger lands where the sliding bar's Keyboard button
    /// is near the end of the hide animation, which opened the keyboard
    /// again in CI. Tap with three fingers together in the middle instead.
    private func revealHUD(app: XCUIApplication) {
        app.revealHUD(.threeFingersTogether)
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
