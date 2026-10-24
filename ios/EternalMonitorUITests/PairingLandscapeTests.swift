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
}
