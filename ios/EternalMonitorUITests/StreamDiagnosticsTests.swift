import XCTest

/// scripts/test_ios.sh starts the real synthetic host on this dedicated port.
final class StreamDiagnosticsTests: XCTestCase {
    func testQualityPopoverShowsRepairsAndJitter() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES"]
        app.launchEnvironment["EM_AUTOCONNECT"] = "127.0.0.1:19875"
        app.launchEnvironment["EM_UDP_BACKEND"] = "bsd"
        app.launch()
        let hud = app.buttons["display.hud"]
        if !hud.waitForExistence(timeout: 2) {
            // The normal HUD fades after five seconds. Reveal it with the
            // same three-finger gesture a user makes during input relay.
            app.tap(withNumberOfTaps: 1, numberOfTouches: 3)
        }
        XCTAssertTrue(hud.waitForExistence(timeout: 5), app.debugDescription)
        hud.tap()
        let repaired = app.descendants(matching: .any)["quality.repaired"].firstMatch
        let jitter = app.descendants(matching: .any)["quality.jitter"].firstMatch
        XCTAssertTrue(repaired.waitForExistence(timeout: 5))
        XCTAssertTrue(jitter.exists)
        let hasRepairs = NSPredicate { _, _ in
            Int(repaired.value as? String ?? "") ?? 0 > 0
        }
        expectation(for: hasRepairs, evaluatedWith: repaired)
        waitForExpectations(timeout: 10)
        XCTAssertTrue((jitter.value as? String ?? "").contains("milliseconds"))
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "stream-quality-repairs"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        app.terminate()
    }
}
