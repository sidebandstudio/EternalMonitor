import CoreGraphics
import Foundation

/// A measured Pencil sample. No predicted points are sent to a remote app:
/// Windows cannot retract an already-painted brush stroke.
struct PencilReading {
    var point: TouchRelayMachine.Point
    var pressure: UInt16
    var tiltX: Int16
    var tiltY: Int16
    var timeUs: UInt64
    /// False while UIKit still expects to update this sample's force: an
    /// estimated 0 at touch-down says nothing about the Pencil.
    var forceIsFinal: Bool
    /// The sample as the tip lifts. Every Pencil's force falls there, so it
    /// says nothing about the sensor either.
    var atLift: Bool
    /// What UIKit reported, for the Pencil diagnostics line.
    var force: CGFloat
    var maximumForce: CGFloat

    init(point: TouchRelayMachine.Point, force: CGFloat, maximumForce: CGFloat,
         altitude: CGFloat, azimuth: CGFloat, timeUs: UInt64, forceIsFinal: Bool = true, atLift: Bool = false) {
        self.point = point
        self.forceIsFinal = forceIsFinal
        self.atLift = atLift
        self.force = force
        self.maximumForce = maximumForce
        // A device without a pressure sensor still draws at a fixed pressure.
        let normalized = maximumForce.isFinite && maximumForce > 0 && force.isFinite
            ? min(1, max(0, force / maximumForce)) : 0.5
        pressure = UInt16((normalized * 1000).rounded())
        // UIKit's azimuth is already in this view's coordinate system. Project
        // the shaft onto XZ/YZ planes, matching Windows' signed tilt axes.
        if altitude.isFinite && azimuth.isFinite {
            let elevation = min(CGFloat.pi / 2, max(0, altitude))
            let z = sin(elevation)
            let radial = cos(elevation)
            tiltX = Int16((atan2(cos(azimuth) * radial, z) * 180 / .pi).rounded())
            tiltY = Int16((atan2(sin(azimuth) * radial, z) * 180 / .pi).rounded())
        } else {
            tiltX = 0
            tiltY = 0
        }
        self.timeUs = timeUs
    }

    func event(phase: UInt8, contact: Bool) -> WireInputEvent {
        WireInputEvent(inputVer: 2, kind: 1, phase: phase, buttons: contact ? 1 : 0,
            eventId: 0, xNorm: point.x, yNorm: point.y,
            pressureX1000: contact && phase < 2 ? pressure : 0,
            clientTimeUs: timeUs, tiltX: tiltX, tiltY: tiltY)
    }
}

/// Whether the Pencil senses pressure. UIKit names neither the Pencil model
/// nor its sensors, and the USB-C Pencil has none on iPad or iPhone, so this
/// is learned from what the Pencil reports. Without a sensor, contact draws at
/// a fixed 500, because Windows apps draw nothing at 0.
///
/// Only final readings away from the lift count: UIKit can send an estimated
/// force first, and every force falls as the tip lifts. On iPad one measured
/// force means a sensor, for good, as iPad has always sent what it measures.
/// Every other lesson takes a completed stroke: a palm, a rotation or the next
/// touch-down can cancel one on readings no finished stroke would send.
/// "No sensor" takes a stroke of several final readings, all 0, with no
/// estimated force above 0 either, so a short tap teaches nothing. iPhone takes
/// only the USB-C Pencil and starts without pressure; it believes a sensor only
/// from forces that vary within a stroke, so a small constant still draws at 500.
struct PencilPressure: Equatable {
    enum Sensor {
        case unknown
        case pressure
        case none
    }

    /// What one stroke reported.
    struct Stroke: Equatable {
        var maximumForce: CGFloat = 0
        var finalReadings = 0
        var forces = Set<CGFloat>()
        var lowest: UInt16 = .max
        var highest: UInt16 = 0
        var estimated = false
        var estimatedForce = false
    }

    static let zeroReadingsForNoSensor = 3
    /// The spread of final pressures, out of 1000, that proves a sensor on iPhone.
    static let variationForPressure: UInt16 = 50

    private(set) var sensor: Sensor
    /// iPhone's rule: a sensor only from forces that vary.
    let needsVariation: Bool
    private(set) var stroke = Stroke()

    init(sensor: Sensor = .unknown, needsVariation: Bool = false) {
        self.sensor = sensor
        self.needsVariation = needsVariation
    }

    /// iPhone supports only the USB-C Pencil, so it starts without pressure.
    /// iPad starts as it always has: the measured force, 0 included.
    static var deviceDefault: PencilPressure {
        DeviceNoun.isPhone ? PencilPressure(sensor: .none, needsVariation: true) : PencilPressure()
    }

    /// What this app session has learned, so a reconnect needn't learn again.
    @MainActor static var session = deviceDefault

    mutating func strokeBegan() {
        stroke = Stroke()
    }

    mutating func observe(_ reading: PencilReading) {
        stroke.maximumForce = reading.maximumForce
        guard !reading.atLift else { return }
        guard reading.forceIsFinal else {
            stroke.estimated = true
            if reading.pressure > 0 { stroke.estimatedForce = true }
            return
        }
        stroke.finalReadings += 1
        if reading.force.isFinite { stroke.forces.insert(reading.force) }
        stroke.lowest = min(stroke.lowest, reading.pressure)
        stroke.highest = max(stroke.highest, reading.pressure)
        if !needsVariation && reading.pressure > 0 { sensor = .pressure }
    }

    mutating func strokeEnded(completed: Bool) {
        guard completed else { return }
        if needsVariation && stroke.finalReadings >= 2
            && stroke.highest - stroke.lowest >= Self.variationForPressure {
            sensor = .pressure
        }
        if sensor == .unknown && stroke.finalReadings >= Self.zeroReadingsForNoSensor
            && stroke.highest == 0 && !stroke.estimatedForce {
            sensor = .none
        }
    }

    /// The diagnostics line for the stroke that just ended: forces and counts
    /// only, never where the Pencil was.
    var strokeSummary: String {
        let forces = stroke.forces
        func value(_ force: CGFloat?) -> String { force.map { String(format: "%.4f", $0) } ?? "none" }
        return "max_force=\(value(stroke.maximumForce)) final=\(stroke.finalReadings) "
            + "min=\(value(forces.min())) max=\(value(forces.max())) distinct=\(forces.count) "
            + "estimated=\(stroke.estimated) estimated_force=\(stroke.estimatedForce) learned=\(sensor)"
    }

    /// Strokes logged this session. The first few show what the hardware
    /// reports; after that the lines would only repeat.
    @MainActor static var strokesLogged = 0
    static let strokesToLog = 8

    @MainActor static func logStroke(_ pressure: PencilPressure, to diagnostics: SupportDiagnostics = .shared) {
        guard strokesLogged < strokesToLog else { return }
        strokesLogged += 1
        diagnostics.record("info", "pencil", "stroke=\(strokesLogged) \(pressure.strokeSummary)")
    }

    func pressure(for reading: PencilReading) -> UInt16 {
        sensor == .none ? 500 : reading.pressure
    }
}

/// Independent from finger gestures, which must never end a Pencil stroke.
struct PencilRelayMachine {
    private(set) var isDown = false
    private var last: PencilReading?
    /// The view keeps this for the app session; see `PencilPressure.session`.
    var pressure: PencilPressure

    init(pressure: PencilPressure = PencilPressure()) {
        self.pressure = pressure
    }

    mutating func began(_ reading: PencilReading) -> [WireInputEvent] {
        let releases = cancel()
        isDown = true
        pressure.strokeBegan()
        return releases + [contact(reading, phase: 0)]
    }

    mutating func moved(_ reading: PencilReading) -> [WireInputEvent] {
        guard isDown, reading.timeUs > (last?.timeUs ?? 0) else { return [] }
        return [contact(reading, phase: 1)]
    }

    mutating func ended(_ reading: PencilReading?, cancelled: Bool) -> [WireInputEvent] {
        guard isDown, let sample = reading ?? last else { return [] }
        isDown = false
        last = nil
        pressure.strokeEnded(completed: !cancelled)
        return [sample.event(phase: cancelled ? 3 : 2, contact: true)]
    }

    private mutating func contact(_ reading: PencilReading, phase: UInt8) -> WireInputEvent {
        pressure.observe(reading)
        var sample = reading
        sample.pressure = pressure.pressure(for: reading)
        last = sample
        return sample.event(phase: phase, contact: true)
    }

    mutating func hover(_ reading: PencilReading?) -> [WireInputEvent] {
        guard !isDown else { return [] }
        if let reading {
            last = reading
            return [reading.event(phase: 1, contact: false)]
        }
        guard let sample = last else { return [] }
        last = nil
        return [sample.event(phase: 2, contact: false)]
    }

    mutating func cancel() -> [WireInputEvent] {
        if isDown { return ended(nil, cancelled: true) }
        return hover(nil)
    }
}

/// The preset is fixed at connection time, like input/audio negotiation.
struct DrawingProfile {
    let enabled: Bool
    let isUSB: Bool

    func preferredFPS(_ preference: Int, maximum: Int) -> UInt8 {
        UInt8(clamping: enabled && isUSB ? min(120, maximum) : preference)
    }
}
