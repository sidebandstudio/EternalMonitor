import XCTest

final class PairingFlowTests: XCTestCase {
    func testPairingFlow() throws {
        continueAfterFailure = false
        let environment = ProcessInfo.processInfo.environment
        let code = try XCTUnwrap(environment["EM_PAIRING_CODE"], "Harness must read the real host's startup code")
        let host = try XCTUnwrap(environment["EM_PAIRING_HOST"])
        XCTAssertEqual(code.count, 6)
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "NO", "-playPCaudio", "NO"]
        app.launchEnvironment["EM_AUTOCONNECT"] = host
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        app.launch()
        let input = app.textFields["pairing.code"]
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        input.tap()
        input.typeText("000000")
        app.buttons["pairing.submit"].tap()
        let error = app.staticTexts["pairing.error"]
        XCTAssertTrue(error.waitForExistence(timeout: 5))
        XCTAssertTrue(error.label.contains("didn’t match"))
        XCTAssertTrue(input.exists)
        capture("pairing-wrong-code", app: app)
        input.tap()
        input.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: 6) + code)
        app.buttons["pairing.submit"].tap()
        XCTAssertTrue(app.buttons["display.disconnect"].waitForExistence(timeout: 10))
        XCTAssertFalse(input.exists)
        capture("pairing-connected", app: app)
        app.buttons["display.disconnect"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.images["recent.paired"].firstMatch.exists)
        app.terminate()
        // A new process must obtain its token from Keychain and skip the sheet.
        app.launch()
        XCTAssertTrue(app.buttons["display.disconnect"].waitForExistence(timeout: 10))
        XCTAssertFalse(input.exists)
        capture("pairing-token-reconnect", app: app)
        app.buttons["display.settings"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        let forget = app.buttons["settings.forgetPairings"]
        for _ in 0..<3 where !forget.isHittable { app.swipeUp() }
        XCTAssertTrue(forget.isHittable)
        forget.tap()
        app.buttons["settings.done"].tap()
        app.buttons["display.disconnect"].tap()
        app.terminate()
        app.launch()
        XCTAssertTrue(input.waitForExistence(timeout: 10), "Forget paired hosts must require a code again")
        app.buttons["pairing.cancel"].tap()
        app.terminate()
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
