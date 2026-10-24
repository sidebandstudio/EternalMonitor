import XCTest
@testable import EternalMonitor

/// Apple Pencil on iPhone Duo is the USB-C Pencil: tilt, but no pressure and
/// no hover. A Pencil without pressure may still report a maximum force while
/// its force stays 0, and Windows apps draw nothing at pressure 0.
@MainActor
final class DuoPencilTests: XCTestCase {
    private func reading(force: CGFloat, maximumForce: CGFloat = 4, altitude: CGFloat = .pi / 4,
                         azimuth: CGFloat = 0, time: UInt64 = 1) -> PencilReading {
        PencilReading(point: .init(x: 100, y: 200), force: force, maximumForce: maximumForce,
            altitude: altitude, azimuth: azimuth, timeUs: time)
    }

    func testAPencilWithoutPressureDrawsAtHalfPressureWhileTouching() {
        var pen = PencilRelayMachine()
        let down = pen.began(reading(force: 0, time: 1))
        XCTAssertEqual(down.map(\.pressureX1000), [500])
        let moves = pen.moved(reading(force: 0, time: 2)) + pen.moved(reading(force: 1e-7, time: 3))
        XCTAssertEqual(moves.map(\.pressureX1000), [500, 500])
        XCTAssertTrue(moves.allSatisfy { $0.buttons == 1 })
        // Lifting still releases at 0.
        XCTAssertEqual(pen.ended(nil, cancelled: false).map(\.pressureX1000), [0])
    }

    func testAPencilWithPressureIsUnchanged() {
        var pen = PencilRelayMachine()
        XCTAssertEqual(pen.began(reading(force: 1, time: 1)).map(\.pressureX1000), [250])
        XCTAssertEqual(pen.moved(reading(force: 3, time: 2)).map(\.pressureX1000), [750])
        XCTAssertEqual(pen.moved(reading(force: 4, time: 3)).map(\.pressureX1000), [1000])
        // The lightest touch a pressure Pencil reports still goes out as measured.
        XCTAssertEqual(pen.moved(reading(force: 0.004, time: 4)).map(\.pressureX1000), [1])
        XCTAssertEqual(pen.moved(reading(force: 100, time: 5)).map(\.pressureX1000), [1000])
    }

    func testHoverSendsNoPressure() {
        var pen = PencilRelayMachine()
        // As TouchRelayView builds a hover sample.
        let hover = pen.hover(reading(force: 0, maximumForce: 1))
        XCTAssertEqual(hover.map(\.pressureX1000), [0])
        XCTAssertEqual(hover.map(\.buttons), [0])
        XCTAssertEqual(pen.hover(nil).map(\.pressureX1000), [0])
    }

    func testTiltIsUnchangedWithoutPressure() {
        let cases: [(altitude: CGFloat, azimuth: CGFloat, x: Int16, y: Int16)] = [
            (.pi / 2, 0, 0, 0), (.pi / 4, 0, 45, 0), (.pi / 4, .pi, -45, 0),
            (.pi / 4, .pi / 2, 0, 45), (.pi / 3, -.pi / 2, 0, -30),
        ]
        for (altitude, azimuth, x, y) in cases {
            let flat = reading(force: 0, altitude: altitude, azimuth: azimuth)
            let pressed = reading(force: 2, altitude: altitude, azimuth: azimuth)
            XCTAssertEqual(flat.tiltX, x, "\(altitude) \(azimuth)")
            XCTAssertEqual(flat.tiltY, y, "\(altitude) \(azimuth)")
            XCTAssertEqual(flat.tiltX, pressed.tiltX)
            XCTAssertEqual(flat.tiltY, pressed.tiltY)
            let event = flat.event(phase: 1, contact: true)
            XCTAssertEqual(event.tiltX, x)
            XCTAssertEqual(event.tiltY, y)
        }
    }

    /// The whole path, from a UITouch with a maximum force but no force.
    func testAStrokeFromAPencilWithoutPressureReachesThePCAtHalfPressure() {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 466, height: 678))
        view.videoSize = CGSize(width: 1920, height: 1080)
        view.nativePenSupported = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = NoPressureTip()
        view.touchesBegan([tip], with: nil)
        tip.sampleTime = 2
        view.touchesMoved([tip], with: nil)
        view.touchesEnded([tip], with: nil)
        XCTAssertEqual(events.map(\.phase), [0, 1, 2])
        XCTAssertEqual(events.map(\.pressureX1000), [500, 500, 0])
        XCTAssertTrue(events.allSatisfy { $0.kind == 1 && $0.tiltX == 45 })
    }
}

/// The USB-C Pencil as it might report itself: a maximum force, no force.
private final class NoPressureTip: UITouch {
    var sampleTime: TimeInterval = 1
    override var type: UITouch.TouchType { .pencil }
    override var timestamp: TimeInterval { sampleTime }
    override var force: CGFloat { 0 }
    override var maximumPossibleForce: CGFloat { 4.17 }
    override var altitudeAngle: CGFloat { .pi / 4 }
    override func azimuthAngle(in view: UIView?) -> CGFloat { 0 }
    override func location(in view: UIView?) -> CGPoint { CGPoint(x: 233, y: 339) }
    override func preciseLocation(in view: UIView?) -> CGPoint { CGPoint(x: 233, y: 339) }
}
