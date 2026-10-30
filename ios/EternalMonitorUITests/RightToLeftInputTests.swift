import XCTest

/// In Arabic the app's own controls mirror, but the PC picture and touch
/// input do not: a tap on the left of the iPad clicks the left of the PC.
final class RightToLeftInputTests: UITestCase {
    func testArabicMirrorsControlsButNotInput() throws {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["EM_INPUT_HOST_LOG"])
        let before = (try? Data(contentsOf: URL(fileURLWithPath: path)).count) ?? 0
        let width = Double(environment["EM_INPUT_WIDTH"] ?? "640") ?? 640
        let height = Double(environment["EM_INPUT_HEIGHT"] ?? "360") ?? 360
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES",
                               "-playPCaudio", "NO", "-pictureFit", "fit", "-appLanguage", "ar"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        launchCleanly(app)
        defer { app.terminate() }

        // The controls are Arabic and mirrored: Disconnect, last in the bar,
        // sits at its left end.
        let disconnect = app.buttons["display.disconnect"]
        let keyboard = app.buttons["display.keyboard"]
        XCTAssertTrue(keyboard.waitForExistence(timeout: 15))
        XCTAssertEqual(disconnect.label, "قطع الاتصال")
        XCTAssertLessThan(disconnect.frame.midX, keyboard.frame.midX, "the controls did not mirror")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "arabic-stream"
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
    }
}
