import XCTest

final class ConnectScreenTests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func testConnectControls() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES"]
        app.launch()

        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.textFields["connect.port"].exists)
        for identifier in ["connect.button", "connect.scan", "connect.qr", "settings.button"] {
            XCTAssertTrue(app.buttons[identifier].exists, identifier)
        }
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "connect-screen"
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    func testSettingsOpenAndDismiss() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES"]
        app.launch()
        let settings = app.buttons["settings.button"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        settings.tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["settings.done"].exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "settings"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.buttons["settings.done"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
    }
}
