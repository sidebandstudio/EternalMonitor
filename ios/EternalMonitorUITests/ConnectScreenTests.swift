import XCTest

final class ConnectScreenTests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func testConnectControls() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-lastHost", "127.0.0.1", "-lastPort", "9876"]
        app.launch()

        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.textFields["connect.port"].exists)
        for identifier in ["connect.button", "connect.scan", "connect.qr", "settings.button"] {
            XCTAssertTrue(app.buttons[identifier].exists, identifier)
        }
        XCTAssertEqual(app.textFields["connect.host"].value as? String, "127.0.0.1")
        XCTAssertTrue(app.buttons["connect.button"].isEnabled)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "connect-screen"
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    func testSettingsOpenAndDismiss() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-lastHost", "127.0.0.1", "-lastPort", "9876"]
        app.launch()
        let settings = app.buttons["settings.button"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        settings.tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["settings.done"].exists)
        let allowUSB = app.switches["settings.allowUSB"]
        XCTAssertTrue(allowUSB.exists)
        XCTAssertEqual(allowUSB.value as? String, "1")
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "settings"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        // SwiftUI exposes the whole form row as a switch. Tap its thumb.
        allowUSB.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        XCTAssertEqual(allowUSB.value as? String, "0")
        app.buttons["settings.done"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["connect.usbStatus"].label, "USB: off")
        settings.tap()
        XCTAssertTrue(allowUSB.waitForExistence(timeout: 5))
        allowUSB.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        XCTAssertEqual(allowUSB.value as? String, "1")
        app.buttons["settings.done"].tap()
        let listening = NSPredicate(format: "label == %@", "USB: waiting for a cable")
        expectation(for: listening, evaluatedWith: app.staticTexts["connect.usbStatus"])
        waitForExpectations(timeout: 5)
    }

    func testFrameRatePreference() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-targetFPS", "60", "-allowUSB", "NO"]
        app.launch()
        XCTAssertTrue(app.buttons["settings.button"].waitForExistence(timeout: 10))
        app.buttons["settings.button"].tap()
        let rate = app.buttons["settings.targetFPS"]
        XCTAssertTrue(rate.waitForExistence(timeout: 5))
        XCTAssertEqual(app.switches["settings.allowUSB"].value as? String, "0")
        XCTAssertEqual(rate.value as? String, "60 fps")
        rate.tap()
        app.buttons["120 fps"].tap()
        XCTAssertEqual(rate.value as? String, "120 fps")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "settings-fps120"; shot.lifetime = .keepAlways; add(shot)
        app.terminate()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launch()
        XCTAssertTrue(app.buttons["settings.button"].waitForExistence(timeout: 10))
        app.buttons["settings.button"].tap()
        XCTAssertTrue(rate.waitForExistence(timeout: 5))
        XCTAssertEqual(rate.value as? String, "120 fps")
        rate.tap()
        app.buttons["60 fps"].tap()
        app.buttons["settings.done"].tap()
    }
}
