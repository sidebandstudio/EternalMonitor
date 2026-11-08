import Foundation

/// A PC that turns this iPad down over the cable dials it again about a
/// second after each refusal, so the refusal would repeat for as long as the
/// cable stays in. Until something changes, the iPad stops answering the
/// cable: after Busy for a wait that grows with each Busy in a row, and after
/// a version refusal until the user taps Try again or Connect USB.
struct CableHold: Equatable {
    enum Reason: Equatable {
        case busy
        case version
    }

    let reason: Reason
    /// When the iPad answers the cable again; nil waits for the user.
    let until: Date?

    /// The wait after `count` Busy refusals in a row: 30 s, doubling each
    /// time, at most 5 minutes.
    static func busySeconds(_ count: Int) -> Double {
        min(30 * pow(2, Double(min(max(count, 1), 16) - 1)), 300)
    }

    /// Whether the cable still goes unanswered at `now`.
    func holds(at now: Date) -> Bool {
        until.map { now < $0 } ?? true
    }
}
