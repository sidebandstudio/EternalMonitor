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
        let control = buttons[identifier]
        for _ in 0..<3 where !result.exists {
            if !control.exists { revealHUD(reveal) }
            // A snapshot throws, instead of failing the test, if the control
            // fades between the check and the read.
            guard Self.appears(control, within: 5), let frame = try? control.snapshot().frame else { continue }
            coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
            if Self.appears(result, within: 5) { return }
        }
        XCTAssertTrue(result.exists, "\(identifier) did not open \(result)", file: file, line: line)
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

    private static func appears(_ element: XCUIElement, within timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: element)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }
}
