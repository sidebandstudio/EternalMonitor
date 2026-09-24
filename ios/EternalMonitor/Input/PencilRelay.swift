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

    init(point: TouchRelayMachine.Point, force: CGFloat, maximumForce: CGFloat,
         altitude: CGFloat, azimuth: CGFloat, timeUs: UInt64) {
        self.point = point
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

/// Independent from finger gestures, which must never end a Pencil stroke.
struct PencilRelayMachine {
    private(set) var isDown = false
    private var last: PencilReading?

    mutating func began(_ reading: PencilReading) -> [WireInputEvent] {
        let releases = cancel()
        isDown = true
        last = reading
        return releases + [reading.event(phase: 0, contact: true)]
    }

    mutating func moved(_ reading: PencilReading) -> [WireInputEvent] {
        guard isDown, reading.timeUs > (last?.timeUs ?? 0) else { return [] }
        last = reading
        return [reading.event(phase: 1, contact: true)]
    }

    mutating func ended(_ reading: PencilReading?, cancelled: Bool) -> [WireInputEvent] {
        guard isDown, let sample = reading ?? last else { return [] }
        isDown = false
        last = nil
        return [sample.event(phase: cancelled ? 3 : 2, contact: true)]
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
