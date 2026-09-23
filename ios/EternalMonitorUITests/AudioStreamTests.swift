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
        XCTAssertTrue(control("display.disconnect", in: app).exists)
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
        let host = app.textFields["connect.host"]
        for _ in 0..<2 where !host.exists {
            control("display.disconnect", in: app).tap()
            _ = host.waitForExistence(timeout: 5)
        }
        XCTAssertTrue(host.exists)
        app.terminate()
    }

    // Display controls fade five seconds after they appear, sooner than a
    // busy runner may finish waiting for audio. Bring them back with the
    // user's three-finger gesture instead of racing the fade.
    private func revealControls(_ app: XCUIApplication, showing element: XCUIElement) {
        if !(element.exists && element.isHittable) {
            app.tap(withNumberOfTaps: 1, numberOfTouches: 3)
        }
    }

    private func control(_ identifier: String, in app: XCUIApplication) -> XCUIElement {
        let control = app.buttons[identifier]
        revealControls(app, showing: control)
        XCTAssertTrue(control.waitForExistence(timeout: 5))
        return control
    }

    private func waitForHUD(_ app: XCUIApplication, containing text: String) {
        let hud = app.buttons["display.hud"]
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            revealControls(app, showing: hud)
            if hud.waitForExistence(timeout: 2), hud.label.contains(text) { return }
        }
        XCTFail("HUD never showed \"\(text)\": \(hud.exists ? hud.label : "hidden")")
    }

    // A tap can still land just after the controls fade on a slow runner.
    private func openSettings(_ app: XCUIApplication) {
        let settings = app.navigationBars["Settings"]
        for _ in 0..<2 where !settings.exists {
            control("display.settings", in: app).tap()
            _ = settings.waitForExistence(timeout: 5)
        }
        XCTAssertTrue(settings.exists)
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
