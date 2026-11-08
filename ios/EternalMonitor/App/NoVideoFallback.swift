import Foundation

/// A PC accepted the session and sends video, and no picture comes of it:
/// the path to this address carries the small control datagrams but drops
/// the large media ones (its MTU, fragments, or a filter). The iPad first asks
/// for smaller media datagrams, then tries the PC's other address. Only a
/// PC that offers `HelloAck.hostCapMediaSize` takes part: its heartbeats
/// count the frames it sent, so a PC that sends nothing yet is never taken
/// for a path that loses everything.
struct NoVideoFallback: Equatable {
    enum Step: Equatable {
        case smallerPackets(UInt16)
        case otherAddress
    }

    /// The media datagram sizes asked for, in turn.
    static let sizes: [UInt16] = [1200, KeyframeRequest.minMediaDgram]

    /// How long video the PC says it sent may go without a picture before
    /// the next step.
    let wait: TimeInterval
    private var sizesAsked = 0
    private var finished = false
    /// The latest count of frames the PC sent, and the count when the last
    /// step was taken: only frames sent after a step count against it.
    private var framesSent: UInt32 = 0
    private var framesBeforeStep: UInt32 = 0
    /// When a heartbeat first said frames went out after the last step.
    private var sendingSince: TimeInterval?

    init(wait: TimeInterval) {
        self.wait = wait
    }

    /// A heartbeat said the PC has sent `frames` this session.
    mutating func heard(framesSent frames: UInt32, at now: TimeInterval) {
        framesSent = max(framesSent, frames)
        if framesSent > framesBeforeStep, sendingSince == nil { sendingSince = now }
    }

    /// The step to take at `now`, while there is no picture; nil while the
    /// PC sends nothing, or within `wait` of its sending.
    mutating func next(at now: TimeInterval) -> Step? {
        guard !finished, let since = sendingSince, now - since >= wait else { return nil }
        sendingSince = nil
        framesBeforeStep = framesSent
        guard sizesAsked < Self.sizes.count else {
            finished = true
            return .otherAddress
        }
        sizesAsked += 1
        return .smallerPackets(Self.sizes[sizesAsked - 1])
    }
}

/// Where a PC's video didn't arrive, and the address of the same PC where it
/// did, so the next connect to the first goes to the second. Kept in
/// UserDefaults as "host:port" pairs.
enum NoVideoPaths {
    private static let key = "noVideoPaths"
    private static let kept = 16

    struct Address: Codable, Equatable {
        var host: String
        var port: UInt16
    }

    private struct Entry: Codable {
        var from: Address
        var to: Address
    }

    /// The address where the PC at `host:port` showed video instead.
    static func better(than host: String, port: UInt16) -> Address? {
        load().last { $0.from == Address(host: host, port: port) }?.to
    }

    static func remember(from: Address, to: Address) {
        var entries = load().filter { $0.from != from && $0.from != to }
        entries.append(Entry(from: from, to: to))
        save(Array(entries.suffix(kept)))
    }

    static func forgetAll() {
        UserDefaults.standard.removeObject(forKey: key)
    }

    private static func load() -> [Entry] {
        guard let data = UserDefaults.standard.data(forKey: key) else { return [] }
        return (try? JSONDecoder().decode([Entry].self, from: data)) ?? []
    }

    private static func save(_ entries: [Entry]) {
        guard let data = try? JSONEncoder().encode(entries) else { return }
        UserDefaults.standard.set(data, forKey: key)
    }
}
