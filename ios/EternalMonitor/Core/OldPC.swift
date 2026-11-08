import Foundation

/// Tells a PC whose EternalMonitor is older than 0.3.4 from a current one,
/// using only what PCs already send.
///
/// - Bonjour: every PC puts its version in the TXT record's `version`.
///   Only a plain release version counts; a beta's or a missing one doesn't.
/// - HELLO_ACK: every 0.3.4 and later PC sets `hostCapContiguousFrameSeq` in
///   every answer, whatever its hardware or settings, and no earlier PC ever
///   set that bit. This covers PCs reached over USB or by address.
///
/// A PC is old only when nothing it sent says it is current.
enum OldPC {
    static let current = AppVersion("0.3.4")!

    /// True for an old PC, false for a current one, nil when it can't tell.
    static func isOld(announced: String?, hostCaps: UInt16?) -> Bool? {
        if let hostCaps, hostCaps & HelloAck.hostCapContiguousFrameSeq != 0 { return false }
        if let version = announced.flatMap(AppVersion.init) { return version < current }
        return hostCaps == nil ? nil : true
    }

    /// The name a PC gives itself, from its Bonjour service name.
    static func pcName(bonjour name: String) -> String {
        let prefix = "EternalMonitor (", suffix = ")"
        guard name.hasPrefix(prefix), name.hasSuffix(suffix), name.count > prefix.count + suffix.count else { return name }
        return String(name.dropFirst(prefix.count).dropLast(suffix.count))
    }
}

/// The Connect screen's card asking to update an old PC. It shows at most
/// once per PC every three days, only on the Connect screen, and goes away
/// when closed or when a stream starts.
@MainActor
final class OldPCPrompt: ObservableObject {
    static let shared = OldPCPrompt()
    static let interval: TimeInterval = 3 * 86_400
    static let defaultsKey = "oldPCPromptShownAt"
    /// PCs whose last showing is kept.
    static let kept = 16

    struct Card: Equatable {
        /// The name the PC gives itself. It also keys the three days.
        let pc: String
        /// The release version it announced, if any.
        let version: String?
    }

    /// The card on screen.
    @Published private(set) var card: Card?
    /// An old PC waiting for the Connect screen.
    private var waiting: Card?
    private let defaults: UserDefaults
    private let now: () -> Date
    private let log: (String) -> Void

    init(defaults: UserDefaults = .standard, now: @escaping () -> Date = { Date() },
         log: @escaping (String) -> Void = { SupportDiagnostics.shared.record("info", "update", $0) }) {
        self.defaults = defaults
        self.now = now
        self.log = log
    }

    /// A PC this iPad found or connected to. An old one waits for `show`; a
    /// current one takes its card away.
    func saw(pc: String, announced: String?, hostCaps: UInt16?) {
        switch OldPC.isOld(announced: announced, hostCaps: hostCaps) {
        case true?:
            guard card == nil, due(pc) else { return }
            waiting = Card(pc: pc, version: announced.flatMap(AppVersion.init) == nil ? nil : announced)
        case false?:
            if waiting?.pc == pc { waiting = nil }
            if card?.pc == pc { card = nil }
        case nil:
            break
        }
    }

    /// The Connect screen is in view with nothing streaming: shows the
    /// waiting card, if its PC is due.
    func show() {
        guard card == nil, let next = waiting else { return }
        waiting = nil
        guard due(next.pc) else { return }
        var shown = defaults.dictionary(forKey: Self.defaultsKey) as? [String: Date] ?? [:]
        shown[next.pc] = now()
        for (pc, _) in shown.sorted(by: { $0.value > $1.value }).dropFirst(Self.kept) { shown[pc] = nil }
        defaults.set(shown, forKey: Self.defaultsKey)
        card = next
        log("old_pc_prompt shown=true version=\(next.version ?? "unknown")")
    }

    /// Closed, or the Connect screen gave way to a stream.
    func close() {
        card = nil
    }

    /// Whether `pc`'s card may show again. A showing dated after now, from a
    /// clock that was ahead, doesn't hold it back.
    func due(_ pc: String) -> Bool {
        guard let last = (defaults.dictionary(forKey: Self.defaultsKey) as? [String: Date])?[pc] else { return true }
        let now = now()
        return last > now || now.timeIntervalSince(last) >= Self.interval
    }

    #if DEBUG
    func preview(_ card: Card) {
        self.card = card
    }
    #endif
}
