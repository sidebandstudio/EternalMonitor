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

/// The PC that didn't answer, watched for on Bonjour from the start of the
/// attempt. Its HELLO2s go out for two seconds only, so a PC app that opens
/// after that never hears this iPad. While the card is up, each listing of
/// the PC is asked to prove it holds the saved pairing: at once when it
/// turns up or changes, then every few seconds, because a PC that closed
/// without saying goodbye stays listed, unchanged, after it opens again.
/// A listing that proves it gets one try; only a new or changed one gets
/// another.
struct PCReturn {
    /// The PC's name, when this iPad knows it: its Bonjour service is the
    /// PC, wherever it is. The service at the address tried is the PC too.
    let name: String?
    let host: String
    let port: UInt16
    /// The PC's listings now, by name, address, port and version.
    private(set) var listed: [String: DiscoveredHost] = [:]
    /// Listings that had their try, while they stay listed.
    private var tried = Set<String>()
    /// When the card went up and the watch's time limit started.
    private(set) var armedAt: Date?

    init(name: String?, host: String, port: UInt16) {
        self.name = name.flatMap { $0.isEmpty ? nil : $0 }
        self.host = host
        self.port = port
    }

    var service: String? { name.map(MovedPC.serviceName) }

    func matches(_ pc: DiscoveredHost) -> Bool {
        guard let address = pc.address else { return false }
        return (service != nil && pc.name == service) || (address == host && pc.port == port)
    }

    private static func key(_ pc: DiscoveredHost) -> String {
        "\(pc.name)|\(pc.address ?? "")|\(pc.port)|\(pc.version ?? "")"
    }

    /// Bonjour's list now. True when a listing of the PC turned up, or
    /// changed address, port or version: it is asked at once. A listing
    /// that leaves and comes back may have a try again.
    mutating func heard(_ hosts: [DiscoveredHost]) -> Bool {
        var current: [String: DiscoveredHost] = [:]
        for pc in hosts where matches(pc) { current[Self.key(pc)] = pc }
        let new = Set(current.keys).subtracting(listed.keys)
        listed = current
        tried.formIntersection(current.keys)
        return !new.isEmpty
    }

    /// The listing to ask next: one that hasn't had its try.
    var candidate: DiscoveredHost? {
        listed.keys.sorted().first { !tried.contains($0) }.flatMap { listed[$0] }
    }

    /// `pc` had its try, or can't have one.
    mutating func didTry(_ pc: DiscoveredHost) {
        tried.insert(Self.key(pc))
    }

    /// The card is up: the time limit starts, once.
    mutating func arm(at now: Date) {
        if armedAt == nil { armedAt = now }
    }

    func expired(at now: Date, limit: TimeInterval) -> Bool {
        armedAt.map { now.timeIntervalSince($0) >= limit } ?? false
    }
}
