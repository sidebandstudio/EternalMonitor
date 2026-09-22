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
        waitForLabel(hud, containing: "PC audio playing")
        app.buttons["display.settings"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5))
        let toggle = app.switches["settings.playPCaudio"]
        XCTAssertTrue(toggle.exists)
        XCTAssertEqual(toggle.value as? String, "1")
        toggle.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        XCTAssertEqual(toggle.value as? String, "0")
        app.buttons["settings.done"].tap()
        waitForLabel(hud, containing: "PC audio muted or unavailable")
        XCTAssertTrue(app.buttons["display.disconnect"].exists)
        capture("audio-muted-hud", app: app)
        app.buttons["display.settings"].tap()
        XCTAssertTrue(toggle.waitForExistence(timeout: 5))
        toggle.coordinate(withNormalizedOffset: CGVector(dx: 0.93, dy: 0.5)).tap()
        XCTAssertEqual(toggle.value as? String, "1")
        app.swipeUp()
        let audio = app.descendants(matching: .any)["settings.hostAudio"].firstMatch
        XCTAssertTrue(audio.waitForExistence(timeout: 5))
        let activeAudio = NSPredicate(format: "value CONTAINS %@", "Opus 128 kbps, buffer")
        expectation(for: activeAudio, evaluatedWith: audio)
        waitForExpectations(timeout: 5)
        capture("audio-host-stream", app: app)
        app.buttons["settings.done"].tap()
        waitForLabel(hud, containing: "PC audio playing")
        capture("audio-playing-hud", app: app)
        app.buttons["display.disconnect"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
        app.terminate()
    }

    private func waitForLabel(_ element: XCUIElement, containing text: String) {
        expectation(for: NSPredicate(format: "label CONTAINS %@", text), evaluatedWith: element)
        waitForExpectations(timeout: 10)
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
