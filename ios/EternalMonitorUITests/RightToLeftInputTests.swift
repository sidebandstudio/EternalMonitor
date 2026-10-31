import XCTest

/// In Arabic the app's own controls mirror, but the PC picture, touch
/// input and the keyboard bar do not: a tap on the left of the iPad clicks
/// the left of the PC.
final class RightToLeftInputTests: UITestCase {
    /// Arabic chosen in the app on an English iPad. A switch made while the
    /// keyboard bar exists is covered by KeyboardBarLanguageTests.
    func testArabicMirrorsControlsButNotInput() throws {
        try checkArabic(["-appLanguage", "ar"], shot: "arabic-stream")
    }

    /// An iPad set to Arabic, the app following it. UIKit then runs right
    /// to left as well, outside the SwiftUI screens.
    func testSystemArabicMirrorsControlsButNotInput() throws {
        try checkArabic(["-AppleLanguages", "(ar)", "-appLanguage", ""],
                        shot: "system-arabic-stream")
    }

    private func checkArabic(_ language: [String], shot name: String) throws {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["EM_INPUT_HOST_LOG"])
        let before = (try? Data(contentsOf: URL(fileURLWithPath: path)).count) ?? 0
        let width = Double(environment["EM_INPUT_WIDTH"] ?? "640") ?? 640
        let height = Double(environment["EM_INPUT_HEIGHT"] ?? "360") ?? 360
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES",
                               "-playPCaudio", "NO", "-pictureFit", "fit"] + language
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        launchCleanly(app)

        // The controls are Arabic and mirrored: Disconnect, last in the bar,
        // sits at its left end.
        let disconnect = app.buttons["display.disconnect"]
        let keyboard = app.buttons["display.keyboard"]
        XCTAssertTrue(keyboard.waitForExistence(timeout: 15))
        XCTAssertEqual(disconnect.label, "قطع الاتصال")
        XCTAssertLessThan(disconnect.frame.midX, keyboard.frame.midX, "the controls did not mirror")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
        // Taps go to the picture once the controls hide.
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: keyboard)
        waitForExpectations(timeout: 8)

        let relay = app.otherElements["display.relay"]
        XCTAssertTrue(relay.waitForExistence(timeout: 5))
        let frame = relay.frame
        let scale = min(frame.width / width, frame.height / height)
        let video = CGRect(x: frame.midX - width * scale / 2, y: frame.midY - height * scale / 2,
                           width: width * scale, height: height * scale)
        for fraction in [0.2, 0.8] {
            app.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: video.minX + fraction * video.width, dy: video.midY))
                .tap()
        }

        // The PC moved the pointer to the left fifth, then the right fifth,
        // of its 0...65535 screen space.
        var moves: [Int] = []
        let pattern = try NSRegularExpression(pattern: #"MoveAbs \{ x: (\d+), y: \d+ \}"#)
        let clicked = NSPredicate { _, _ in
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)), data.count >= before else { return false }
            let log = String(decoding: data.dropFirst(before), as: UTF8.self)
            moves = pattern.matches(in: log, range: NSRange(log.startIndex..., in: log)).compactMap {
                Range($0.range(at: 1), in: log).flatMap { Int(log[$0]) }
            }
            return moves.count >= 2 && log.contains("LeftUp")
        }
        expectation(for: clicked, evaluatedWith: nil)
        waitForExpectations(timeout: 15)
        XCTAssertEqual(Double(moves[0]) / 65535, 0.2, accuracy: 0.05, "first tap: \(moves)")
        XCTAssertEqual(Double(moves[moves.count - 1]) / 65535, 0.8, accuracy: 0.05, "second tap: \(moves)")

        // The keyboard bar keeps a PC keyboard's order, Done at its right
        // end, with Done and the toggles' values in Arabic.
        let done = app.buttons["keyboard.done"]
        app.tapHUDControl("display.keyboard", until: done, reveal: .threeFingersTogether)
        let arrows = [0x50, 0x52, 0x51, 0x4f].map { app.buttons["keyboard.key.\($0)"].frame.midX }
        XCTAssertTrue(zip(arrows, arrows.dropFirst()).allSatisfy { $0 < $1 },
                      "the arrow keys are not ← ↑ ↓ → from the left: \(arrows)")
        XCTAssertLessThan(app.buttons["keyboard.key.41"].frame.midX, arrows[0], "Esc is not left of the arrows")
        XCTAssertGreaterThan(done.frame.midX, app.buttons["keyboard.key.76"].frame.maxX, "Done is not at the right end")
        XCTAssertEqual(done.label, "تم")
        let control = app.buttons["keyboard.key.224"]
        control.tap()
        XCTAssertEqual(control.value as? String, "قيد التشغيل")
        control.tap()
        XCTAssertEqual(control.value as? String, "متوقف")
        let bar = XCTAttachment(screenshot: app.screenshot())
        bar.name = "\(name)-keyboard"
        bar.lifetime = .keepAlways
        add(bar)
        done.tap()
    }
}
