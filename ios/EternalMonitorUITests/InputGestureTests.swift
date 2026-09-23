import XCTest

final class InputGestureTests: XCTestCase {
    func testVideoGesturesAndKeyboard() throws {
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .portrait
        let environment = ProcessInfo.processInfo.environment
        let path = try XCTUnwrap(environment["EM_INPUT_HOST_LOG"])
        let before = (try? Data(contentsOf: URL(fileURLWithPath: path)).count) ?? 0
        let width = Double(environment["EM_INPUT_WIDTH"] ?? "640") ?? 640
        let height = Double(environment["EM_INPUT_HEIGHT"] ?? "360") ?? 360
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES", "-playPCaudio", "NO"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        app.launchEnvironment["EM_E2E_LOG"] = "1"
        app.launch()
        defer { app.terminate() }
        let keyboard = app.buttons["display.keyboard"]
        XCTAssertTrue(keyboard.waitForExistence(timeout: 15))
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: keyboard)
        waitForExpectations(timeout: 6)
        let relay = app.otherElements["display.relay"]
        XCTAssertTrue(relay.waitForExistence(timeout: 5))
        let frame = relay.frame
        let scale = min(frame.width / width, frame.height / height)
        let video = CGRect(x: frame.midX - width * scale / 2, y: frame.midY - height * scale / 2,
                           width: width * scale, height: height * scale)
        func point(_ x: Double, _ y: Double) -> CGPoint {
            CGPoint(x: video.minX + x / (width - 1) * video.width,
                    y: video.minY + y / (height - 1) * video.height)
        }
        func coordinate(_ p: CGPoint) -> XCUICoordinate {
            app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: p.x, dy: p.y))
        }
        let clicks = [[width / 2, height / 2], [20, 20], [width - 21, 20],
                      [20, height - 21], [width - 21, height - 21]]
        for target in clicks { coordinate(point(target[0], target[1])).tap() }
        let dragStart = [width * 0.2, height * 0.4]
        let dragEnd = [width * 0.8, height * 0.6]
        coordinate(point(dragStart[0], dragStart[1])).press(forDuration: 0.05,
            thenDragTo: coordinate(point(dragEnd[0], dragEnd[1])),
            withVelocity: .slow, thenHoldForDuration: 0)
        let rightClick = [width * 0.6, height * 0.4]
        coordinate(point(rightClick[0], rightClick[1])).press(forDuration: 0.8)
        let scroll = expectation(description: "Two-finger upward scroll")
        EMSwipeTwoFingers(point(width * 0.5, height * 0.7), video.height * 0.3) { success, error in
            XCTAssertTrue(success, error?.localizedDescription ?? "Touch synthesis failed")
            scroll.fulfill()
        }
        wait(for: [scroll], timeout: 10)
        capture("input-gestures", app: app)
        app.tap(withNumberOfTaps: 1, numberOfTouches: 3)
        XCTAssertTrue(keyboard.waitForExistence(timeout: 5))
        keyboard.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        app.typeText("Hi!\n")
        let relayed = NSPredicate { _, _ in
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)), data.count >= before else { return false }
            let log = String(decoding: data.dropFirst(before), as: UTF8.self)
            return ["LeftDown", "LeftUp", "RightDown", "RightUp", "Wheel { delta:",
                    "Unicode(72)", "Unicode(105)", "Unicode(33)",
                    "KeyDown { scan: 28, extended: false }", "KeyUp { scan: 28, extended: false }"]
                .allSatisfy { log.contains($0) } && !log.contains("probe guard rejected")
        }
        expectation(for: relayed, evaluatedWith: nil)
        waitForExpectations(timeout: 15)
        capture("input-keyboard", app: app)
        let expected: [String: Any] = ["width": width, "height": height, "clicks": clicks,
                                     "drag_start": dragStart, "drag_end": dragEnd,
                                     "right_click": rightClick, "scroll_direction": "up"]
        let attachment = XCTAttachment(data: try JSONSerialization.data(withJSONObject: expected),
                                       uniformTypeIdentifier: "public.json")
        attachment.name = "input-expected"
        attachment.lifetime = .keepAlways
        add(attachment)
        app.buttons["keyboard.done"].tap()
        app.buttons["display.disconnect"].tap()
        XCTAssertTrue(app.textFields["connect.host"].waitForExistence(timeout: 5))
    }

    private func capture(_ name: String, app: XCUIApplication) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
