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
            let back = element.exists && element.frame.midY < frame.minY
            let start = list.coordinate(withNormalizedOffset: .zero)
                .withOffset(CGVector(dx: 6, dy: frame.height * (back ? 0.35 : 0.65)))
            start.press(forDuration: 0.05,
                        thenDragTo: start.withOffset(CGVector(dx: 0, dy: frame.height * (back ? 0.3 : -0.3))),
                        withVelocity: .slow, thenHoldForDuration: 0.3)
        }
        return element.exists && Self.settled(element)
    }

    /// Whether `element` keeps the same frame across two polls and can be
    /// tapped, within `timeout`.
    private static func settled(_ element: XCUIElement, timeout: TimeInterval = 5) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        var last: CGRect?
        while Date() < deadline {
            guard element.exists else { return false }
            let frame = element.frame
            if frame == last && element.isHittable { return true }
            last = frame
            Thread.sleep(forTimeInterval: 0.15)
        }
        return false
    }
}
