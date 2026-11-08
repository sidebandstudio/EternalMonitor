import XCTest

/// Stream states that need no PC, shown through the debug UI preview.
final class DisplayStateTests: UITestCase {
    func testHeldPictureSaysItIsBeingRestored() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "restoring"
        launchCleanly(app)
        let note = app.otherElements["display.restoringPicture"]
        XCTAssertTrue(note.waitForExistence(timeout: 10), "a held picture must not look like a live one")
        XCTAssertEqual(note.label, "Restoring the picture")
        // The note is not a card over the controls: Disconnect still works.
        // The HUD hides five seconds after it appears, which a starved
        // runner can spend launching; a tap on the picture brings it back.
        let disconnect = app.buttons["display.disconnect"]
        var hittable = false
        for _ in 0..<3 where !hittable {
            if !disconnect.exists { app.revealHUD(.tap) }
            let check = XCTNSPredicateExpectation(predicate: NSPredicate(format: "hittable == true"), object: disconnect)
            hittable = XCTWaiter().wait(for: [check], timeout: 5) == .completed
        }
        XCTAssertTrue(hittable)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "restoring-picture"
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }

    func testALockedPCSaysSoOverTheHeldPicture() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "locked"
        launchCleanly(app)
        let note = app.otherElements["display.pcLocked"]
        XCTAssertTrue(note.waitForExistence(timeout: 10), "a locked PC must not look like a frozen picture")
        XCTAssertEqual(note.label, "Your PC is locked. Unlock it to continue.")
        XCTAssertFalse(app.otherElements["display.restoringPicture"].exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "pc-locked"
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }
}
