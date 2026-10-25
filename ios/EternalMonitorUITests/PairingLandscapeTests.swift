import XCTest

/// On a phone in landscape, or a short one, the number pad covers the sheet's
/// Pair button, so the navigation bar offers Pair too. iPad never shows it.
final class PairingLandscapeTests: XCTestCase {
    override func tearDown() {
        rotate(.portrait)
    }

    /// XCUIDevice rotates every simulator but the iPhone Duo, which follows
    /// only Device Hub's orientation picker.
    private func rotate(_ orientation: UIDeviceOrientation, file: StaticString = #filePath, line: UInt = #line) {
        XCUIDevice.shared.orientation = orientation
        if EMHingeAvailable() {
            var error: NSString?
            XCTAssertTrue(EMSetDeviceOrientation(orientation == .portrait ? "portrait" : "landscape-left", &error),
                          "Device Hub's orientation event failed: \(error as String? ?? "")", file: file, line: line)
        }
    }

    /// A rotation that did nothing would test the other orientation's
    /// layout, so the app's window must have turned before any check.
    private func assertTurned(_ app: XCUIApplication, to orientation: UIDeviceOrientation,
                              file: StaticString = #filePath, line: UInt = #line) {
        let window = app.windows.firstMatch
        let turned = NSPredicate { _, _ in
            let size = window.frame.size
            return size.width > 0 && (size.width > size.height) == orientation.isLandscape
        }
        let result = XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: turned, object: nil)], timeout: 5)
        XCTAssertEqual(result, .completed, "Rotating to \(orientation.isLandscape ? "landscape" : "portrait") did nothing: "
                       + "the window is \(window.frame.size), the device \(XCUIDevice.shared.orientation.rawValue)",
                       file: file, line: line)
    }

    func testPairStaysReachableAboveTheNumberPad() throws {
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom != .phone, "iPhone only")
        continueAfterFailure = false
        rotate(.landscapeLeft)
        let app = XCUIApplication()
        app.launchArguments = ["-allowUSB", "NO", "-didSeeOnboarding", "YES"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "pairing"
        app.launch()
        defer { app.terminate() }
        assertTurned(app, to: .landscapeLeft)
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 10))
        app.typeText("123456")
        let bar = app.buttons["pairing.submitBar"]
        XCTAssertTrue(bar.waitForExistence(timeout: 5))
        XCTAssertTrue(bar.isHittable && bar.isEnabled)
        XCTAssertLessThan(bar.frame.maxY, app.keyboards.firstMatch.frame.minY)

        rotate(.portrait)
        assertTurned(app, to: .portrait)
        let submit = app.buttons["pairing.submit"]
        XCTAssertTrue(submit.waitForExistence(timeout: 5))
        // The bar goes, unless the pad still covers the sheet's Pair, as on
        // a short phone such as the folded iPhone Duo.
        let settled = NSPredicate { _, _ in
            let top = app.keyboards.firstMatch.frame.minY
            return submit.frame.maxY > top ? bar.exists && bar.frame.maxY < top : !bar.exists && submit.isHittable
        }
        expectation(for: settled, evaluatedWith: nil)
        waitForExpectations(timeout: 5)
    }

    /// The bar says why Pair can't be tapped, and an error shows above the
    /// field, where the number pad doesn't cover it.
    func testErrorsAndWaitingStayAboveTheNumberPad() throws {
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom != .phone, "iPhone only")
        for state in ["pairing-wrong-code", "pairing-rate-limited", "pairing-submitting"] {
            rotate(.landscapeLeft)
            let app = XCUIApplication()
            app.launchArguments = ["-allowUSB", "NO", "-didSeeOnboarding", "YES"]
            app.launchEnvironment["EM_UI_PREVIEW"] = state
            app.launch()
            assertTurned(app, to: .landscapeLeft)
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
