import XCTest

extension XCUIApplication {
    /// How a test brings the auto-hiding HUD back. A tap toggles it; while
    /// touches go to the PC, only a three-finger tap does.
    enum HUDReveal {
        case tap
        case threeFingerTap
        /// Three fingers side by side mid-screen. XCTest's own three-finger
        /// tap spreads them to the screen edges, where the bottom one can land
        /// on a HUD button as the bar slides away.
        case threeFingersTogether
    }

    /// Taps the HUD control `identifier` until `result` appears.
    ///
    /// The HUD hides five seconds after it was last shown, so a control can
    /// vanish between any check and the touch, and an element tap then fails
    /// the test with "No matches found". This reveals the HUD only when the
    /// control is gone (a reveal toggles it), touches the control's last
    /// position at once, and reveals it and tries again if that touch missed.
    func tapHUDControl(_ identifier: String, until result: XCUIElement, reveal: HUDReveal = .tap,
                       file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(triedHUDControl(identifier, until: result, reveal: reveal),
                      "\(identifier) did not open \(result)", file: file, line: line)
    }

    /// `tapHUDControl` without the failure: whether `result` appeared.
    func triedHUDControl(_ identifier: String, until result: XCUIElement, reveal: HUDReveal = .tap) -> Bool {
        let control = buttons[identifier]
        for _ in 0..<3 where !result.exists {
            if !control.exists { revealHUD(reveal) }
            // A snapshot throws, instead of failing the test, if the control
            // fades between the check and the read.
            guard Self.appears(control, within: 5), let frame = try? control.snapshot().frame else { continue }
            coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
            if Self.appears(result, within: 5) { return true }
        }
        return result.exists
    }

    /// The HUD controls `identifiers`, read from one accessibility snapshot.
    ///
    /// The HUD hides five seconds after it was last shown. On a starved
    /// runner launch and idle alone can take that long (7.9 s on #161), so a
    /// control one query found was gone when the next read its label. This
    /// reads every control at once, and if the HUD has hidden, reveals it and
    /// reads again.
    func hudControls(_ identifiers: [String], reveal: HUDReveal = .tap,
                     file: StaticString = #filePath, line: UInt = #line) -> [String: XCUIElementSnapshot] {
        let found = triedHUDControls(identifiers, reveal: reveal)
        if found.count < identifiers.count {
            XCTFail("The HUD never showed all of \(identifiers): \(found.keys.sorted())", file: file, line: line)
        }
        return found
    }

    /// `hudControls` without the failure: the controls it found.
    func triedHUDControls(_ identifiers: [String], reveal: HUDReveal = .tap) -> [String: XCUIElementSnapshot] {
        var found: [String: XCUIElementSnapshot] = [:]
        for _ in 0..<3 {
            found = [:]
            if let tree = try? snapshot() { Self.collect(identifiers, in: tree, into: &found) }
            if found.count == identifiers.count { return found }
            // A reveal toggles the HUD, so only reveal it once it is gone.
            if found.isEmpty { revealHUD(reveal) }
            _ = Self.appears(descendants(matching: .any)[identifiers[0]].firstMatch, within: 5)
        }
        return found
    }

    /// What the app reports to a UI test launched with `-uiTestProbe YES`
    /// (debug builds): the HUD's Keyboard button the first time it showed
    /// while connected ("never", "shown", or the seconds it stayed), and how
    /// often the signal was lost. A starved runner can miss either while it
    /// lasts. Nil if the app isn't running.
    func probe() -> (keyboardButton: String, signalLosses: Int)? {
        guard let value = (try? descendants(matching: .any)["debug.probe"].firstMatch.snapshot())?.value as? String
        else { return nil }
        let fields = Dictionary(value.split(separator: " ").compactMap { field -> (String, String)? in
            let parts = field.split(separator: "=", maxSplits: 1)
            return parts.count == 2 ? (String(parts[0]), String(parts[1])) : nil
        }, uniquingKeysWith: { $1 })
        guard let button = fields["keyboardButton"], let losses = fields["signalLosses"].flatMap({ Int($0) })
        else { return nil }
        return (button, losses)
    }

    /// Waits for the display, and proves its HUD showed the Keyboard button
    /// on its own and kept it for the HUD's time. The app reports how long
    /// the button first stayed; under 4 s fails (the HUD hides 5 s after it
    /// appears). While the app reports it showing, the button must be in
    /// the accessibility tree: two reports of "shown" around a check that
    /// misses it fail. A starved runner can wait out those 5 s before it
    /// looks (#161), so the app's report is what counts, and this returns
    /// once the HUD has hidden again.
    func waitForDisplayAndHUD(file: StaticString = #filePath, line: UInt = #line) {
        if let problem = displayAndHUDProblem() { XCTFail(problem, file: file, line: line) }
    }

    /// `waitForDisplayAndHUD` without the failure: what went wrong, if
    /// anything.
    func displayAndHUDProblem() -> String? {
        let deadline = Date().addingTimeInterval(30)
        var last = "never"
        while Date() < deadline {
            if let before = probe()?.keyboardButton {
                last = before
                if before == "shown" {
                    if !buttons["display.keyboard"].exists, probe()?.keyboardButton == "shown" {
                        return "The HUD showed the Keyboard button, but not to accessibility"
                    }
                } else if let seconds = Double(before) {
                    return seconds >= 4 ? nil : "The HUD hid \(seconds) s after the stream started"
                }
            }
            Thread.sleep(forTimeInterval: 0.25)
        }
        return last == "never" ? "The HUD did not show when the stream started" : "The HUD was still showing after 30 s"
    }

    func revealHUD(_ reveal: HUDReveal) {
        switch reveal {
        case .tap: coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        case .threeFingerTap: tap(withNumberOfTaps: 1, numberOfTouches: 3)
        case .threeFingersTogether:
            let tapped = XCTestExpectation(description: "Three-finger tap")
            EMTapFingers([-60.0, 0, 60].map { NSValue(cgPoint: CGPoint(x: frame.midX + $0, y: frame.midY)) }) { success, error in
                XCTAssertTrue(success, error?.localizedDescription ?? "Touch synthesis failed")
                tapped.fulfill()
            }
            _ = XCTWaiter().wait(for: [tapped], timeout: 10)
        }
    }

    private static func collect(_ identifiers: [String], in element: XCUIElementSnapshot,
                                into found: inout [String: XCUIElementSnapshot]) {
        if identifiers.contains(element.identifier) { found[element.identifier] = element }
        for child in element.children { collect(identifiers, in: child, into: &found) }
    }

    private static func appears(_ element: XCUIElement, within timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: element)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }
}
