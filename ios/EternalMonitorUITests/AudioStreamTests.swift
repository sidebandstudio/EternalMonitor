import XCTest

final class AudioStreamTests: XCTestCase {
    func testAudioStatusAndLiveMuteKeepVideoConnected() {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "NO", "-playPCaudio", "YES"]
        app.launchEnvironment["EM_AUTOCONNECT"] = "127.0.0.1:19875"
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        app.launch()
        let hud = app.buttons["display.hud"]
        XCTAssertTrue(hud.waitForExistence(timeout: 10))
        waitForHUD(app, containing: "PC audio playing")
        openSettings(app)
        let toggle = app.switches["settings.playPCaudio"]
        XCTAssertTrue(toggle.exists)
        XCTAssertEqual(toggle.value as? String, "1")
        pressSwitch(toggle)
        waitForValue(toggle, value: "0")
        app.buttons["settings.done"].tap()
        waitForHUD(app, containing: "PC audio muted or unavailable")
        let disconnect = app.buttons["display.disconnect"]
        revealControls(app, showing: disconnect)
        XCTAssertTrue(appears(disconnect, within: 5))
        capture("audio-muted-hud", app: app)
        openSettings(app)
        XCTAssertTrue(toggle.waitForExistence(timeout: 5))
        pressSwitch(toggle)
        waitForValue(toggle, value: "1")
        app.swipeUp()
        let audio = app.descendants(matching: .any)["settings.hostAudio"].firstMatch
        XCTAssertTrue(audio.waitForExistence(timeout: 5))
        let activeAudio = NSPredicate(format: "value CONTAINS %@", "Opus 128 kbps, buffer")
        expectation(for: activeAudio, evaluatedWith: audio)
        waitForExpectations(timeout: 5)
        capture("audio-host-stream", app: app)
        app.buttons["settings.done"].tap()
        waitForHUD(app, containing: "PC audio playing")
        capture("audio-playing-hud", app: app)
        tapControl("display.disconnect", in: app, until: app.textFields["connect.host"])
        app.terminate()
    }

    // Display controls fade five seconds after they appear. A busy runner can
    // outlast that between finding a control and touching it, and a touch on
    // a vanished element fails the test outright. Bring the controls back
    // with the user's three-finger gesture, touch the control's position, and
    // check the outcome before continuing.
    private func revealControls(_ app: XCUIApplication, showing element: XCUIElement) {
        if !element.exists { app.tap(withNumberOfTaps: 1, numberOfTouches: 3) }
    }

    private func appears(_ element: XCUIElement, where format: String = "exists == true",
                         _ arguments: CVarArg..., within timeout: TimeInterval) -> Bool {
        let predicate = NSPredicate(format: format, argumentArray: arguments)
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: element)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }

    private func tapControl(_ identifier: String, in app: XCUIApplication, until result: XCUIElement) {
        let control = app.buttons[identifier]
        for _ in 0..<3 where !result.exists {
            revealControls(app, showing: control)
            guard appears(control, within: 5) else { continue }
            let center = control.frame
            app.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: center.midX, dy: center.midY)).tap()
            _ = appears(result, within: 5)
        }
        XCTAssertTrue(result.exists, "\(identifier) did not open \(result)")
    }

    private func openSettings(_ app: XCUIApplication) {
        tapControl("display.settings", in: app, until: app.navigationBars["Settings"])
    }

    private func waitForHUD(_ app: XCUIApplication, containing text: String) {
        let hud = app.buttons["display.hud"]
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            revealControls(app, showing: hud)
            if appears(hud, where: "exists == true AND label CONTAINS %@", text, within: 2) { return }
        }
        XCTFail("HUD never showed \"\(text)\"")
    }

    private func waitForValue(_ element: XCUIElement, value: String) {
        expectation(for: NSPredicate(format: "value == %@", value), evaluatedWith: element)
        waitForExpectations(timeout: 5)
    }

    private func pressSwitch(_ element: XCUIElement) {
        expectation(for: NSPredicate(format: "hittable == true"), evaluatedWith: element)
        waitForExpectations(timeout: 5)
        // The accessible switch covers the whole Form row. Hit the actual
        // control and keep contact across more than one busy simulator frame.
        element.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5))
            .press(forDuration: 0.15)
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
