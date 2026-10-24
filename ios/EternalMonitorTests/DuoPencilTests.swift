import XCTest
@testable import EternalMonitor

/// Apple Pencil on iPhone Duo is the USB-C Pencil: tilt, but no pressure and
/// no hover. iPads take it too. Windows apps draw nothing at pressure 0, so a
/// Pencil known to have no sensor draws at 500, while a Pencil that senses
/// pressure always sends what it measures, however light.
@MainActor
final class DuoPencilTests: XCTestCase {
    private var savedSession = PencilPressure()

    override func setUp() {
        savedSession = PencilPressure.session
    }

    override func tearDown() {
        PencilPressure.session = savedSession
    }

    private func reading(force: CGFloat, maximumForce: CGFloat = 4, altitude: CGFloat = .pi / 4,
                         azimuth: CGFloat = 0, time: UInt64 = 1, final: Bool = true,
                         lift: Bool = false) -> PencilReading {
        PencilReading(point: .init(x: 100, y: 200), force: force, maximumForce: maximumForce,
            altitude: altitude, azimuth: azimuth, timeUs: time, forceIsFinal: final, atLift: lift)
    }

    private let phone = PencilPressure(sensor: .none, needsVariation: true)

    /// One stroke, each sample at the next timestamp; the pressures sent.
    private func stroke(_ pen: inout PencilRelayMachine, _ forces: [CGFloat], from time: UInt64 = 1,
                        maximumForce: CGFloat = 4, final: Bool = true) -> [UInt16] {
        guard let first = forces.first else { return [] }
        var events = pen.began(reading(force: first, maximumForce: maximumForce, time: time, final: final))
        for (i, force) in forces.dropFirst().enumerated() {
            events += pen.moved(reading(force: force, maximumForce: maximumForce, time: time + UInt64(i) + 1,
                                        final: final))
        }
        events += pen.ended(nil, cancelled: false)
        return events.map(\.pressureX1000)
    }

    /// Sol's light stroke on a Pencil that senses pressure: what it measures,
    /// rounded, and never the fixed 500.
    func testALightStrokeOnAPressurePencilIsSentAsMeasured() {
        var pen = PencilRelayMachine()
        XCTAssertEqual(stroke(&pen, [0, 0.004, 0.0019, 0.002]), [0, 1, 0, 1, 0])
        XCTAssertEqual(pen.pressure.sensor, .pressure)
        XCTAssertEqual(stroke(&pen, [1, 3, 4, 100], from: 10), [250, 750, 1000, 1000, 0])
    }

    /// UIKit can send an estimated force at touch-down and the real one later.
    /// An estimated 0 is neither half pressure nor proof of a missing sensor.
    func testAnEstimatedZeroAtTouchDownIsNotHalfPressure() {
        var pen = PencilRelayMachine()
        XCTAssertEqual(pen.began(reading(force: 0, time: 1, final: false)).map(\.pressureX1000), [0])
        XCTAssertEqual(pen.moved(reading(force: 2, time: 2)).map(\.pressureX1000), [500])
        _ = pen.ended(nil, cancelled: false)
        var estimatedOnly = PencilRelayMachine()
        XCTAssertEqual(stroke(&estimatedOnly, [0, 0], final: false), [0, 0, 0])
        XCTAssertEqual(estimatedOnly.pressure.sensor, .unknown)

        // The same through the view, on iPad's starting assumption.
        PencilPressure.session = PencilPressure()
        let view = relay()
        var events: [WireInputEvent] = []
        view.onEvents = { events += $0 }
        let tip = Tip(force: 0, estimated: true)
        view.touchesBegan([tip], with: nil)
        XCTAssertEqual(events.map(\.pressureX1000), [0])
    }

    /// A Pencil known to have no sensor draws at 500 from the first sample:
    /// at once on iPhone, after one stroke that measured nothing on iPad.
    func testAPencilWithoutPressureDrawsAtHalfForTheWholeStroke() {
        var known = PencilRelayMachine(pressure: PencilPressure(sensor: .none))
        XCTAssertEqual(stroke(&known, [0, 0, 0]), [500, 500, 500, 0])

        var learning = PencilRelayMachine()
        XCTAssertEqual(stroke(&learning, [0, 0, 0]), [0, 0, 0, 0])
        XCTAssertEqual(learning.pressure.sensor, .none)
        XCTAssertEqual(stroke(&learning, [0, 0, 0], from: 10), [500, 500, 500, 0])
        // A stroke of two readings is too short to judge.
        var short = PencilRelayMachine()
        XCTAssertEqual(stroke(&short, [0, 0]), [0, 0, 0])
        XCTAssertEqual(short.pressure.sensor, .unknown)
        // A cancelled stroke teaches nothing.
        var cancelled = PencilRelayMachine()
        _ = cancelled.began(reading(force: 0, time: 1))
        _ = cancelled.ended(nil, cancelled: true)
        XCTAssertEqual(cancelled.pressure.sensor, .unknown)
    }

    /// The view keeps what it learned for the app session, so the next
    /// connection's view draws at 500 from its first sample.
    func testWhatTheViewLearnsOutlastsTheConnection() {
        PencilPressure.session = .deviceDefault
        XCTAssertEqual(PencilPressure.session.sensor, DeviceNoun.isPhone ? .none : .unknown)
        let connections: [(Int, [UInt16])] = [(1, DeviceNoun.isPhone ? [500, 500, 500, 0] : [0, 0, 0, 0]),
                                              (2, [500, 500, 500, 0])]
        for (connection, expected) in connections {
            let view = relay()
            var events: [WireInputEvent] = []
            view.onEvents = { events += $0 }
            // Three readings away from the lift: long enough to judge.
            let tip = Tip(force: 0)
            view.touchesBegan([tip], with: nil)
            for time in [2.0, 3.0] {
                tip.sampleTime = time
                view.touchesMoved([tip], with: nil)
            }
            view.touchesEnded([tip], with: nil)
            XCTAssertEqual(events.map(\.phase), [0, 1, 1, 2], "connection \(connection)")
            XCTAssertEqual(events.map(\.pressureX1000), expected, "connection \(connection)")
            XCTAssertTrue(events.allSatisfy { $0.kind == 1 && $0.tiltX == 45 })
        }
        XCTAssertEqual(PencilPressure.session.sensor, .none)
    }

    /// On iPad, once a Pencil has measured any force, a 0 is a 0.
    func testOneMeasuredForceMeansPressureForGoodOnIPad() {
        var pen = PencilRelayMachine()
        XCTAssertEqual(stroke(&pen, [2]), [500, 0])
        XCTAssertEqual(stroke(&pen, [0, 0, 0], from: 10), [0, 0, 0, 0])
        XCTAssertEqual(pen.pressure.sensor, .pressure)
    }

    /// The review's tap: an estimated force at touch-down, then a final 0 as
    /// the tip lifts. It must not teach "no sensor", or the next stroke's
    /// estimated samples would jump from what the Pencil measured to 500.
    func testAShortTapDoesNotTeachNoSensor() {
        PencilPressure.session = PencilPressure()
        let first = relay()
        first.onEvents = { _ in }
        let down = Tip(force: 0.2, estimated: true)
        first.touchesBegan([down], with: nil)
        let lift = Tip(force: 0)
        lift.sampleTime = 2
        first.touchesEnded([down], with: Coalesced(down, [lift]))
        XCTAssertEqual(PencilPressure.session.sensor, .unknown)

        let next = relay()
        var events: [WireInputEvent] = []
        next.onEvents = { events += $0 }
        let tip = Tip(force: 0.2, estimated: true)
        tip.sampleTime = 10
        next.touchesBegan([tip], with: nil)
        for time in [11.0, 12.0] {
            tip.sampleTime = time
            next.touchesMoved([tip], with: nil)
        }
        // 0.2 of 4.17, as main sends it.
        XCTAssertEqual(events.map(\.pressureX1000), [48, 48, 48])
    }

    /// Each mitigation alone keeps a stroke from teaching "no sensor".
    func testOnlyALongStrokeOfFinalZerosTeachesNoSensor() {
        // An estimated force above 0, even with three final zeros after it.
        var estimated = PencilRelayMachine()
        _ = estimated.began(reading(force: 0.8, time: 1, final: false))
        for time: UInt64 in 2...4 { _ = estimated.moved(reading(force: 0, time: time)) }
        _ = estimated.ended(nil, cancelled: false)
        XCTAssertEqual(estimated.pressure.sensor, .unknown)
        // Two final zeros and the lift's: the lift doesn't count.
        var lift = PencilRelayMachine()
        _ = lift.began(reading(force: 0, time: 1))
        _ = lift.moved(reading(force: 0, time: 2))
        _ = lift.moved(reading(force: 0, time: 3, lift: true))
        _ = lift.ended(nil, cancelled: false)
        XCTAssertEqual(lift.pressure.sensor, .unknown)
        // Three final zeros away from the lift.
        var long = PencilRelayMachine()
        _ = long.began(reading(force: 0, time: 1))
        for time: UInt64 in 2...3 { _ = long.moved(reading(force: 0, time: time)) }
        _ = long.moved(reading(force: 0, time: 4, lift: true))
        _ = long.ended(nil, cancelled: false)
        XCTAssertEqual(long.pressure.sensor, .none)
    }

    /// iPhone believes a sensor only from forces that vary within a stroke,
    /// and then from the next stroke on, so no stroke changes halfway.
    func testIPhoneLearnsPressureOnlyFromVariation() {
        // A small constant, as the USB-C Pencil might report: still 500.
        var constant = PencilRelayMachine(pressure: phone)
        XCTAssertEqual(stroke(&constant, [0.004, 0.004, 0.004, 0.004], maximumForce: 4.17), [500, 500, 500, 500, 0])
        XCTAssertEqual(constant.pressure.sensor, .none)
        XCTAssertEqual(stroke(&constant, [0.004, 0.004], from: 10, maximumForce: 4.17), [500, 500, 0])
        // Zero throughout: 500.
        var zero = PencilRelayMachine(pressure: phone)
        XCTAssertEqual(stroke(&zero, [0, 0, 0, 0]), [500, 500, 500, 500, 0])
        XCTAssertEqual(zero.pressure.sensor, .none)
        // A ramp is a sensor: the next stroke goes out as measured.
        var ramp = PencilRelayMachine(pressure: phone)
        XCTAssertEqual(stroke(&ramp, [0.4, 1, 2, 3]), [500, 500, 500, 500, 0])
        XCTAssertEqual(ramp.pressure.sensor, .pressure)
        XCTAssertEqual(stroke(&ramp, [1, 0, 3], from: 10), [250, 0, 750, 0])
        // Estimated forces never count as variation.
        var estimated = PencilRelayMachine(pressure: phone)
        XCTAssertEqual(stroke(&estimated, [0.4, 3, 0.4], final: false), [500, 500, 500, 0])
        XCTAssertEqual(estimated.pressure.sensor, .none)
    }

    /// Sol's noisy stroke, final readings of 1 and 51, teaches iPhone nothing
    /// when it is cancelled, so a constant 1 afterwards still draws at 500.
    func testACancelledStrokeTeachesIPhoneNothing() {
        var cancelled = PencilRelayMachine(pressure: phone)
        _ = cancelled.began(reading(force: 0.004, time: 1))
        _ = cancelled.moved(reading(force: 0.204, time: 2))
        XCTAssertEqual(cancelled.ended(nil, cancelled: true).map(\.phase), [3])
        XCTAssertEqual(cancelled.pressure.sensor, .none)
        XCTAssertEqual(stroke(&cancelled, [0.004, 0.004, 0.004], from: 10), [500, 500, 500, 0])
        // Ended by a second touch-down instead.
        var replaced = PencilRelayMachine(pressure: phone)
        _ = replaced.began(reading(force: 0.004, time: 1))
        _ = replaced.moved(reading(force: 0.204, time: 2))
        XCTAssertEqual(replaced.began(reading(force: 0.004, time: 3)).map(\.phase), [3, 0])
        XCTAssertEqual(replaced.pressure.sensor, .none)
        // A picture change cancels it in the view.
        PencilPressure.session = phone
        let view = relay()
        view.onEvents = { _ in }
        let light = Tip(force: 0.004)
        view.touchesBegan([light], with: nil)
        let heavier = Tip(force: 0.213)
        heavier.sampleTime = 2
        view.touchesMoved([light], with: Coalesced(light, [heavier]))
        view.pictureFit = .fill
        light.sampleTime = 3
        view.touchesEnded([light], with: nil)
        XCTAssertEqual(PencilPressure.session.sensor, .none)
    }

    /// A second touch-down while a stroke is down ends it, and what it taught
    /// is kept rather than replaced by the session's older lesson.
    func testATouchDownMidStrokeKeepsItsLesson() {
        PencilPressure.session = PencilPressure()
        let view = relay()
        view.onEvents = { _ in }
        let first = Tip(force: 2)
        view.touchesBegan([first], with: nil)
        let second = Tip(force: 0)
        second.sampleTime = 2
        view.touchesBegan([second], with: nil)
        for time in [3.0, 4.0] {
            second.sampleTime = time
            view.touchesMoved([second], with: nil)
        }
        view.touchesEnded([second], with: nil)
        XCTAssertEqual(PencilPressure.session.sensor, .pressure)
    }

    /// One line per stroke for the device test: forces and counts, no place.
    func testTheStrokeSummaryReportsWhatTheHardwareSent() {
        var pen = PencilRelayMachine()
        _ = pen.began(reading(force: 0.5, maximumForce: 4.17, time: 1, final: false))
        for (time, force) in [(2, 0.5), (3, 1.25), (4, 1.25)] as [(UInt64, CGFloat)] {
            _ = pen.moved(reading(force: force, maximumForce: 4.17, time: time))
        }
        _ = pen.moved(reading(force: 0, maximumForce: 4.17, time: 5, lift: true))
        _ = pen.ended(nil, cancelled: false)
        XCTAssertEqual(pen.pressure.strokeSummary,
                       "max_force=4.1700 final=3 min=0.5000 max=1.2500 distinct=2 "
                       + "estimated=true estimated_force=true learned=pressure")
        XCTAssertEqual(SupportDiagnostics.redact(pen.pressure.strokeSummary), pen.pressure.strokeSummary)
    }

    /// The first strokes of a session reach Diagnostics; later ones would repeat.
    func testOnlyTheFirstStrokesOfASessionAreLogged() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let diagnostics = SupportDiagnostics(directory: directory)
        let saved = PencilPressure.strokesLogged
        defer { PencilPressure.strokesLogged = saved }
        PencilPressure.strokesLogged = 0
        var pen = PencilRelayMachine(pressure: phone)
        for stroke in 0..<(PencilPressure.strokesToLog + 2) {
            _ = self.stroke(&pen, [0, 0, 0], from: UInt64(stroke * 10 + 1), maximumForce: 4.17)
            PencilPressure.logStroke(pen.pressure, to: diagnostics)
        }
        let report = await diagnostics.report()
        XCTAssertTrue(report.contains("[pencil] stroke=1 max_force=4.1700 final=3 min=0.0000 max=0.0000 distinct=1 "
                                      + "estimated=false estimated_force=false learned=none"), report)
        XCTAssertTrue(report.contains("[pencil] stroke=\(PencilPressure.strokesToLog) "))
        XCTAssertFalse(report.contains("[pencil] stroke=\(PencilPressure.strokesToLog + 1) "))
    }

    func testHoverSendsNoPressure() {
        for sensor in [PencilPressure.Sensor.unknown, .pressure, .none] {
            var pen = PencilRelayMachine(pressure: PencilPressure(sensor: sensor))
            // As TouchRelayView builds a hover sample.
            let hover = pen.hover(reading(force: 0, maximumForce: 1))
            XCTAssertEqual(hover.map(\.pressureX1000), [0], "\(sensor)")
            XCTAssertEqual(hover.map(\.buttons), [0], "\(sensor)")
            XCTAssertEqual(pen.hover(nil).map(\.pressureX1000), [0], "\(sensor)")
        }
    }

    func testTiltIsUnchangedWithoutPressure() {
        let cases: [(altitude: CGFloat, azimuth: CGFloat, x: Int16, y: Int16)] = [
            (.pi / 2, 0, 0, 0), (.pi / 4, 0, 45, 0), (.pi / 4, .pi, -45, 0),
            (.pi / 4, .pi / 2, 0, 45), (.pi / 3, -.pi / 2, 0, -30),
        ]
        for (altitude, azimuth, x, y) in cases {
            var pen = PencilRelayMachine(pressure: PencilPressure(sensor: .none))
            let event = pen.began(reading(force: 0, altitude: altitude, azimuth: azimuth))
            XCTAssertEqual(event.map(\.pressureX1000), [500])
            XCTAssertEqual(event.map(\.tiltX), [x], "\(altitude) \(azimuth)")
            XCTAssertEqual(event.map(\.tiltY), [y], "\(altitude) \(azimuth)")
            let pressed = reading(force: 2, altitude: altitude, azimuth: azimuth)
            XCTAssertEqual(pressed.tiltX, x)
            XCTAssertEqual(pressed.tiltY, y)
        }
    }

    private func relay() -> RelayTouchUIView {
        let view = RelayTouchUIView(frame: CGRect(x: 0, y: 0, width: 466, height: 678))
        view.videoSize = CGSize(width: 1920, height: 1080)
        view.nativePenSupported = true
        return view
    }
}

/// An event whose coalesced samples for one touch are given.
private final class Coalesced: UIEvent {
    let touch: UITouch
    let samples: [UITouch]
    init(_ touch: UITouch, _ samples: [UITouch]) { self.touch = touch; self.samples = samples; super.init() }
    override var allTouches: Set<UITouch>? { [touch] }
    override func coalescedTouches(for touch: UITouch) -> [UITouch]? { touch === self.touch ? samples : nil }
}

/// A Pencil tip with a maximum force, as the USB-C Pencil might report itself.
private final class Tip: UITouch {
    let tipForce: CGFloat
    let estimated: Bool
    var sampleTime: TimeInterval = 1
    init(force: CGFloat, estimated: Bool = false) {
        tipForce = force
        self.estimated = estimated
        super.init()
    }
    override var type: UITouch.TouchType { .pencil }
    override var timestamp: TimeInterval { sampleTime }
    override var force: CGFloat { tipForce }
    override var maximumPossibleForce: CGFloat { 4.17 }
    override var estimatedPropertiesExpectingUpdates: UITouch.Properties { estimated ? [.force] : [] }
    override var altitudeAngle: CGFloat { .pi / 4 }
    override func azimuthAngle(in view: UIView?) -> CGFloat { 0 }
    override func location(in view: UIView?) -> CGPoint { CGPoint(x: 233, y: 339) }
    override func preciseLocation(in view: UIView?) -> CGPoint { CGPoint(x: 233, y: 339) }
}
