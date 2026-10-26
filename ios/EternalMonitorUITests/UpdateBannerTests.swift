import XCTest

/// Debug builds read `EM_UPDATE_MANIFEST` instead of the website, so these
/// tests never touch the network.
final class UpdateBannerTests: XCTestCase {
    private let manifest = #"{"schema":1,"ipad":{"version":"9.9.9","build":999,"testflight":"https://testflight.apple.com/join/ppc9QfXc"}}"#

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func launch(manifest: String?, resetDismissal: Bool = true, keepPopup: Bool = false) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-checkForUpdates", "YES"]
        if resetDismissal { app.launchArguments += ["-updateDismissedBuild", "0", "-updatePopupClosedBuild", "0"] }
        if let manifest { app.launchEnvironment["EM_UPDATE_MANIFEST"] = manifest }
        app.launch()
        // A new build opens the popup first; the banner tests close it.
        if resetDismissal, !keepPopup, manifest?.contains(#""build":999"#) == true {
            let later = app.buttons["updatePopup.later"]
            XCTAssertTrue(later.waitForExistence(timeout: 10))
            later.tap()
            XCTAssertFalse(later.waitForExistence(timeout: 1))
        }
        return app
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = name
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    func testBannerAndSettingsRowOpenTestFlight() {
        let app = launch(manifest: manifest)
        let banner = app.otherElements["update.banner"]
        XCTAssertTrue(banner.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["A new version is ready in TestFlight"].exists)
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Version 9.9.9, build 999")).firstMatch.exists)
        XCTAssertTrue(app.buttons["update.openTestFlight"].exists)
        attach(app, "update-banner")

        app.buttons["settings.button"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        let row = app.buttons["settings.openTestFlight"]
        for _ in 0..<8 where !row.isHittable {
            app.swipeUp()
        }
        XCTAssertTrue(row.isHittable)
        XCTAssertTrue(app.switches["settings.checkForUpdates"].exists)
        attach(app, "update-settings")

        // Turning automatic checks off hides the update everywhere.
        app.switches["settings.checkForUpdates"].coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        XCTAssertFalse(row.waitForExistence(timeout: 1))
        app.buttons["settings.done"].tap()
        XCTAssertFalse(banner.waitForExistence(timeout: 2))
    }

    /// One thing at a time: the popup waits for the feedback ask, then for
    /// the feedback sheet the ask opens, and shows once that closes.
    func testFeedbackAskAndSheetComeBeforeThePopup() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-checkForUpdates", "YES",
                               "-updateDismissedBuild", "0", "-updatePopupClosedBuild", "0",
                               "-feedbackLongSessions", "3", "-feedbackAskDone", "NO"]
        app.launchEnvironment["EM_UPDATE_MANIFEST"] = manifest
        app.launch()
        let send = app.buttons["feedback.askSend"]
        XCTAssertTrue(send.waitForExistence(timeout: 10))
        let later = app.buttons["updatePopup.later"]
        XCTAssertFalse(later.waitForExistence(timeout: 3), "not next to the feedback ask")
        send.tap()
        let done = app.buttons["feedback.done"]
        XCTAssertTrue(done.waitForExistence(timeout: 15))
        XCTAssertFalse(later.waitForExistence(timeout: 2), "not over the feedback sheet")
        attach(app, "feedback-sheet-without-popup")
        done.tap()
        XCTAssertTrue(later.waitForExistence(timeout: 10), "shows once the sheet closes")
        later.tap()
    }

    /// Between Send feedback and its sheet (Mail collects diagnostics first)
    /// the ask is gone and the sheet isn't up yet: the popup still waits.
    func testFeedbackPreparationKeepsThePopupAway() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-checkForUpdates", "YES",
                               "-updateDismissedBuild", "0", "-updatePopupClosedBuild", "0",
                               "-feedbackLongSessions", "3", "-feedbackAskDone", "NO"]
        app.launchEnvironment["EM_UPDATE_MANIFEST"] = manifest
        app.launchEnvironment["EM_FEEDBACK_PREPARE_MS"] = "4000"
        app.launch()
        let send = app.buttons["feedback.askSend"]
        XCTAssertTrue(send.waitForExistence(timeout: 10))
        send.tap()
        let later = app.buttons["updatePopup.later"]
        let done = app.buttons["feedback.done"]
        XCTAssertFalse(later.waitForExistence(timeout: 3), "not while the feedback sheet is on its way")
        XCTAssertTrue(done.waitForExistence(timeout: 15))
        XCTAssertFalse(later.exists, "not over the feedback sheet")
        done.tap()
        XCTAssertTrue(later.waitForExistence(timeout: 10), "shows once the sheet closes")
        later.tap()
    }

    /// The feedback sheet (Mail, or the copy sheet without Mail) on its own.
    func testFeedbackSheetComesBeforeThePopup() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-checkForUpdates", "YES",
                               "-updateDismissedBuild", "0", "-updatePopupClosedBuild", "0", "-feedbackAskDone", "YES"]
        app.launchEnvironment["EM_UPDATE_MANIFEST"] = manifest
        app.launchEnvironment["EM_UI_PREVIEW"] = "feedback"
        app.launch()
        let done = app.buttons["feedback.done"]
        XCTAssertTrue(done.waitForExistence(timeout: 15))
        let later = app.buttons["updatePopup.later"]
        XCTAssertFalse(later.waitForExistence(timeout: 3), "not over the feedback sheet")
        done.tap()
        XCTAssertTrue(later.waitForExistence(timeout: 10), "shows once the sheet closes")
        later.tap()
    }

    func testFirstLaunchShowsThePopupOnce() {
        let withNote = #"{"schema":1,"ipad":{"version":"9.9.9","build":999,"note":"Fixes Apple Pencil pressure in drawing apps."}}"#
        let app = launch(manifest: withNote, keepPopup: true)
        let later = app.buttons["updatePopup.later"]
        XCTAssertTrue(later.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["A new version of EternalMonitor is ready"].exists)
        XCTAssertTrue(app.staticTexts["Updating is highly recommended."].exists)
        XCTAssertTrue(app.staticTexts["Fixes Apple Pencil pressure in drawing apps."].exists)
        XCTAssertTrue(app.buttons["updatePopup.openTestFlight"].exists)
        attach(app, "update-popup")
        later.tap()
        XCTAssertFalse(later.waitForExistence(timeout: 2))
        XCTAssertTrue(app.otherElements["update.banner"].exists, "the banner stays")

        // Closed for three days, also after relaunch.
        app.terminate()
        let relaunched = launch(manifest: withNote, resetDismissal: false)
        XCTAssertTrue(relaunched.buttons["settings.button"].waitForExistence(timeout: 10))
        XCTAssertFalse(relaunched.buttons["updatePopup.later"].waitForExistence(timeout: 5))
    }

    func testDismissHidesTheBanner() {
        let app = launch(manifest: manifest)
        let banner = app.otherElements["update.banner"]
        XCTAssertTrue(banner.waitForExistence(timeout: 10))
        app.buttons["update.dismiss"].tap()
        XCTAssertFalse(banner.waitForExistence(timeout: 2))
        app.terminate()
        let relaunched = launch(manifest: manifest, resetDismissal: false)
        XCTAssertTrue(relaunched.buttons["settings.button"].waitForExistence(timeout: 10))
        XCTAssertFalse(relaunched.otherElements["update.banner"].waitForExistence(timeout: 3),
                       "dismissed for a day, also after relaunch")
    }

    func testNoBannerWithoutAnUpdate() {
        // An older build in the manifest also clears what an earlier test stored.
        let app = launch(manifest: #"{"schema":1,"ipad":{"version":"0.0.1","build":1}}"#)
        XCTAssertTrue(app.buttons["settings.button"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.otherElements["update.banner"].exists)
    }

    func testMismatchOffersTestFlightOnTheIPad() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "mismatch"
        app.launch()
        XCTAssertTrue(app.buttons["mismatch.openTestFlight"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Update the iPad app: open TestFlight")).firstMatch.exists)
        attach(app, "version-mismatch")
    }
}
