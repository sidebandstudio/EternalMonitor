import XCTest
@testable import EternalMonitor

@MainActor
final class PencilRelayTests: XCTestCase {
    private func reading(_ time: UInt64, force: CGFloat = 2) -> PencilReading {
        PencilReading(point: .init(x: 100, y: 200), force: force, maximumForce: 4,
            altitude: .pi / 4, azimuth: 0, timeUs: time)
    }

    func testPressureStartsAtContactAndCoalescedSamplesAreNotThrottled() {
        var pen = PencilRelayMachine()
        let down = pen.began(reading(1_000))
        XCTAssertEqual(down[0].pressureX1000, 500)
        XCTAssertEqual(down[0].inputVer, 2)
        let moves = [1_001, 1_002, 1_003].flatMap { pen.moved(reading(UInt64($0), force: 3)) }
        XCTAssertEqual(moves.count, 3)
        XCTAssertEqual(moves.map(\.clientTimeUs), [1_001, 1_002, 1_003])
        XCTAssertEqual(moves.map(\.pressureX1000), [750, 750, 750])
        XCTAssertTrue(pen.moved(reading(1_002)).isEmpty)
        XCTAssertEqual(pen.ended(nil, cancelled: false)[0].pressureX1000, 0)
        XCTAssertTrue(pen.moved(reading(2_000)).isEmpty)
    }

    func testTiltProjectionUsesSignedViewAxesAndSanitizesSensors() {
        func sample(_ altitude: CGFloat, _ azimuth: CGFloat) -> PencilReading {
            PencilReading(point: .init(x: 0, y: 0), force: .nan, maximumForce: 0,
                altitude: altitude, azimuth: azimuth, timeUs: 1)
        }
        let upright = sample(.pi / 2, 0)
        XCTAssertEqual(upright.tiltX, 0)
        XCTAssertEqual(upright.tiltY, 0)
        XCTAssertEqual(sample(.pi / 4, 0).tiltX, 45)
        XCTAssertEqual(sample(.pi / 4, .pi).tiltX, -45)
        XCTAssertEqual(sample(.pi / 4, .pi / 2).tiltY, 45)
        XCTAssertEqual(sample(.pi / 4, -.pi / 2).tiltY, -45)
        XCTAssertEqual(sample(.nan, .infinity).tiltX, 0)
        XCTAssertEqual(sample(.nan, .infinity).pressure, 500)
        XCTAssertEqual(reading(1, force: 100).pressure, 1000)
        XCTAssertEqual(reading(1, force: -1).pressure, 0)
    }

    func testCancelAndHoverNeverDrawAndReleaseOnlyOnce() {
        var pen = PencilRelayMachine()
        XCTAssertEqual(pen.hover(reading(1))[0].buttons, 0)
        let start = pen.began(reading(2))
        XCTAssertEqual(start.map(\.phase), [2, 0])
        XCTAssertTrue(pen.hover(reading(3)).isEmpty)
        XCTAssertEqual(pen.cancel().map(\.phase), [3])
        XCTAssertTrue(pen.cancel().isEmpty)
        XCTAssertFalse(pen.isDown)
    }

    func testReliableUSBPreservesSampleTimingWithoutDuplicatingEdges() {
        var sequence = InputEventSequencer()
        let samples = [reading(1).event(phase: 0, contact: true),
                       reading(2).event(phase: 1, contact: true),
                       reading(3).event(phase: 2, contact: true)]
        let packets = sequence.packets(samples, timeUs: 10, reliable: true)
        XCTAssertEqual(packets.map(\.eventId), [1, 2, 3])
        XCTAssertEqual(packets.map(\.clientTimeUs), [1, 2, 3])
        XCTAssertEqual(sequence.packets(samples, timeUs: 10).map(\.eventId), [4, 4, 5, 6, 6])
        XCTAssertEqual(DrawingProfile(enabled: true, isUSB: true).preferredFPS(30, maximum: 120), 120)
        XCTAssertEqual(DrawingProfile(enabled: true, isUSB: true).preferredFPS(120, maximum: 60), 60)
        XCTAssertEqual(DrawingProfile(enabled: true, isUSB: false).preferredFPS(30, maximum: 120), 30)
        XCTAssertEqual(DrawingProfile(enabled: false, isUSB: true).preferredFPS(30, maximum: 120), 30)
    }

    func testPalmCannotInterruptPencilOrStartFingerGestureAfterPencilLifts() {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 100, height: 100))
        view.videoSize = CGSize(width: 100, height: 100)
        view.nativePenSupported = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = TestTouch(.pencil)
        let palm = TestTouch(.direct)
        view.touchesBegan([tip, palm], with: nil)
        XCTAssertEqual(events.map(\.kind), [1])
        XCTAssertEqual(events[0].pressureX1000, 500)
        tip.sampleTime = 2
        view.touchesMoved([palm, tip], with: nil)
        XCTAssertEqual(events.map(\.phase), [0, 1])
        view.touchesEnded([tip], with: nil)
        view.touchesMoved([palm], with: nil)
        view.touchesEnded([palm], with: nil)
        XCTAssertEqual(events.map(\.phase), [0, 1, 2])
        XCTAssertTrue(events.allSatisfy { $0.kind == 1 })
    }

    func testDrawingModeRejectsPalmBeforeTipAndCancellationReleasesTip() {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 100, height: 100))
        view.videoSize = CGSize(width: 100, height: 100)
        view.nativePenSupported = true
        view.drawingMode = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let palm = TestTouch(.direct)
        view.touchesBegan([palm], with: nil)
        view.touchesMoved([palm], with: nil)
        XCTAssertTrue(events.isEmpty)
        let tip = TestTouch(.pencil)
        view.touchesBegan([tip], with: nil)
        view.touchesEnded([palm], with: nil)
        XCTAssertEqual(events.map(\.phase), [0])
        view.active = false
        XCTAssertEqual(events.map(\.phase), [0, 3])
        view.active = false
        XCTAssertEqual(events.count, 2)
    }

    func testPencilUsesPreciseLocationAndClampsAtLetterboxEdge() {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 100, height: 100))
        view.videoSize = CGSize(width: 200, height: 100)
        view.nativePenSupported = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = TestTouch(.pencil)
        tip.position = CGPoint(x: 25, y: 25)
        view.touchesBegan([tip], with: nil)
        XCTAssertEqual(events[0].xNorm, 16384)
        XCTAssertEqual(events[0].yNorm, 0)
        tip.position = CGPoint(x: 150, y: 99)
        tip.sampleTime = 2
        view.touchesMoved([tip], with: nil)
        view.touchesEnded([tip], with: nil)
        XCTAssertEqual(events.last?.xNorm, 65535)
        XCTAssertEqual(events.last?.yNorm, 65535)
    }

    func testRotationCancelsStrokeBeforeCoordinateMappingChanges() {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 100, height: 100))
        view.videoSize = CGSize(width: 200, height: 100)
        view.nativePenSupported = true
        view.layoutIfNeeded()
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = TestTouch(.pencil)
        view.touchesBegan([tip], with: nil)
        view.frame.size = CGSize(width: 200, height: 100)
        view.layoutIfNeeded()
        tip.sampleTime = 2
        view.touchesMoved([tip], with: nil)
        XCTAssertEqual(events.map(\.phase), [0, 3])
    }
}

private final class TestTouch: UITouch {
    let touchType: UITouch.TouchType
    var sampleTime: TimeInterval = 1
    var position = CGPoint(x: 50, y: 50)
    init(_ type: UITouch.TouchType) { touchType = type; super.init() }
    override var type: UITouch.TouchType { touchType }
    override var timestamp: TimeInterval { sampleTime }
    override var force: CGFloat { 2 }
    override var maximumPossibleForce: CGFloat { 4 }
    override var altitudeAngle: CGFloat { .pi / 4 }
    override func azimuthAngle(in view: UIView?) -> CGFloat { 0 }
    override func location(in view: UIView?) -> CGPoint { CGPoint(x: 1, y: 1) }
    override func preciseLocation(in view: UIView?) -> CGPoint { position }
}
