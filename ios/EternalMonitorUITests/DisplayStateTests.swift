import XCTest

/// Stream states that need no PC, shown through the debug UI preview.
final class DisplayStateTests: XCTestCase {
    func testHeldPictureSaysItIsBeingRestored() {
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO"]
        app.launchEnvironment["EM_UI_PREVIEW"] = "restoring"
        app.launch()
        defer { app.terminate() }
        let note = app.otherElements["display.restoringPicture"]
        XCTAssertTrue(note.waitForExistence(timeout: 10), "a held picture must not look like a live one")
        XCTAssertEqual(note.label, "Restoring the picture")
        // The note is not a card over the controls: Disconnect still works.
        XCTAssertTrue(app.buttons["display.disconnect"].isHittable)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "restoring-picture"
        screenshot.lifetime = .keepAlways
        add(screenshot)
    }
}
