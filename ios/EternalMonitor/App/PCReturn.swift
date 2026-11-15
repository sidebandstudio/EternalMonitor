import Combine
import Foundation
import Network

/// Bonjour's list of PCs, kept up to date until stopped. Tests pass a fake.
@MainActor
protocol PCBrowser: AnyObject {
    /// Called with the whole list each time it changes.
    var onHosts: (([DiscoveredHost]) -> Void)? { get set }
    /// Called when Bonjour can't search, as with Local Network off.
    var onSearchFailed: (() -> Void)? { get set }
    func start()
    func stop()
}

/// A Bonjour search that runs until stopped.
@MainActor
final class BonjourPCBrowser: PCBrowser {
    var onHosts: (([DiscoveredHost]) -> Void)?
    var onSearchFailed: (() -> Void)?
    private let scanner = NetworkScanner()
    private var watch: AnyCancellable?
    private var failure: AnyCancellable?

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
        failure = scanner.$searchProblem.compactMap { $0 }.sink { [weak self] _ in self?.onSearchFailed?() }
        scanner.startScan()
    }

    func stop() {
        watch = nil
        failure = nil
        scanner.stopScan()
    }
}

/// The PC that didn't answer, watched for on Bonjour from the start of the
/// attempt. Its HELLO2s go out for two seconds only, so a PC app that opens
/// after that never hears this iPad. While the card is up, each listing of
/// the PC is asked to prove it holds the saved pairing: at once when it
/// turns up or changes, then every few seconds, because a PC that closed
/// without saying goodbye stays listed, unchanged, after it opens again.
/// Every such listing is asked in turn, so one that never answers, such as
/// another PC at the saved address, can't keep the PC from being asked
/// where it is now. A listing that proves it gets one try; only a new or
/// changed one gets another.
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
    /// For the support log (`NoReplyFacts`): every `address:port` the PC
    /// was listed at since the watch began, whether Bonjour gave a list at
    /// all, and whether a search failed.
    private(set) var seenAt = Set<String>()
    private var heardAny = false
    private var searchFailed = false

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
        heardAny = true
        for pc in current.values { seenAt.insert("\(pc.address ?? ""):\(pc.port)") }
        let new = Set(current.keys).subtracting(listed.keys)
        listed = current
        tried.formIntersection(current.keys)
        return !new.isEmpty
    }

    /// The listings to ask, each in its turn: those that haven't had their
    /// try. One that doesn't answer stays among them.
    var candidates: [DiscoveredHost] {
        listed.keys.sorted().filter { !tried.contains($0) }.compactMap { listed[$0] }
    }

    /// Whether `pc` is still listed and hasn't had its try.
    func awaits(_ pc: DiscoveredHost) -> Bool {
        let key = Self.key(pc)
        return listed[key] != nil && !tried.contains(key)
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

    /// Bonjour couldn't search: a PC never listed may still be there.
    mutating func couldNotSearch() {
        searchFailed = true
    }

    /// Whether Bonjour listed the PC since the watch began: `yes`, `no`, or
    /// `unknown` when it gave no list or a search failed.
    var listedFact: String {
        if !seenAt.isEmpty { return "yes" }
        return heardAny && !searchFailed ? "no" : "unknown"
    }
}

/// The support log's line when an attempt over Wi-Fi ends with "didn't
/// answer" (`when=timeout`), and when the watch for that PC ends
/// (`when=watch_end`). It tells apart a PC that is closed, asleep or off
/// (not listed), one that runs but is blocked (listed at the address
/// tried), one that moved (listed elsewhere), and an iPad on another
/// network than that address (not on its subnet):
/// `no_reply_facts when=timeout listed=yes|no|unknown listed_at_tried=yes|no|-
/// same_subnet=yes|no|unknown path=wifi|wired|cellular|other|unknown`.
enum NoReplyFacts {
    /// `watch` is the watch for the PC, nil if none ran; `route` is how
    /// this iPad reaches `host:port` now, nil if no path was ready.
    static func line(when: String, watch: PCReturn?, host: String, port: UInt16, route: MovedPC.Route?) -> String {
        let listed = watch?.listedFact ?? "unknown"
        let atTried = listed != "yes" ? "-" : watch?.seenAt.contains("\(host):\(port)") == true ? "yes" : "no"
        let subnet = route.map { IPv4Address(host) == nil ? "unknown" : MovedPC.isLocal(host, over: $0) ? "yes" : "no" }
        return "no_reply_facts when=\(when) listed=\(listed) listed_at_tried=\(atTried) same_subnet=\(subnet ?? "unknown")"
            + " path=\(route?.kind ?? "unknown")"
    }
}
