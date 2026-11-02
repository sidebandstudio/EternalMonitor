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
        // The HUD can come and go while a starved runner waits for the app
        // to idle. Wait for the display, then read or reveal the HUD.
        XCTAssertTrue(app.otherElements["display.relay"].waitForExistence(timeout: 15))
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
        XCTAssertEqual(value(of: control, app: app), "Off")
        closeKeyboard(with: app.keyboards.buttons["Hide keyboard"], app: app)
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
        closeKeyboard(with: app.buttons["keyboard.done"], app: app)
        verifyDismissalAndRestoreHUD(app: app, keyboard: keyboard)
        app.tapHUDControl("display.disconnect", until: app.textFields["connect.host"], reveal: .threeFingersTogether)
        app.terminate()
    }

    /// Set once `restoreKeyboard` opened the keyboard again.
    private var reopened = false

    /// When the PC's heartbeats stop for 3 s the app shows Reconnecting and
    /// closes the keyboard itself. On #164's starved runner the host
    /// restarted its stalled pipeline while XCTest spent 6 s finding Hide
    /// keyboard, so the key vanished mid-tap. Wait for the picture and open
    /// the keyboard again.
    private func restoreKeyboard(app: XCUIApplication) {
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: app.staticTexts["Reconnecting to your PC"])
        waitForExpectations(timeout: 15)
        guard !app.keyboards.firstMatch.exists else { return }
        XCTContext.runActivity(named: "Open the keyboard the lost signal closed") { _ in
            app.tapHUDControl("display.keyboard", until: app.keyboards.firstMatch, reveal: .threeFingersTogether)
        }
        reopened = true
    }

    /// A bar key's value, read again in a restored keyboard if the key is
    /// gone. A snapshot throws, instead of failing the test, if it is.
    private func value(of key: XCUIElement, app: XCUIApplication) -> String? {
        for _ in 0..<3 {
            if let snapshot = try? key.snapshot() { return snapshot.value as? String }
            restoreKeyboard(app: app)
        }
        return nil
    }

    /// Closes the keyboard with `key`, a key on the keyboard or its bar, by
    /// touching the key's last position. If the keyboard closed while the
    /// signal was lost, restores it and closes it again.
    private func closeKeyboard(with key: XCUIElement, app: XCUIApplication,
                               file: StaticString = #filePath, line: UInt = #line) {
        let shown = app.keyboards.firstMatch
        for _ in 0..<3 {
            restoreKeyboard(app: app)
            // The simulator takes typeText for a hardware keyboard, so the
            // keyboard opens again as its bar alone, as it does for Done
            // below, with Hide keyboard under the screen's edge. Done closes
            // a restored keyboard.
            let target = reopened ? app.buttons["keyboard.done"] : key
            guard let frame = try? target.snapshot().frame else { continue }
            app.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
            let closed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: shown)
            if XCTWaiter().wait(for: [closed], timeout: 5) == .completed
                && !app.staticTexts["Reconnecting to your PC"].exists { return }
        }
        XCTFail("\(key) did not close the keyboard", file: file, line: line)
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
        let controls = app.hudControls(["display.keyboard", "display.disconnect"], reveal: .threeFingersTogether)
        XCTAssertEqual(controls["display.keyboard"]?.label, "Keyboard")
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
