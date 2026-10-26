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
        // Start from USB on. The setting is saved, so a run that failed
        // between the two taps below left it off for every later run. A
        // launch argument can't set it: it would shadow the toggle.
        if allowUSB.value as? String == "0" {
            allowUSB.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        }
        XCTAssertTrue(switchValue(of: allowUSB, becomes: "1"), "\(allowUSB.value ?? "nil")")
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "settings"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        // SwiftUI exposes the whole form row as a switch. Tap its thumb.
        allowUSB.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        // The switch's value can update a moment after the tap returns.
        XCTAssertTrue(switchValue(of: allowUSB, becomes: "0"), "\(allowUSB.value ?? "nil")")
        app.buttons["settings.done"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["connect.usbStatus"].label, "Off. Turn on to connect with a cable.")
        settings.tap()
        XCTAssertTrue(allowUSB.waitForExistence(timeout: 5))
        allowUSB.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        XCTAssertTrue(switchValue(of: allowUSB, becomes: "1"), "\(allowUSB.value ?? "nil")")
        app.buttons["settings.done"].tap()
        let listening = NSPredicate(format: "label == %@", "Plug this iPad into your PC. Keep Apple Devices open on the PC.")
        expectation(for: listening, evaluatedWith: app.staticTexts["connect.usbStatus"])
        waitForExpectations(timeout: 5)
    }

    /// A flick or drag that starts on the Picture row scrolls Settings and
    /// leaves the choice alone; only choosing from its menu changes it.
    func testScrollingFromThePictureRowKeepsTheChoice() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-pictureFit", "fit"]
        app.launch()
        let settings = app.buttons["settings.button"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        let picture = app.buttons["settings.pictureFit"]
        for gesture in ["drag", "swipe"] {
            settings.tap()
            XCTAssertTrue(picture.waitForExistence(timeout: 5), gesture)
            XCTAssertEqual(picture.value as? String, "Fit", gesture)
            let top = picture.frame.minY
            // Start over the value, where the old segmented control's Fill sat.
            let start = picture.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: 0.5))
            if gesture == "drag" {
                start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: 0, dy: -300)),
                            withVelocity: .slow, thenHoldForDuration: 0.3)
            } else {
                start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: 0, dy: -300)),
                            withVelocity: .fast, thenHoldForDuration: 0)
            }
            XCTAssertTrue(!picture.exists || picture.frame.minY < top - 100, "the \(gesture) did not scroll Settings")
            XCTAssertFalse(app.buttons["Fill"].exists, "the \(gesture) opened the Picture menu")
            app.buttons["settings.done"].tap()
            XCTAssertTrue(settings.waitForExistence(timeout: 5))
            settings.tap()
            XCTAssertTrue(picture.waitForExistence(timeout: 5), gesture)
            XCTAssertEqual(picture.value as? String, "Fit", "a \(gesture) that started on the Picture row changed it")
            app.buttons["settings.done"].tap()
        }
        // The menu itself still works.
        settings.tap()
        XCTAssertTrue(picture.waitForExistence(timeout: 5))
        picture.tap()
        app.buttons["Fill"].tap()
        XCTAssertEqual(picture.value as? String, "Fill")
        picture.tap()
        app.buttons["Fit"].tap()
        XCTAssertEqual(picture.value as? String, "Fit")
        app.buttons["settings.done"].tap()
    }

    /// The pairing code typed into Address is caught at once, instead of
    /// being read as a numeric address that never answers.
    func testPairingCodeAsAddressIsExplainedAtOnce() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-lastHost", "390756", "-lastPort", "9876"]
        app.launch()
        XCTAssertTrue(app.buttons["connect.button"].waitForExistence(timeout: 10))
        app.buttons["connect.button"].tap()
        let title = app.staticTexts["connect.problemTitle"]
        XCTAssertTrue(title.waitForExistence(timeout: 3), app.debugDescription)
        XCTAssertEqual(title.label, "That looks like the pairing code, not the PC's address")
        XCTAssertFalse(app.buttons["Cancel"].exists)
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "pairing-code-as-address"; shot.lifetime = .keepAlways; add(shot)
    }

    /// A search that fails for a reason other than Local Network access
    /// offers Search again, not Settings.
    func testAFailedSearchOffersSearchAgain() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "searchFailed"
        app.launch()
        let title = app.staticTexts["connect.scanProblemTitle"]
        XCTAssertTrue(title.waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertEqual(title.label, "Couldn't search for PCs")
        XCTAssertTrue(app.buttons["Search again"].exists)
        XCTAssertFalse(app.buttons["Open Settings"].exists)
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "search-failed"; shot.lifetime = .keepAlways; add(shot)
        app.buttons["Search again"].tap()
        XCTAssertTrue(title.waitForExistence(timeout: 15))
    }

    /// An address where nothing answers ends in plain words and a retry,
    /// not a transport trace.
    func testUnreachablePCSaysWhatToDo() {
        let app = XCUIApplication()
        // 192.0.2.1 is a documentation address: nothing ever answers.
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-lastHost", "192.0.2.1", "-lastPort", "9876"]
        app.launch()
        XCTAssertTrue(app.buttons["connect.button"].waitForExistence(timeout: 10))
        app.buttons["connect.button"].tap()
        let title = app.staticTexts["connect.problemTitle"]
        XCTAssertTrue(title.waitForExistence(timeout: 20), app.debugDescription)
        XCTAssertEqual(title.label, "Your PC didn't answer")
        let detail = app.staticTexts["connect.problemDetail"].label
        XCTAssertTrue(detail.contains("Ready for your iPad") && detail.contains("Firewall"), detail)
        XCTAssertFalse(detail.contains("HELLO") || detail.contains("datagram"), detail)
        XCTAssertTrue(app.buttons["connect.retry"].exists)
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "pc-did-not-answer"; shot.lifetime = .keepAlways; add(shot)
        app.buttons["connect.retry"].tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 5))
        app.buttons["Cancel"].tap()
    }

    func testDiagnosticsCanBeCopiedAndSharedWithoutAConnection() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launch()
        XCTAssertTrue(app.buttons["settings.button"].waitForExistence(timeout: 10))
        app.buttons["settings.button"].tap()
        let diagnostics = app.buttons["settings.diagnostics"]
        XCTAssertTrue(app.swipeUp(toReveal: diagnostics))
        diagnostics.tap()
        let copy = app.buttons["diagnostics.copy"]
        XCTAssertTrue(copy.waitForExistence(timeout: 5))
        copy.tap()
        XCTAssertTrue(app.buttons["Diagnostics copied"].waitForExistence(timeout: 5))
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "support-diagnostics"; shot.lifetime = .keepAlways; add(shot)
        app.buttons["diagnostics.export"].tap()
        let share = app.navigationBars["UIActivityContentView"]
        // On a cold simulator, Apple's remote share-sheet container can appear
        // before its contents finish loading. Still require the document and
        // export actions once the remote interface is ready.
        XCTAssertTrue(share.waitForExistence(timeout: 30), app.debugDescription)
        XCTAssertTrue(share.otherElements["EternalMonitor-iPad-diagnostics"].waitForExistence(timeout: 5))
        let documentType = share.otherElements.matching(NSPredicate(format: "label BEGINSWITH %@", "Text Document")).firstMatch
        XCTAssertTrue(documentType.waitForExistence(timeout: 5))
        let save = app.cells["Save to Files"]
        XCTAssertTrue(save.waitForExistence(timeout: 5), app.debugDescription)
        XCTAssertTrue(save.isEnabled)
        // Verify our exported attachment and share actions. The Files provider
        // can remain uninitialized on a fresh simulator after its picker opens.
        app.buttons["header.closeButton"].tap()
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: share)
        waitForExpectations(timeout: 5)
        XCTAssertTrue(app.buttons["diagnostics.export"].isEnabled)
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

    /// Whether `element`'s value becomes `value` within five seconds.
    private func switchValue(of element: XCUIElement, becomes value: String) -> Bool {
        let changed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", value), object: element)
        return XCTWaiter().wait(for: [changed], timeout: 5) == .completed
    }
}
