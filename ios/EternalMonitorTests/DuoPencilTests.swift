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
                         azimuth: CGFloat = 0, time: UInt64 = 1, final: Bool = true) -> PencilReading {
        PencilReading(point: .init(x: 100, y: 200), force: force, maximumForce: maximumForce,
            altitude: altitude, azimuth: azimuth, timeUs: time, forceIsFinal: final)
    }

    /// One stroke, each sample at the next timestamp; the pressures sent.
    private func stroke(_ pen: inout PencilRelayMachine, _ forces: [CGFloat], from time: UInt64 = 1,
                        final: Bool = true) -> [UInt16] {
        guard let first = forces.first else { return [] }
        var events = pen.began(reading(force: first, time: time, final: final))
        for (i, force) in forces.dropFirst().enumerated() {
            events += pen.moved(reading(force: force, time: time + UInt64(i) + 1, final: final))
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
        let connections: [(Int, [UInt16])] = [(1, DeviceNoun.isPhone ? [500, 500, 0] : [0, 0, 0]), (2, [500, 500, 0])]
        for (connection, expected) in connections {
            let view = relay()
            var events: [WireInputEvent] = []
            view.onEvents = { events += $0 }
            let tip = Tip(force: 0)
            view.touchesBegan([tip], with: nil)
            tip.sampleTime = 2
            view.touchesMoved([tip], with: nil)
            view.touchesEnded([tip], with: nil)
            XCTAssertEqual(events.map(\.phase), [0, 1, 2], "connection \(connection)")
            XCTAssertEqual(events.map(\.pressureX1000), expected, "connection \(connection)")
            XCTAssertTrue(events.allSatisfy { $0.kind == 1 && $0.tiltX == 45 })
        }
        XCTAssertEqual(PencilPressure.session.sensor, .none)
    }

    /// Once a Pencil has measured any force, a 0 is a 0. A Pencil thought to
    /// have no sensor that measures force is believed at once.
    func testOneMeasuredForceMeansPressureForGood() {
        var pen = PencilRelayMachine()
        XCTAssertEqual(stroke(&pen, [2]), [500, 0])
        XCTAssertEqual(stroke(&pen, [0, 0, 0], from: 10), [0, 0, 0, 0])
        XCTAssertEqual(pen.pressure.sensor, .pressure)

        var phone = PencilRelayMachine(pressure: PencilPressure(sensor: .none))
        XCTAssertEqual(stroke(&phone, [0, 1, 0]), [500, 250, 0, 0])
        XCTAssertEqual(phone.pressure.sensor, .pressure)
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
