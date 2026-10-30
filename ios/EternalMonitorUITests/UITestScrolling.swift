import XCTest

extension XCUIApplication {
    /// Scrolls up until `element` is on screen. Settings rows load lazily, so
    /// a row further down may not exist at all until it is scrolled near;
    /// a fixed number of swipes breaks whenever a section is added above.
    /// Each step drags a third of the list from its left margin, clear of any
    /// switch or segmented control, and holds before lifting so the list can't
    /// fling past a row a busy runner is still slow to report. A row that
    /// exists gets time to become hittable, and one scrolled past is brought
    /// back down.
    /// Returns once the list has stopped moving: a tap while it still
    /// decelerates only stops the scroll, and the element is never pressed.
    @discardableResult
    func swipeUp(toReveal element: XCUIElement, maxSwipes: Int = 12) -> Bool {
        for _ in 0..<maxSwipes {
            if element.exists && Self.settled(element, timeout: 3) { return true }
            let list = collectionViews.allElementsBoundByIndex.last(where: { $0.isHittable }) ?? self
            let frame = list.frame
            let back = Self.frame(of: element).map { $0.midY < frame.minY } ?? false
            let start = list.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: 6, dy: frame.height * (back ? 0.35 : 0.65)))
            start.press(forDuration: 0.05,
                        thenDragTo: start.withOffset(CGVector(dx: 0, dy: frame.height * (back ? 0.3 : -0.3))),
                        withVelocity: .slow, thenHoldForDuration: 0.3)
        }
        return element.exists && Self.settled(element)
    }

    /// Taps the button `label` in the menu just opened. Until the menu has
    /// slid in (from the left in Arabic) its items exist with an infinite
    /// frame, and reading `hittable` of one then fails the test at once
    /// ("Activation point invalid"), so a predicate wait on it can't wait.
    /// A snapshot reports the frame without that check: the item is tapped
    /// at its centre once it lies inside the window and holds still for two
    /// polls. Tapped once only: a second tap on a closing menu would land on
    /// the row under it.
    func chooseMenuItem(_ label: String, timeout: TimeInterval = 10) -> Bool {
        let item = buttons[label]
        let window = frame
        let deadline = Date().addingTimeInterval(timeout)
        var last: CGRect?
        while Date() < deadline {
            let landed = Self.frame(of: item).flatMap { !$0.isEmpty && window.contains($0) ? $0 : nil }
            if let landed, landed == last {
                // Frames are in screen points; the app's .zero is at window's origin.
                coordinate(withNormalizedOffset: .zero)
                    .withOffset(CGVector(dx: landed.midX - window.minX, dy: landed.midY - window.minY)).tap()
                return true
            }
            last = landed
            Thread.sleep(forTimeInterval: 0.15)
        }
        return false
    }

    /// Where `element` is, or `nil` while it is gone. A row can leave the
    /// list between two reads (in a review run the diagnostics row did, a
    /// second after the list stopped), and reading `frame` of an element
    /// that is gone fails the test; a snapshot reports it instead.
    private static func frame(of element: XCUIElement) -> CGRect? {
        (try? element.snapshot())?.frame
    }

    /// Whether `element` keeps the same frame across two polls and can be
    /// tapped, within `timeout`. A row that briefly leaves the list is waited
    /// for, not swiped past.
    private static func settled(_ element: XCUIElement, timeout: TimeInterval = 5) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        var last: CGRect?
        while Date() < deadline {
            guard let frame = frame(of: element) else {
                last = nil
                Thread.sleep(forTimeInterval: 0.15)
                continue
            }
            if frame == last && element.isHittable { return true }
            last = frame
            Thread.sleep(forTimeInterval: 0.15)
        }
        return false
    }
}
