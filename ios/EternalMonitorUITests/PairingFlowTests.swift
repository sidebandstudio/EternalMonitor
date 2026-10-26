import XCTest

final class PairingFlowTests: XCTestCase {
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
        app.launch()
        let input = app.textFields["pairing.code"]
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        input.tap()
        let wrongCode = code == "000000" ? "000001" : "000000"
        input.typeText(wrongCode)
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
        disconnect(app)
        XCTAssertTrue(app.images["recent.paired"].firstMatch.exists)
        app.terminate()
        // A new process must obtain its token from Keychain and skip the sheet.
        app.launch()
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
        app.launch()
        XCTAssertTrue(input.waitForExistence(timeout: 10), "Forget paired hosts must require a code again")
        app.buttons["pairing.cancel"].tap()
        app.terminate()
    }

    private func disconnect(_ app: XCUIApplication) {
        app.tapHUDControl("display.disconnect", until: app.textFields["connect.host"])
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
