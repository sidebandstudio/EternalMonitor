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

    init(point: TouchRelayMachine.Point, force: CGFloat, maximumForce: CGFloat,
         altitude: CGFloat, azimuth: CGFloat, timeUs: UInt64, forceIsFinal: Bool = true) {
        self.point = point
        self.forceIsFinal = forceIsFinal
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
/// is learned from what the Pencil reports. Any measured force means it senses
/// pressure, for good. A whole stroke that measured none means it doesn't, and
/// then contact draws at a fixed 500, because Windows apps draw nothing at 0.
/// Only final readings count: UIKit can send an estimated force first.
struct PencilPressure: Equatable {
    enum Sensor {
        case unknown
        case pressure
        case none
    }

    private(set) var sensor: Sensor
    private var strokeHadFinalReading = false

    init(sensor: Sensor = .unknown) {
        self.sensor = sensor
    }

    /// iPhone supports only the USB-C Pencil, so it starts without pressure.
    /// iPad starts as it always has: the measured force, 0 included.
    static var deviceDefault: PencilPressure {
        PencilPressure(sensor: DeviceNoun.isPhone ? .none : .unknown)
    }

    /// What this app session has learned, so a reconnect needn't learn again.
    @MainActor static var session = deviceDefault

    mutating func strokeBegan() {
        strokeHadFinalReading = false
    }

    mutating func observe(_ reading: PencilReading) {
        guard reading.forceIsFinal else { return }
        strokeHadFinalReading = true
        if reading.pressure > 0 { sensor = .pressure }
    }

    mutating func strokeEnded(completed: Bool) {
        if completed && sensor == .unknown && strokeHadFinalReading { sensor = .none }
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
