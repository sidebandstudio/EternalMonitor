import Network
import XCTest

/// The fixture changes cable availability; all handshake and video traffic
/// comes from the real synthetic host started by scripts/test_ios.sh.
final class USBStreamTests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
        cable("STOP")
    }

    override func tearDownWithError() throws {
        XCUIApplication().terminate()
        cable("STOP")
    }

    func testUSBAutoconnectAndManualDisconnect() {
        let app = launchApp()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["connect.usbStatus"].exists)
        cable("START")
        assertTransport("USB", app: app)
        capture("usb-connected", app: app)
        app.buttons["display.disconnect"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["connect.usb"].exists)
        XCTAssertTrue(app.staticTexts["connect.usbStatus"].label.contains("disconnected"))
        capture("usb-disconnected", app: app)
    }

    func testUSBTakesOverWiFiAndUnplugFallsBack() {
        let app = launchApp(autoconnect: "127.0.0.1:19877")
        assertTransport("WiFi", app: app)
        cable("START")
        assertTransport("USB", app: app)
        capture("usb-takeover", app: app)
        cable("STOP")
        assertTransport("WiFi", app: app, timeout: 5)
        capture("usb-unplug-wifi", app: app)
        cable("START")
        assertTransport("USB", app: app)
        capture("usb-replug", app: app)
    }

    private func launchApp(autoconnect: String = "") -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "YES", "-autoReconnect", "YES"]
        app.launchEnvironment["EM_AUTOCONNECT"] = autoconnect
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        app.launch()
        return app
    }

    private func assertTransport(_ name: String, app: XCUIApplication, timeout: TimeInterval = 10) {
        let hud = app.buttons["display.hud"]
        // A cable can arrive during the host's retry backoff. Wait for the
        // display before interacting; a premature gesture can hide its HUD.
        XCTAssertTrue(hud.waitForExistence(timeout: timeout), app.debugDescription)
        let matches = NSPredicate(format: "exists == true AND label CONTAINS %@", name)
        expectation(for: matches, evaluatedWith: hud)
        waitForExpectations(timeout: timeout)
    }

    private func cable(_ command: String) {
        let done = expectation(description: "USB fixture \(command)")
        let connection = NWConnection(host: "127.0.0.1", port: 19874, using: .tcp)
        connection.stateUpdateHandler = { state in
            if case .ready = state {
                connection.send(content: Data("\(command)\n".utf8), completion: .contentProcessed { error in
                    XCTAssertNil(error)
                    connection.receive(minimumIncompleteLength: 3, maximumLength: 16) { data, _, _, error in
                        XCTAssertNil(error)
                        XCTAssertEqual(data, Data("OK\n".utf8))
                        done.fulfill()
                    }
                })
            }
        }
        connection.start(queue: DispatchQueue(label: "usb.ui.fixture"))
        wait(for: [done], timeout: 5)
        connection.cancel()
        connection.stateUpdateHandler = nil
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = name
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }
}
