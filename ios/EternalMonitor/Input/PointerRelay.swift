import CoreGraphics

struct PointerRelayMachine {
    private var buttons: UInt8 = 0
    private var lastPoint: TouchRelayMachine.Point?
    private var lastHoverUs: UInt64?
    private var scrollRemainder: CGSize = .zero

    private func event(kind: UInt8, phase: UInt8, button: UInt8 = 0,
                       point: TouchRelayMachine.Point, dx: Int16 = 0, dy: Int16 = 0) -> WireInputEvent {
        WireInputEvent(kind: kind, phase: phase, buttons: button, eventId: 0,
            xNorm: point.x, yNorm: point.y, scrollDx: dx, scrollDy: dy, clientTimeUs: 0)
    }

    mutating func began(at point: TouchRelayMachine.Point?, mask: UInt8) -> [WireInputEvent] {
        guard let point else { return [] }
        lastPoint = point
        let requested = mask == 0 ? 1 : mask & 7
        let new = requested & ~buttons
        buttons |= requested
        return [UInt8(1), 2, 4].filter { new & $0 != 0 }
            .map { event(kind: 2, phase: 0, button: $0, point: point) }
    }

    mutating func moved(to point: TouchRelayMachine.Point?) -> [WireInputEvent] {
        guard let point else { return [] }
        lastPoint = point
        return [event(kind: 2, phase: 1, button: buttons, point: point)]
    }

    mutating func ended(at point: TouchRelayMachine.Point?, remainingMask: UInt8 = 0) -> [WireInputEvent] {
        if let point { lastPoint = point }
        guard let lastPoint else { return [] }
        let released = buttons & ~remainingMask
        buttons &= remainingMask
        return [UInt8(1), 2, 4].filter { released & $0 != 0 }
            .map { event(kind: 2, phase: 2, button: $0, point: lastPoint) }
    }

    mutating func hover(at point: TouchRelayMachine.Point?, timeUs: UInt64) -> [WireInputEvent] {
        guard let point else { return [] }
        guard lastHoverUs.map({ timeUs &- $0 >= 16_667 }) ?? true else { return [] }
        lastHoverUs = timeUs
        lastPoint = point
        return [event(kind: 6, phase: 1, point: point)]
    }

    mutating func scroll(at point: TouchRelayMachine.Point?, delta: CGSize) -> [WireInputEvent] {
        guard let point else { return [] }
        lastPoint = point
        scrollRemainder.width += delta.width
        scrollRemainder.height += delta.height
        let dx = Int16(clamping: Int(scrollRemainder.width.rounded()))
        let dy = Int16(clamping: Int(scrollRemainder.height.rounded()))
        scrollRemainder.width -= CGFloat(dx)
        scrollRemainder.height -= CGFloat(dy)
        guard dx != 0 || dy != 0 else { return [] }
        return [event(kind: 3, phase: 1, point: point, dx: dx, dy: dy)]
    }
}
