import XCTest

/// First-run setup and the in-app language choice.
final class SetupTests: UITestCase {
    /// A fresh install: nothing saved, nothing paired, no recent PC. The
    /// language follows the device (English on CI's simulator).
    private let freshInstall = ["-didSeeOnboarding", "NO", "-lastHost", "", "-recentConnections", "",
                                "-appLanguage", "", "-allowUSB", "NO"]

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func capture(_ name: String, _ app: XCUIApplication) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    /// Setup opens on a fresh install, a language applies at once to the
    /// screen already showing, every step can be skipped, and setup is not
    /// shown again.
    func testFreshInstallShowsSetupAndLanguageAppliesAtOnce() {
        let app = XCUIApplication()
        forgetPairings(app)
        app.launchArguments = freshInstall
        launchCleanly(app)
        let title = app.staticTexts["setup.step.language"]
        XCTAssertTrue(title.waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertEqual(title.label, "Language")
        // The connect screen is under the cover, not in front of it.
        XCTAssertFalse(app.textFields["connect.host"].isHittable, "the connect screen showed before setup")
        let next = app.buttons["setup.next"]

        app.buttons["setup.language.de"].tap()
        XCTAssertTrue(waitForLabel(title, "Sprache"), title.label)
        XCTAssertEqual(next.label, "Weiter")
        capture("setup-language-de", app)
        app.buttons["setup.language.ar"].tap()
        XCTAssertTrue(waitForLabel(title, "اللغة"), title.label)
        // Right to left: Skip setup moves to the left edge.
        let skip = app.buttons["setup.skip"]
        XCTAssertLessThan(skip.frame.midX, app.frame.midX, "Arabic did not mirror the layout")
        capture("setup-language-ar", app)
        app.buttons["setup.language.system"].tap()
        XCTAssertTrue(waitForLabel(title, "Language"), title.label)
        XCTAssertGreaterThan(skip.frame.midX, app.frame.midX)

        next.tap()
        let network = app.staticTexts["setup.step.network"]
        XCTAssertTrue(network.waitForExistence(timeout: 5))
        XCTAssertEqual(network.label, "Local network")
        app.buttons["setup.network.skip"].tap()
        XCTAssertTrue(app.staticTexts["setup.step.connect"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["setup.connect.qr"].exists)
        XCTAssertTrue(app.buttons["setup.connect.usb"].exists)
        app.buttons["setup.done"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 10))

        // Finished, so the next launch goes straight to the connect screen.
        app.launchArguments = ["-lastHost", "", "-recentConnections", "", "-appLanguage", "", "-allowUSB", "NO"]
        launchCleanly(app)
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.staticTexts["setup.step.language"].exists)
    }

    /// Someone updating with a saved PC never sees setup.
    func testExistingUserSkipsSetup() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "NO", "-lastHost", "192.0.2.1", "-lastPort", "9876",
                               "-appLanguage", "", "-allowUSB", "NO"]
        launchCleanly(app)
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 15))
        XCTAssertFalse(app.staticTexts["setup.step.language"].exists)
    }

    /// Local Network turned off: the step says how to turn it on and opens
    /// the app's Settings page. Choosing Scan QR code opens the scanner.
    func testBlockedLocalNetworkOffersSettings() {
        let app = XCUIApplication()
        app.launchArguments = freshInstall
        app.launchEnvironment["EM_UI_PREVIEW"] = "setup-blocked"
        launchCleanly(app)
        XCTAssertTrue(app.buttons["setup.next"].waitForExistence(timeout: 15))
        app.buttons["setup.next"].tap()
        let allow = app.buttons["setup.network.allow"]
        XCTAssertTrue(allow.waitForExistence(timeout: 5))
        allow.tap()
        XCTAssertTrue(app.staticTexts["setup.network.blocked"].waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertTrue(app.buttons["setup.network.openSettings"].isHittable)
        capture("setup-network-blocked", app)
        // Scan QR code ends setup and opens the scanner.
        app.buttons["setup.next"].tap()
        let qr = app.buttons["setup.connect.qr"]
        XCTAssertTrue(qr.waitForExistence(timeout: 5))
        qr.tap()
        XCTAssertTrue(app.buttons["Cancel"].waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertFalse(app.staticTexts["setup.step.connect"].exists)
    }

    /// Settings switches the language for the screen it is on and the one
    /// under it, without a relaunch, and Run setup again opens setup.
    func testSettingsLanguageAppliesAtOnceAndRunsSetupAgain() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-appLanguage", ""]
        launchCleanly(app)
        let settings = app.buttons["settings.button"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        settings.tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        let picker = app.buttons["settings.language"]
        XCTAssertTrue(app.swipeUp(toReveal: picker))
        picker.tap()
        app.buttons["日本語"].tap()
        XCTAssertTrue(app.navigationBars["設定"].waitForExistence(timeout: 5), app.debugDescription)
        XCTAssertEqual(app.buttons["settings.done"].label, "完了")
        capture("settings-ja", app)
        app.buttons["settings.done"].tap()
        let connect = app.buttons["connect.button"]
        XCTAssertTrue(connect.waitForExistence(timeout: 10))
        XCTAssertEqual(connect.label, "PC に接続")

        settings.tap()
        XCTAssertTrue(picker.waitForExistence(timeout: 5) || app.swipeUp(toReveal: picker))
        XCTAssertTrue(app.swipeUp(toReveal: picker))
        picker.tap()
        app.buttons["システムの言語"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5), app.debugDescription)
        let runSetup = app.buttons["settings.runSetup"]
        XCTAssertTrue(app.swipeUp(toReveal: runSetup))
        runSetup.tap()
        XCTAssertTrue(app.staticTexts["setup.step.language"].waitForExistence(timeout: 10), app.debugDescription)
        app.buttons["setup.skip"].tap()
        XCTAssertTrue(app.buttons["connect.button"].waitForExistence(timeout: 10))
        XCTAssertEqual(app.buttons["connect.button"].label, "Connect to PC")
    }

    /// Pairings live in the Keychain, which outlives the app, and the unit
    /// tests' fake PC leaves one there. Forget them the way a person would.
    private func forgetPairings(_ app: XCUIApplication) {
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-appLanguage", ""]
        launchCleanly(app)
        let settings = app.buttons["settings.button"]
        XCTAssertTrue(settings.waitForExistence(timeout: 10))
        settings.tap()
        let forget = app.buttons["settings.forgetPairings"]
        XCTAssertTrue(app.swipeUp(toReveal: forget))
        forget.tap()
        app.buttons["settings.done"].tap()
    }

    private func waitForLabel(_ element: XCUIElement, _ label: String) -> Bool {
        let deadline = Date().addingTimeInterval(5)
        while Date() < deadline {
            if element.exists && element.label == label { return true }
            Thread.sleep(forTimeInterval: 0.1)
        }
        return false
    }
}
