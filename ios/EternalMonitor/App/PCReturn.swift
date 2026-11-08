import Combine
import Foundation

/// Bonjour's list of PCs, kept up to date until stopped. Tests pass a fake.
@MainActor
protocol PCBrowser: AnyObject {
    /// Called with the whole list each time it changes.
    var onHosts: (([DiscoveredHost]) -> Void)? { get set }
    func start()
    func stop()
}

/// A Bonjour search that runs until stopped.
@MainActor
final class BonjourPCBrowser: PCBrowser {
    var onHosts: (([DiscoveredHost]) -> Void)?
    private let scanner = NetworkScanner()
    private var watch: AnyCancellable?

    init() {
        scanner.retriesWhenEmpty = false
        scanner.searchesUntilStopped = true
    }

    func start() {
        // One update replaces a PC's entry in steps; only the list it ends on counts.
        watch = scanner.$hosts.debounce(for: .milliseconds(100), scheduler: RunLoop.main).sink { [weak self] _ in
            guard let self else { return }
            self.onHosts?(self.scanner.hosts)
        }
        scanner.startScan()
    }

    func stop() {
        watch = nil
        scanner.stopScan()
    }
}

/// The PC that didn't answer, watched for on Bonjour. Its HELLO2s go out
/// for two seconds only, so a PC app that opens after that never hears this
/// iPad: the PC's service turning up, or changing, is the sign to try once
/// more. What Bonjour already listed when the search began is no sign; the
/// PC was there and didn't answer.
struct PCReturn {
    /// The first second of a search lists what Bonjour already knew.
    static let settleSeconds = 1.0

    /// The PC's Bonjour service, when this iPad knows the PC's name. The
    /// service at the address tried is the PC too.
    let service: String?
    let host: String
    let port: UInt16
    /// Until when the latest search lists what Bonjour already knew.
    private var settlesAt: Date
    /// A search ran before this one: what it listed is what was known.
    private var resumed = false
    /// The PC's listings, by name, address, port and version.
    private var listed = Set<String>()
    /// The latest list while settling, which replaces `listed` after it.
    private var settled: Set<String>?
    /// A listing turned up that has had no try yet.
    private(set) var appeared = false
    /// When the card went up and the watch's time limit started.
    private(set) var armedAt: Date?

    init(service: String?, host: String, port: UInt16, at now: Date) {
        self.service = service
        self.host = host
        self.port = port
        settlesAt = now.addingTimeInterval(Self.settleSeconds)
    }

    func matches(_ pc: DiscoveredHost) -> Bool {
        guard let address = pc.address else { return false }
        return (service != nil && pc.name == service) || (address == host && pc.port == port)
    }

    /// Bonjour's list now. A listing of the PC that wasn't there before is
    /// an appearance; a new address, port or version is one too.
    mutating func heard(_ hosts: [DiscoveredHost], at now: Date) {
        let current = Set(hosts.filter(matches).map { "\($0.name)|\($0.address ?? "")|\($0.port)|\($0.version ?? "")" })
        if now < settlesAt {
            // A search that starts again lists only what changed meanwhile as new.
            if resumed, !current.subtracting(listed).isEmpty { appeared = true }
            listed.formUnion(current)
            settled = current
            return
        }
        if let settled {
            listed = settled
            self.settled = nil
        }
        if !current.subtracting(listed).isEmpty { appeared = true }
        // A listing that left and comes back is a new appearance.
        listed = current
    }

    /// The search starts again, as when the app comes back to the foreground.
    mutating func restarted(at now: Date) {
        settlesAt = now.addingTimeInterval(Self.settleSeconds)
        resumed = true
    }

    /// The card is up: the time limit starts, once.
    mutating func arm(at now: Date) {
        if armedAt == nil { armedAt = now }
    }

    func expired(at now: Date, limit: TimeInterval) -> Bool {
        armedAt.map { now.timeIntervalSince($0) >= limit } ?? false
    }

    /// Takes the appearance for its one try.
    mutating func takeAppearance() -> Bool {
        defer { appeared = false }
        return appeared
    }
}
