import XCTest

extension XCUIApplication {
    /// Taps the keyboard bar's `key` and waits for the modifier toggle
    /// `toggle` (the key itself if nil) to read `value`. Returns what the
    /// toggle read last, nil if it was gone.
    ///
    /// A bar key's action sends the PC its key and sets the toggles' values
    /// in one step. On a starved runner a tap can be lost before that step:
    /// on #172 XCTest synthesized one tap on Ctrl, the PC got no key and Ctrl
    /// still read Off. So this waits for the key to be hittable, touches it
    /// where it is, and touches it once more only if the toggle doesn't read
    /// `value` and the PC's input log at `path` shows nothing since the
    /// touch. A tap that reached the PC is never repeated, so a toggle that
    /// flips twice, or stays put although the PC got the key, still fails.
    /// A toggle that stays put and sends nothing looks like a lost touch and
    /// gets the second tap.
    func tapKeyboardKey(_ key: XCUIElement, until value: String, on toggle: XCUIElement? = nil,
                        hostLog path: String) -> String? {
        let toggle = toggle ?? key
        for attempt in 1...2 {
            guard Self.becomes(key, "exists == true AND hittable == true", within: 5),
                  let frame = try? key.snapshot().frame else { break }
            let before = (try? Data(contentsOf: URL(fileURLWithPath: path)).count) ?? 0
            coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: frame.midX, dy: frame.midY)).tap()
            if Self.becomes(toggle, "exists == true AND value == %@", value, within: 5) { return value }
            let relayed = NSPredicate { _, _ in
                guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)), data.count >= before else { return false }
                return String(decoding: data.dropFirst(before), as: UTF8.self).contains("Input injection")
            }
            guard attempt == 1, !Self.becomes(nil, relayed, within: 2) else { break }
            XCTContext.runActivity(named: "The tap on \(key) never reached the PC; tapping again") { _ in }
        }
        return (try? toggle.snapshot())?.value as? String
    }

    // A waiter can time out without looking again when the runner stalls
    // past its deadline, so this looks once more before giving up.
    private static func becomes(_ element: XCUIElement, _ format: String, _ arguments: CVarArg...,
                                within timeout: TimeInterval) -> Bool {
        becomes(element, NSPredicate(format: format, argumentArray: arguments), within: timeout)
    }

    private static func becomes(_ object: Any?, _ predicate: NSPredicate, within timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: object)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
            || predicate.evaluate(with: object)
    }
}
