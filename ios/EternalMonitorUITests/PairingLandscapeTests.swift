import XCTest

/// On a phone in landscape the number pad covers the sheet's Pair button, so
/// the navigation bar offers Pair too. iPad never shows it.
final class PairingLandscapeTests: XCTestCase {
    override func tearDown() {
        XCUIDevice.shared.orientation = .portrait
    }

    func testPairStaysReachableAboveTheNumberPad() throws {
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom != .phone, "iPhone only")
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .landscapeLeft
        let app = XCUIApplication()
        app.launchArguments = ["-allowUSB", "NO", "-didSeeOnboarding", "YES"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "pairing"
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 10))
        app.typeText("123456")
        let bar = app.buttons["pairing.submitBar"]
        XCTAssertTrue(bar.waitForExistence(timeout: 5))
        XCTAssertTrue(bar.isHittable && bar.isEnabled)
        XCTAssertLessThan(bar.frame.maxY, app.keyboards.firstMatch.frame.minY)

        XCUIDevice.shared.orientation = .portrait
        XCTAssertTrue(app.buttons["pairing.submit"].waitForExistence(timeout: 5))
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: bar)
        waitForExpectations(timeout: 5)
        XCTAssertTrue(app.buttons["pairing.submit"].isHittable)
    }

    /// The bar says why Pair can't be tapped, and an error shows above the
    /// field, where the number pad doesn't cover it.
    func testErrorsAndWaitingStayAboveTheNumberPad() throws {
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom != .phone, "iPhone only")
        for state in ["pairing-wrong-code", "pairing-rate-limited", "pairing-submitting"] {
            XCUIDevice.shared.orientation = .landscapeLeft
            let app = XCUIApplication()
            app.launchArguments = ["-allowUSB", "NO", "-didSeeOnboarding", "YES"]
            app.launchEnvironment["EM_UI_PREVIEW"] = state
            app.launch()
            let keyboard = app.keyboards.firstMatch
            XCTAssertTrue(keyboard.waitForExistence(timeout: 10), state)
            let bar = app.buttons["pairing.submitBar"]
            switch state {
            case "pairing-submitting":
                XCTAssertTrue(app.activityIndicators["pairing.submitBar.progress"].waitForExistence(timeout: 5), state)
                XCTAssertFalse(bar.exists, state)
            case "pairing-rate-limited":
                XCTAssertTrue(bar.waitForExistence(timeout: 5), state)
                XCTAssertTrue(bar.label.hasPrefix("Try again in "), bar.label)
                XCTAssertFalse(bar.isEnabled, state)
            default:
                XCTAssertTrue(bar.waitForExistence(timeout: 5), state)
                XCTAssertEqual(bar.label, "Pair")
                XCTAssertTrue(bar.isEnabled, state)
            }
            if state != "pairing-submitting" {
                let error = app.staticTexts["pairing.error"]
                XCTAssertTrue(error.waitForExistence(timeout: 5), state)
                XCTAssertLessThan(error.frame.maxY, keyboard.frame.minY, state)
                XCTAssertTrue(error.isHittable, state)
            }
            if let out = ProcessInfo.processInfo.environment["DUO_OUT"] {
                try? XCUIScreen.main.screenshot().pngRepresentation
                    .write(to: URL(fileURLWithPath: "\(out)/\(state)-landscape.png"))
            }
            app.terminate()
        }
    }
}
