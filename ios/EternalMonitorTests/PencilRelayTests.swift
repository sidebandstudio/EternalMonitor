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

    /// Fill: the 200 × 100 picture covers the 100 × 100 view at 200 × 100,
    /// so 50 pt are trimmed off each side and the view spans x 0.25...0.75.
    func testPencilMapsThroughFillAndClampsToTheVisiblePicture() {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 100, height: 100))
        view.videoSize = CGSize(width: 200, height: 100)
        view.pictureFit = .fill
        view.nativePenSupported = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = TestTouch(.pencil)
        tip.position = CGPoint(x: 0, y: 50)
        view.touchesBegan([tip], with: nil)
        XCTAssertEqual(events[0].xNorm, 16384)
        XCTAssertEqual(events[0].yNorm, 32768)
        // Past the screen edge: held on the visible picture, never the trimmed part.
        tip.position = CGPoint(x: 150, y: 99)
        tip.sampleTime = 2
        view.touchesMoved([tip], with: nil)
        view.touchesEnded([tip], with: nil)
        XCTAssertEqual(events.last?.xNorm, 49151)
        XCTAssertEqual(events.last?.yNorm, 64880)
    }

    func testFingerOnTheFitBarClicksTheFilledPicture() {
        // The test finger sits at (1, 1): a letterbox bar in Fit.
        // Fill: x = (1 + 50) / 200 = 0.255, y = 0.01.
        let cases: [(PictureFit, (x: UInt16, y: UInt16)?)] = [(.fit, nil), (.fill, (16711, 655))]
        for (fit, expected) in cases {
            let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 100, height: 100))
            view.videoSize = CGSize(width: 200, height: 100)
            view.pictureFit = fit
            var events: [WireInputEvent] = []
            view.onEvents = { events += $0 }
            let finger = TestTouch(.direct)
            view.touchesBegan([finger], with: nil)
            view.touchesEnded([finger], with: nil)
            if let expected {
                XCTAssertEqual(events.map(\.phase), [0, 2])
                XCTAssertTrue(events.allSatisfy { $0.xNorm == expected.x && $0.yNorm == expected.y })
            } else {
                XCTAssertTrue(events.isEmpty, "a touch on the letterbox must never click")
            }
        }
    }

    func testSwitchingFitOrFillCancelsAStroke() {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 100, height: 100))
        view.videoSize = CGSize(width: 200, height: 100)
        view.nativePenSupported = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = TestTouch(.pencil)
        view.touchesBegan([tip], with: nil)
        view.pictureFit = .fill
        tip.sampleTime = 2
        view.touchesMoved([tip], with: nil)
        XCTAssertEqual(events.map(\.phase), [0, 3])
        // Setting the same mode again changes nothing.
        view.touchesBegan([tip], with: nil)
        view.pictureFit = .fill
        XCTAssertEqual(events.map(\.phase), [0, 3, 0])
    }

    // A change of Fit or Fill, or a rotation, moves the picture under every
    // contact. Each gesture ends where it was, and the contacts still down
    // send nothing more, so no click or drag lands on a point mapped the old
    // way. The customer's iPad Air (1180 × 820 pt) showing a 16:9 PC: x = 10
    // pt is 555 in Fit, but Fill's visible range starts at 6244.
    private func iPadAirRelay() -> RelayTouchUIView {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 1180, height: 820))
        view.videoSize = CGSize(width: 1920, height: 1080)
        view.layoutIfNeeded()
        return view
    }

    func testAPendingTapHeldAcrossASwitchDoesNotClick() {
        let view = iPadAirRelay()
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let finger = TestTouch(.direct)
        finger.point = CGPoint(x: 10, y: 410)
        view.touchesBegan([finger], with: nil)
        view.pictureFit = .fill
        view.touchesEnded([finger], with: nil)
        XCTAssertTrue(events.isEmpty, "a tap held across the switch clicked at \(events.map(\.xNorm))")
    }

    func testADragHeldAcrossASwitchEndsWhereItWas() {
        for change in ["fill", "rotation"] {
            let view = iPadAirRelay()
            var events: [WireInputEvent] = []
            view.onEvents = { events += $0 }
            let finger = TestTouch(.direct)
            finger.point = CGPoint(x: 10, y: 410)
            view.touchesBegan([finger], with: nil)
            finger.point = CGPoint(x: 100, y: 410)
            view.touchesMoved([finger], with: nil)
            XCTAssertEqual(events.map(\.phase), [0, 1], change)
            if change == "fill" {
                view.pictureFit = .fill
            } else {
                view.frame.size = CGSize(width: 820, height: 1180)
                view.layoutIfNeeded()
            }
            finger.point = CGPoint(x: 200, y: 400)
            view.touchesMoved([finger], with: nil)
            view.touchesEnded([finger], with: nil)
            // Released once, at the last point sent, with nothing after it.
            XCTAssertEqual(events.map(\.phase), [0, 1, 3], change)
            XCTAssertEqual(events.map(\.kind), [0, 0, 0], change)
            XCTAssertEqual(events.last?.xNorm, 5554, change)
            XCTAssertEqual(events.last?.yNorm, 32768, change)
        }
    }

    func testAFallbackPencilStrokeAcrossASwitchEndsWhereItWas() {
        let view = iPadAirRelay()
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = TestTouch(.pencil)
        tip.point = CGPoint(x: 10, y: 410)
        view.touchesBegan([tip], with: nil)
        view.pictureFit = .fill
        tip.point = CGPoint(x: 200, y: 400)
        view.touchesMoved([tip], with: nil)
        view.touchesEnded([tip], with: nil)
        XCTAssertEqual(events.map(\.phase), [0, 3])
        XCTAssertEqual(events.map(\.kind), [1, 1])
        XCTAssertTrue(events.allSatisfy { $0.xNorm == 555 && $0.yNorm == 32768 }, "\(events.map(\.xNorm))")
    }

    func testAPointerDragAcrossASwitchReleasesWhereItWas() {
        let view = iPadAirRelay()
        view.keyboardSupported = true
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let pointer = TestTouch(.indirectPointer)
        pointer.point = CGPoint(x: 10, y: 410)
        view.touchesBegan([pointer], with: nil)
        view.pictureFit = .fill
        // The button is up: what follows only moves the cursor, mapped the new way.
        pointer.point = CGPoint(x: 200, y: 400)
        view.touchesMoved([pointer], with: nil)
        view.touchesEnded([pointer], with: nil)
        XCTAssertEqual(events.prefix(2).map(\.phase), [0, 2])
        XCTAssertTrue(events.prefix(2).allSatisfy { $0.kind == 2 && $0.buttons == 1 && $0.xNorm == 555 })
        XCTAssertTrue(events.dropFirst(2).allSatisfy { $0.buttons == 0 && $0.phase == 1 }, "\(events)")
    }

    /// The PC changed resolution mid-drag: a new shape moves the picture, and
    /// a new size changes what each point means on the PC.
    func testADragHeldAcrossAResolutionChangeEndsWhereItWas() {
        for size in [CGSize(width: 2560, height: 1080), CGSize(width: 3840, height: 2160)] {
            let view = iPadAirRelay()
            var events: [WireInputEvent] = []
            view.onEvents = { events += $0 }
            let finger = TestTouch(.direct)
            finger.point = CGPoint(x: 10, y: 410)
            view.touchesBegan([finger], with: nil)
            finger.point = CGPoint(x: 100, y: 410)
            view.touchesMoved([finger], with: nil)
            view.videoSize = size
            finger.point = CGPoint(x: 200, y: 400)
            view.touchesMoved([finger], with: nil)
            view.touchesEnded([finger], with: nil)
            XCTAssertEqual(events.map(\.phase), [0, 1, 3], "\(size)")
            XCTAssertEqual(events.last?.xNorm, 5554, "\(size)")
            XCTAssertEqual(events.last?.yNorm, 32768, "\(size)")
        }
    }

    /// A switch cancels the stroke, but the tip is still on the glass: a palm
    /// that lands before it lifts must still be ignored, not click the PC.
    func testPalmsStayIgnoredUntilACancelledTipLifts() {
        for change in ["fill", "rotation", "resolution"] {
            let view = iPadAirRelay()
            view.nativePenSupported = true
            var events: [WireInputEvent] = []
            view.onEvents = { events += $0 }
            let tip = TestTouch(.pencil)
            tip.position = CGPoint(x: 10, y: 410)
            view.touchesBegan([tip], with: nil)
            switch change {
            case "fill": view.pictureFit = .fill
            case "rotation":
                view.frame.size = CGSize(width: 820, height: 1180)
                view.layoutIfNeeded()
            default: view.videoSize = CGSize(width: 2560, height: 1080)
            }
            XCTAssertEqual(events.map(\.phase), [0, 3], change)
            let palm = TestTouch(.direct)
            palm.point = CGPoint(x: 400, y: 600)
            view.touchesBegan([palm], with: nil)
            palm.point = CGPoint(x: 500, y: 600)
            view.touchesMoved([palm], with: nil)
            view.touchesEnded([palm], with: nil)
            tip.sampleTime = 2
            view.touchesMoved([tip], with: nil)
            view.touchesEnded([tip], with: nil)
            XCTAssertEqual(events.map(\.phase), [0, 3], "\(change): \(events.dropFirst(2))")
            // Once the tip is up, a finger is a finger again. (400, 600) is on
            // the picture in both orientations.
            let finger = TestTouch(.direct)
            finger.point = CGPoint(x: 400, y: 600)
            view.touchesBegan([finger], with: nil)
            view.touchesEnded([finger], with: nil)
            XCTAssertEqual(events.dropFirst(2).map(\.phase), [0, 2], change)
            XCTAssertEqual(events.dropFirst(2).map(\.kind), [0, 0], change)
        }
    }

    func testATwoFingerScrollAcrossASwitchStops() {
        let view = iPadAirRelay()
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let first = TestTouch(.direct), second = TestTouch(.direct)
        first.point = CGPoint(x: 500, y: 300)
        second.point = CGPoint(x: 600, y: 300)
        let event = TestEvent([first, second])
        view.touchesBegan([first, second], with: event)
        first.point.y = 400
        second.point.y = 400
        view.touchesMoved([first, second], with: event)
        XCTAssertEqual(events.map(\.kind), [3], "the scroll did not start")
        view.pictureFit = .fill
        // Past the relay's move interval, so a stale scroll would be sent.
        Thread.sleep(forTimeInterval: 0.01)
        first.point.y = 450
        second.point.y = 450
        view.touchesMoved([first, second], with: event)
        view.touchesEnded([first, second], with: event)
        XCTAssertEqual(events.count, 1, "scrolled after the switch: \(events.dropFirst())")
    }
}

private final class TestEvent: UIEvent {
    let touches: Set<UITouch>
    init(_ touches: Set<UITouch>) { self.touches = touches; super.init() }
    override var allTouches: Set<UITouch>? { touches }
}

private final class TestTouch: UITouch {
    let touchType: UITouch.TouchType
    var sampleTime: TimeInterval = 1
    var position = CGPoint(x: 50, y: 50)
    var point = CGPoint(x: 1, y: 1)
    init(_ type: UITouch.TouchType) { touchType = type; super.init() }
    override var type: UITouch.TouchType { touchType }
    override var timestamp: TimeInterval { sampleTime }
    override var force: CGFloat { 2 }
    override var maximumPossibleForce: CGFloat { 4 }
    override var altitudeAngle: CGFloat { .pi / 4 }
    override func azimuthAngle(in view: UIView?) -> CGFloat { 0 }
    override func location(in view: UIView?) -> CGPoint { point }
    override func preciseLocation(in view: UIView?) -> CGPoint { position }
}
