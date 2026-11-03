import XCTest

final class PairingFlowTests: UITestCase {
    func testPairingFlow() throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let code = try XCTUnwrap(environment["EM_PAIRING_CODE"], "Harness must read the real host's startup code")
        let host = try XCTUnwrap(environment["EM_PAIRING_HOST"])
        XCTAssertEqual(code.count, 6)
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "NO", "-playPCaudio", "NO", "-pictureFit", "fit"]
        app.launchEnvironment["EM_AUTOCONNECT"] = host
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        launchCleanly(app)
        let input = app.textFields["pairing.code"]
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        input.tap()
        // Not the "000000" placeholder, so a cleared field reads differently.
        let wrongCode = code == "111111" ? "222222" : "111111"
        // The sixth digit pairs. On iPadOS 27 the number pad opens over the
        // Pair button, so typing the code must be the only step.
        input.typeText(wrongCode)
        let error = app.staticTexts["pairing.error"]
        XCTAssertTrue(error.waitForExistence(timeout: 5))
        XCTAssertTrue(error.label.contains("didn’t match"))
        XCTAssertTrue(input.exists)
        capture("pairing-wrong-code", app: app)
        // The field keeps its focus, with the caret after the refused code;
        // another tap can move it before the digits. A busy simulator can
        // drop keys, and the refused code or five digits never pair, so read
        // each step back: a lost key must not look like a PC that ignored
        // the right code.
        let start = String(code.dropLast())
        XCTAssertTrue(replace(input, with: start), "The field holds \(value(of: input)), not \(start)")
        XCTAssertTrue(type(String(code.suffix(1)), into: input, until: { $0 != start }),
                      "The last digit did not reach the field")
        let connected = app.buttons["display.disconnect"].waitForExistence(timeout: 10)
        if !connected { capture("pairing-right-code", app: app) }
        XCTAssertTrue(connected, "The right code did not pair; the field holds \(value(of: input)), error: \(errorText(app))")
        // The stream can start while the code sheet is still closing; wait
        // for the sheet to go. The timeout is only a backstop.
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: input)
        waitForExpectations(timeout: 30)
        capture("pairing-connected", app: app)
        disconnect(app)
        XCTAssertTrue(app.images["recent.paired"].firstMatch.exists)
        app.terminate()
        // A new process must obtain its token from Keychain and skip the sheet.
        launchCleanly(app)
        XCTAssertTrue(app.buttons["display.disconnect"].waitForExistence(timeout: 10))
        XCTAssertFalse(input.exists)
        capture("pairing-token-reconnect", app: app)
        app.tapHUDControl("display.settings", until: app.navigationBars["Settings"])
        let forget = app.buttons["settings.forgetPairings"]
        XCTAssertTrue(app.swipeUp(toReveal: forget))
        forget.tap()
        app.buttons["settings.done"].tap()
        disconnect(app)
        app.terminate()
        launchCleanly(app)
        XCTAssertTrue(input.waitForExistence(timeout: 10), "Forget paired hosts must require a code again")
        app.buttons["pairing.cancel"].tap()
        app.terminate()
    }

    private func disconnect(_ app: XCUIApplication) {
        app.tapHUDControl("display.disconnect", until: app.textFields["connect.host"])
    }

    /// Empties the field, then types `text`, until it reads back. The field
    /// is empty before each try, so dropped keys never make a whole code.
    private func replace(_ input: XCUIElement, with text: String) -> Bool {
        let clear = String(repeating: XCUIKeyboardKey.delete.rawValue, count: 6)
        let placeholder = (try? input.snapshot())?.placeholderValue
        for _ in 0..<3 {
            guard type(clear, into: input, until: { $0 == "" || $0 == placeholder }) else { return false }
            if type(text, into: input, until: { $0 == text }, tries: 1) { return true }
        }
        return false
    }

    /// Types `keys` until the field's value passes `done`; a later try
    /// only replaces keys the simulator dropped.
    private func type(_ keys: String, into input: XCUIElement, until done: (String) -> Bool, tries: Int = 3) -> Bool {
        for _ in 0..<tries {
            input.typeText(keys)
            let deadline = Date().addingTimeInterval(3)
            while Date() < deadline {
                if done(value(of: input)) { return true }
                Thread.sleep(forTimeInterval: 0.1)
            }
        }
        return false
    }

    /// The field's text (an empty field has no value); "gone" once the
    /// sheet has closed.
    private func value(of input: XCUIElement) -> String {
        guard let snapshot = try? input.snapshot() else { return "gone" }
        return snapshot.value as? String ?? ""
    }

    private func errorText(_ app: XCUIApplication) -> String {
        (try? app.staticTexts["pairing.error"].snapshot())?.label ?? "none"
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
