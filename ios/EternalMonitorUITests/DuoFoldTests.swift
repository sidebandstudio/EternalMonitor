import XCTest

/// The iPhone Duo shows the app on its outer screen while folded and on its
/// inner screen while open. Folding moves the running app between the two,
/// so the picture, its bars and where a touch lands on the PC must follow
/// every fold, in Fit and in Fill.
final class DuoFoldTests: XCTestCase {
    private enum Screen: String {
        case outer, inner
    }

    /// The synthetic host's picture (`ETERNAL_SYNTH_SIZE`).
    private let video = CGSize(width: 640, height: 360)

    override func setUpWithError() throws {
        try XCTSkipUnless(EMHingeAvailable(), "Only the iPhone Duo has a hinge")
        continueAfterFailure = false
    }

    override func tearDown() {
        // Every other test expects the folded, iPhone-sized screen.
        if EMHingeAvailable() { _ = EMSetHingeAngle(0, 5, nil) }
    }

    func testConnectAndPairOnBothScreens() throws {
        let environment = ProcessInfo.processInfo.environment
        let code = try XCTUnwrap(environment["EM_PAIRING_CODE"], "Harness must read the real host's startup code")
        let host = try XCTUnwrap(environment["EM_PAIRING_HOST"]).split(separator: ":").map(String.init)
        XCTAssertEqual(host.count, 2)
        fold(to: 0)
        let app = XCUIApplication()
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "NO",
                               "-playPCaudio", "NO", "-pictureFit", "fit", "-lastHost", host[0], "-lastPort", host[1]]
        app.launch()
        defer { app.terminate() }
        let hostField = app.textFields["connect.host"]
        let connect = app.buttons["connect.button"]
        XCTAssertTrue(hostField.waitForExistence(timeout: 15))
        waitUntil(app, isOn: .outer)
        XCTAssertTrue(connect.isHittable, "Connect is off the outer screen")
        capture("connect", on: .outer, app: app)

        fold(to: 180, app: app, expecting: .inner)
        XCTAssertEqual(hostField.value as? String, host[0], "Unfolding lost the address")
        XCTAssertTrue(connect.isHittable, "Connect is off the inner screen")
        capture("connect", on: .inner, app: app)

        connect.tap()
        let input = app.textFields["pairing.code"]
        XCTAssertTrue(input.waitForExistence(timeout: 10), "Connect on the inner screen did not ask for the code")
        capture("pairing", on: .inner, app: app)

        fold(to: 0, app: app, expecting: .outer)
        XCTAssertTrue(input.waitForExistence(timeout: 5), "Folding closed the pairing sheet")
        capture("pairing", on: .outer, app: app)
        input.tap()
        input.typeText(code)
        let bar = app.buttons["pairing.submitBar"]
        (bar.exists ? bar : app.buttons["pairing.submit"]).tap()
        XCTAssertTrue(app.buttons["display.disconnect"].waitForExistence(timeout: 10), "Pairing on the outer screen did not connect")
        XCTAssertFalse(input.exists)
        capture("paired-stream", on: .outer, app: app)

        // Leave no pairing behind for PairingFlowTests.
        app.tapHUDControl("display.settings", until: app.navigationBars["Settings"])
        let forget = app.buttons["settings.forgetPairings"]
        XCTAssertTrue(app.swipeUp(toReveal: forget))
        forget.tap()
        app.buttons["settings.done"].tap()
        app.tapHUDControl("display.disconnect", until: hostField)
    }

    func testStreamKeepsPictureAndTouchesAcrossFolds() throws {
        let environment = ProcessInfo.processInfo.environment
        let log = try XCTUnwrap(environment["EM_INPUT_HOST_LOG"])
        fold(to: 0)
        let app = XCUIApplication()
        // Touches go to the PC; a three-finger tap brings the HUD back.
        app.launchArguments = ["-didSeeOnboarding", "YES", "-allowUSB", "NO", "-controlPC", "YES",
                               "-playPCaudio", "NO", "-pictureFit", "fit"]
        app.launchEnvironment["EM_AUTOCONNECT"] = environment["EM_INPUT_HOST"] ?? "127.0.0.1:19875"
        app.launch()
        defer { app.terminate() }
        XCTAssertTrue(app.buttons["display.disconnect"].waitForExistence(timeout: 15))
        waitUntil(app, isOn: .outer)
        check(app, on: .outer, fill: false, log: log, name: "fit")
        setFill(true, app: app)
        check(app, on: .outer, fill: true, log: log, name: "fill")

        // Open and close in each mode, all on one stream.
        fold(to: 180, app: app, expecting: .inner)
        check(app, on: .inner, fill: true, log: log, name: "fill-after-unfolding")
        setFill(false, app: app)
        check(app, on: .inner, fill: false, log: log, name: "fit")
        // Half open stays on the inner screen.
        fold(to: 110, app: app, expecting: .inner)
        check(app, on: .inner, fill: false, log: log, name: "fit-half-open")
        fold(to: 0, app: app, expecting: .outer)
        check(app, on: .outer, fill: false, log: log, name: "fit-after-folding")
        fold(to: 180, app: app, expecting: .inner)
        check(app, on: .inner, fill: false, log: log, name: "fit-after-unfolding")
        setFill(true, app: app)
        check(app, on: .inner, fill: true, log: log, name: "fill")
        fold(to: 0, app: app, expecting: .outer)
        check(app, on: .outer, fill: true, log: log, name: "fill-after-folding")
        XCTAssertFalse(app.staticTexts["Reconnecting to your PC"].exists, "A fold dropped the stream")
    }

    // MARK: - Folding

    private func fold(to degrees: Double, app: XCUIApplication? = nil, expecting screen: Screen? = nil,
                      file: StaticString = #filePath, line: UInt = #line) {
        var error: NSString?
        XCTAssertTrue(EMSetHingeAngle(degrees, 5, &error), error as String? ?? "", file: file, line: line)
        if let app, let screen { waitUntil(app, isOn: screen, file: file, line: line) }
    }

    /// The outer screen is iPhone-sized (466 x 678 points); the inner one
    /// shows the app in landscape, about 951 x 669.
    private func waitUntil(_ app: XCUIApplication, isOn screen: Screen,
                           file: StaticString = #filePath, line: UInt = #line) {
        let deadline = Date().addingTimeInterval(10)
        while screenShowing(app) != screen && Date() < deadline { Thread.sleep(forTimeInterval: 0.25) }
        XCTAssertEqual(screenShowing(app), screen, "The app is \(app.frame.size), not on the \(screen) screen",
                       file: file, line: line)
        // Let the move between screens finish before reading the picture.
        Thread.sleep(forTimeInterval: 1)
    }

    /// XCUITest reports the inner screen in its portrait panel's points
    /// (669 x 951) while the app lays out in landscape on it (951 x 669),
    /// its top along the panel's right edge.
    private func interfaceSize(_ app: XCUIApplication) -> CGSize {
        let frame = app.frame.size
        return screenShowing(app) == .inner && frame.height > frame.width
            ? CGSize(width: frame.height, height: frame.width) : frame
    }

    /// Where XCUITest must touch to reach `point` in the app's layout.
    private func screenPoint(_ point: CGPoint, app: XCUIApplication) -> CGPoint {
        let frame = app.frame.size
        return interfaceSize(app) == frame ? point : CGPoint(x: frame.width - point.y, y: point.x)
    }

    private func screenShowing(_ app: XCUIApplication) -> Screen? {
        let size = app.frame.size
        if size.width < 600 && size.height > size.width { return .outer }
        if max(size.width, size.height) > 800 { return .inner }
        return nil
    }

    // MARK: - Stream checks

    private func setFill(_ fill: Bool, app: XCUIApplication) {
        let label = fill ? "Show the whole picture" : "Fill the screen"
        let after = app.buttons.matching(NSPredicate(format: "identifier == 'display.pictureFit' AND label == %@", label)).firstMatch
        app.tapHUDControl("display.pictureFit", until: after, reveal: .threeFingerTap)
    }

    /// The picture fills or fits the screen the app is on, with bars only in
    /// Fit, and a touch on it lands on the matching PC pixel.
    private func check(_ app: XCUIApplication, on screen: Screen, fill: Bool, log: String, name: String,
                       file: StaticString = #filePath, line: UInt = #line) {
        // The HUD hides five seconds after its last use; its bars cover the picture.
        let pictureButton = app.buttons["display.pictureFit"]
        if pictureButton.exists {
            expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: pictureButton)
            waitForExpectations(timeout: 10)
        }
        let window = interfaceSize(app)
        let picture = pictureRect(fill: fill, in: window)
        var problems: [String] = []
        let deadline = Date().addingTimeInterval(10)
        var shot = upright(screenshot(of: screen), like: window)
        repeat {
            problems = pictureProblems(Pixels(shot, window: window), picture: picture, fill: fill)
            if problems.isEmpty { break }
            Thread.sleep(forTimeInterval: 0.5)
            shot = upright(screenshot(of: screen), like: window)
        } while Date() < deadline
        attach(shot, name: "\(screen)-\(name)")
        XCTAssertTrue(problems.isEmpty, "\(screen) \(name): " + problems.joined(separator: "; "), file: file, line: line)

        // Two touches inside the picture, away from its edges and the HUD.
        let targets = [CGPoint(x: window.width * 0.3, y: window.height * 0.45),
                       CGPoint(x: window.width * 0.7, y: window.height * 0.55)]
        let before = (try? Data(contentsOf: URL(fileURLWithPath: log)).count) ?? 0
        for target in targets {
            let point = screenPoint(target, app: app)
            app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: point.x, dy: point.y)).tap()
        }
        var presses: [CGPoint] = []
        let pressDeadline = Date().addingTimeInterval(10)
        repeat {
            presses = leftDowns(in: log, after: before)
            if presses.count >= targets.count { break }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < pressDeadline
        XCTAssertGreaterThanOrEqual(presses.count, targets.count, "\(screen) \(name): the PC got \(presses.count) clicks",
                                    file: file, line: line)
        for (target, press) in zip(targets, presses) {
            // Where the PC should click, in its 0...65535 input space.
            let expected = CGPoint(x: (target.x - picture.minX) / picture.width * 65535,
                                   y: (target.y - picture.minY) / picture.height * 65535)
            // 600 is under 1% of the picture: a stale screen size or the
            // other mode's mapping misses by several times that.
            XCTAssertLessThan(max(abs(press.x - expected.x), abs(press.y - expected.y)), 600,
                              "\(screen) \(name): a touch at \(target) clicked \(press), not \(expected)", file: file, line: line)
        }
    }

    private func pictureRect(fill: Bool, in window: CGSize) -> CGRect {
        let fit = min(window.width / video.width, window.height / video.height)
        let cover = max(window.width / video.width, window.height / video.height)
        let size = CGSize(width: video.width * (fill ? cover : fit), height: video.height * (fill ? cover : fit))
        return CGRect(x: (window.width - size.width) / 2, y: (window.height - size.height) / 2,
                      width: size.width, height: size.height)
    }

    /// The test pattern's green never drops below a quarter; the bars are
    /// the theme's near-black. Its top 24 rows are a black frame counter, so
    /// only the bottom, left and right edges are checked exactly.
    private func pictureProblems(_ pixels: Pixels, picture: CGRect, fill: Bool) -> [String] {
        let window = pixels.window
        var problems: [String] = []
        func expect(_ point: CGPoint, picture shown: Bool, _ what: String) {
            let green = pixels.green(at: point)
            if shown ? green <= 48 : green >= 24 { problems.append("\(what) at \(point) has green \(green)") }
        }
        let visible = picture.intersection(CGRect(origin: .zero, size: window))
        expect(CGPoint(x: visible.minX + visible.width * 0.25, y: visible.midY), picture: true, "the picture")
        expect(CGPoint(x: visible.minX + visible.width * 0.75, y: visible.midY), picture: true, "the picture")
        expect(CGPoint(x: visible.midX, y: visible.minY + visible.height * 0.85), picture: true, "the picture")
        if fill {
            expect(CGPoint(x: 6, y: window.height / 2), picture: true, "the left edge")
            expect(CGPoint(x: window.width - 6, y: window.height / 2), picture: true, "the right edge")
            expect(CGPoint(x: window.width / 2, y: window.height - 6), picture: true, "the bottom edge")
            return problems
        }
        if picture.minY > 12 {
            expect(CGPoint(x: window.width / 2, y: picture.minY / 2), picture: false, "the top bar")
            expect(CGPoint(x: window.width / 2, y: (picture.maxY + window.height) / 2), picture: false, "the bottom bar")
            expect(CGPoint(x: window.width / 2, y: picture.maxY - 5), picture: true, "the picture's bottom edge")
            expect(CGPoint(x: window.width / 2, y: picture.maxY + 5), picture: false, "below the picture")
        }
        if picture.minX > 12 {
            expect(CGPoint(x: picture.minX / 2, y: window.height / 2), picture: false, "the left bar")
            expect(CGPoint(x: (picture.maxX + window.width) / 2, y: window.height / 2), picture: false, "the right bar")
            expect(CGPoint(x: picture.minX + 5, y: window.height / 2), picture: true, "the picture's left edge")
            expect(CGPoint(x: picture.minX - 5, y: window.height / 2), picture: false, "left of the picture")
        }
        return problems
    }

    /// Positions of the host's recorded left-button presses since `offset`.
    private func leftDowns(in log: String, after offset: Int) -> [CGPoint] {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: log)), data.count > offset else { return [] }
        let text = String(decoding: data.dropFirst(offset), as: UTF8.self)
        let pattern = try! NSRegularExpression(pattern: #"LeftDown \{ x: (\d+), y: (\d+) \}"#)
        return pattern.matches(in: text, range: NSRange(text.startIndex..., in: text)).compactMap { match in
            guard let x = Range(match.range(at: 1), in: text), let y = Range(match.range(at: 2), in: text),
                  let px = Double(text[x]), let py = Double(text[y]) else { return nil }
            return CGPoint(x: px, y: py)
        }
    }

    // MARK: - Screenshots

    /// `XCUIScreen.main` is always the outer screen; the inner one is larger.
    private func screenshot(of screen: Screen) -> UIImage {
        let shots = XCUIScreen.screens.map { $0.screenshot().image }
            .sorted { $0.size.width * $0.size.height < $1.size.width * $1.size.height }
        return screen == .outer ? shots.first! : shots.last!
    }

    /// The screenshot drawn the way the app lays out. The inner screen's
    /// pixels run a quarter turn clockwise from the app's landscape layout.
    private func upright(_ image: UIImage, like window: CGSize) -> UIImage {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let oriented = UIGraphicsImageRenderer(size: image.size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: image.size))
        }
        guard (oriented.size.width > oriented.size.height) != (window.width > window.height) else { return oriented }
        let turned = CGSize(width: oriented.size.height, height: oriented.size.width)
        // A quarter turn back: the panel's right edge becomes the top.
        return UIGraphicsImageRenderer(size: turned, format: format).image { context in
            context.cgContext.translateBy(x: 0, y: turned.height)
            context.cgContext.rotate(by: -.pi / 2)
            oriented.draw(at: .zero)
        }
    }

    private func attach(_ image: UIImage, name: String) {
        let attachment = XCTAttachment(image: image)
        attachment.name = "duo-\(name)"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func capture(_ name: String, on screen: Screen, app: XCUIApplication) {
        attach(upright(screenshot(of: screen), like: interfaceSize(app)), name: "\(screen)-\(name)")
    }
}

/// One upright screenshot's pixels, addressed in window points.
private struct Pixels {
    let window: CGSize
    private let width: Int, height: Int
    private let bytes: [UInt8]

    init(_ image: UIImage, window: CGSize) {
        self.window = window
        let cgImage = image.cgImage!
        let (width, height) = (cgImage.width, cgImage.height)
        var bytes = [UInt8](repeating: 0, count: width * height * 4)
        bytes.withUnsafeMutableBytes { buffer in
            let context = CGContext(data: buffer.baseAddress, width: width, height: height, bitsPerComponent: 8,
                                    bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            context.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))
        }
        (self.width, self.height, self.bytes) = (width, height, bytes)
    }

    func green(at point: CGPoint) -> Int {
        let x = min(max(Int(point.x / window.width * CGFloat(width)), 0), width - 1)
        let y = min(max(Int(point.y / window.height * CGFloat(height)), 0), height - 1)
        // The bitmap's first row is the image's top row.
        return Int(bytes[(y * width + x) * 4 + 1])
    }
}
