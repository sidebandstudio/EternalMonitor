import Combine
import XCTest
@testable import EternalMonitor

/// Bonjour's list as a test hands it over.
@MainActor
private final class FakePCBrowser: PCBrowser {
    var onHosts: (([DiscoveredHost]) -> Void)?
    private(set) var starts = 0
    private(set) var stops = 0
    var running: Bool { starts > stops }
    func start() { starts += 1 }
    func stop() { stops += 1 }
    func list(_ hosts: [DiscoveredHost]) { onHosts?(hosts) }
}

/// The iPad shows "didn't answer" because the PC app wasn't running yet.
/// When the PC's service then shows up on the network, it connects once by
/// itself, while the card is up, in the foreground, for a few minutes.
@MainActor
final class PCReturnTests: XCTestCase {
    private let start = Date(timeIntervalSince1970: 1_000_000)

    private func pc(_ name: String = "EternalMonitor (Test PC)", _ address: String? = "127.0.0.1", port: UInt16,
                    version: String? = "0.3.5") -> DiscoveredHost {
        DiscoveredHost(name: name, address: address, port: port, version: version)
    }

    // MARK: - What counts as the PC showing up

    func testOnlyAListingThatWasNotThereBeforeCounts() {
        var watch = PCReturn(service: "EternalMonitor (Test PC)", host: "192.0.2.20", port: 9876, at: start)
        let listed = pc(port: 9876)
        // What Bonjour already knew when the search began.
        watch.heard([listed], at: start.addingTimeInterval(0.2))
        watch.heard([listed], at: start.addingTimeInterval(PCReturn.settleSeconds + 1))
        XCTAssertFalse(watch.appeared, "the PC was listed and didn't answer")
        // It leaves (the app closed) and comes back.
        watch.heard([], at: start.addingTimeInterval(5))
        XCTAssertFalse(watch.appeared)
        watch.heard([listed], at: start.addingTimeInterval(9))
        XCTAssertTrue(watch.takeAppearance())
        XCTAssertFalse(watch.takeAppearance(), "one try per appearance")
        // The same listing again is no new appearance; a change is.
        watch.heard([listed], at: start.addingTimeInterval(10))
        XCTAssertFalse(watch.appeared)
        watch.heard([pc(port: 9877)], at: start.addingTimeInterval(11))
        XCTAssertTrue(watch.takeAppearance(), "a new port")
        watch.heard([pc(port: 9877, version: "0.3.6")], at: start.addingTimeInterval(12))
        XCTAssertTrue(watch.takeAppearance(), "a new version")
    }

    func testAPCMissingAtTheStartCountsAsSoonAsItShowsUp() {
        var watch = PCReturn(service: nil, host: "192.0.2.20", port: 9876, at: start)
        watch.heard([], at: start.addingTimeInterval(0.1))
        watch.heard([pc("EternalMonitor (Test PC)", "192.0.2.20", port: 9876)], at: start.addingTimeInterval(PCReturn.settleSeconds + 0.5))
        XCTAssertTrue(watch.appeared)
    }

    func testOnlyThisPCCounts() {
        var named = PCReturn(service: "EternalMonitor (Test PC)", host: "192.0.2.20", port: 9876, at: start)
        var unnamed = PCReturn(service: nil, host: "192.0.2.20", port: 9876, at: start)
        let later = start.addingTimeInterval(5)
        let others = [pc("EternalMonitor (Other PC)", "192.0.2.30", port: 9876),
                      pc("EternalMonitor (Test PC 2)", "192.0.2.31", port: 9876),
                      pc("EternalMonitor (Other PC)", "192.0.2.20", port: 9900),
                      pc("EternalMonitor (Test PC)", nil, port: 9876)]
        named.heard(others, at: later)
        unnamed.heard(others, at: later)
        XCTAssertFalse(named.appeared)
        XCTAssertFalse(unnamed.appeared)
        // By name at another address, or by the address tried.
        named.heard(others + [pc("EternalMonitor (Test PC)", "192.0.2.40", port: 9876)], at: later)
        XCTAssertTrue(named.appeared)
        unnamed.heard(others + [pc("EternalMonitor (Any)", "192.0.2.20", port: 9876)], at: later)
        XCTAssertTrue(unnamed.appeared)
    }

    /// Back from the background, the new search lists what changed while
    /// the app was away as new, and what was there before as known.
    func testASearchStartedAgainKeepsWhatItKnew() {
        var watch = PCReturn(service: "EternalMonitor (Test PC)", host: "192.0.2.20", port: 9876, at: start)
        watch.heard([pc(port: 9876)], at: start.addingTimeInterval(0.1))
        watch.restarted(at: start.addingTimeInterval(30))
        watch.heard([], at: start.addingTimeInterval(30.1))
        watch.heard([pc(port: 9876)], at: start.addingTimeInterval(30.2))
        XCTAssertFalse(watch.appeared, "still the listing that didn't answer")

        var away = PCReturn(service: "EternalMonitor (Test PC)", host: "192.0.2.20", port: 9876, at: start)
        away.heard([], at: start.addingTimeInterval(0.1))
        away.restarted(at: start.addingTimeInterval(30))
        away.heard([pc(port: 9876)], at: start.addingTimeInterval(30.2))
        XCTAssertTrue(away.appeared, "it showed up while the app was away")
    }

    func testTheTimeLimitStartsWhenTheCardGoesUp() {
        var watch = PCReturn(service: nil, host: "192.0.2.20", port: 9876, at: start)
        XCTAssertFalse(watch.expired(at: start.addingTimeInterval(1_000), limit: 180), "not armed yet")
        watch.arm(at: start.addingTimeInterval(10))
        watch.arm(at: start.addingTimeInterval(100))
        XCTAssertFalse(watch.expired(at: start.addingTimeInterval(189), limit: 180))
        XCTAssertTrue(watch.expired(at: start.addingTimeInterval(190), limit: 180), "a second card keeps the first limit")
    }

    // MARK: - Connecting

    private struct Setup {
        let manager: ConnectionManager
        let browser: FakePCBrowser
        let host: FakeUDPHost
        let lines: () -> [String]
        let log: AnyCancellable
        /// Moves the manager's clock.
        let advance: (TimeInterval) -> Void

        var tries: Int { lines().filter { $0 == "pc_return result=tried" }.count }
        var attempts: Int { lines().filter { $0.hasPrefix("Starting connection attempt=") }.count }
        var listing: DiscoveredHost { DiscoveredHost(name: "EternalMonitor (Test PC)", address: "127.0.0.1", port: host.port) }
    }

    override func setUp() async throws {
        let store = RecentConnectionStore.shared
        let saved = store.connections
        let defaults = ["recentConnections", "lastHost", "lastPort", "allowUSB", "autoReconnect"]
            .map { ($0, UserDefaults.standard.object(forKey: $0)) }
        // No cable listener in these tests.
        UserDefaults.standard.set(false, forKey: "allowUSB")
        addTeardownBlock { @MainActor in
            store.connections = saved
            for (key, value) in defaults { UserDefaults.standard.set(value, forKey: key) }
        }
    }

    /// A PC that doesn't answer at loopback, saved under its name so the
    /// watch knows its service. The "didn't answer" card is up when this
    /// returns, and the watch armed.
    private func noReply(named: Bool = true) async throws -> Setup {
        let host = FakeUDPHost()
        if named {
            RecentConnectionStore.shared.connections = [
                RecentConnection(host: "127.0.0.1", port: host.port, lastUsed: .now, isUSB: false, name: "Test PC", found: false),
            ]
        }
        let manager = ConnectionManager(timing: .init(connectTimeoutSeconds: 4, reconnectGraceSeconds: 0.1,
                                                      reconnectBackoffSeconds: { _ in 0.05 }, pcReturnDebounceSeconds: 0.1),
                                        pairingStore: PairingStore(storage: MemoryPairingStorage()))
        var clock = start
        manager.now = { clock }
        let browser = FakePCBrowser()
        manager.makePCBrowser = { browser }
        var lines: [String] = []
        let log = manager.$diagnostics.sink { if let line = $0.last?.message { lines.append(line) } }
        manager.connect(host: "127.0.0.1", port: host.port)
        try await until("didn't answer", seconds: 15) { manager.problem == .noReply(usb: false) }
        XCTAssertTrue(manager.watchingForPC)
        XCTAssertTrue(browser.running, "the search runs while the card is up")
        // Bonjour has nothing for the PC: its app isn't open.
        browser.list([])
        clock = start.addingTimeInterval(PCReturn.settleSeconds + 1)
        // Drop the unanswered HELLO2s, so only a new attempt's count.
        _ = await Task.detached { host.helloAttempts(within: 0.4) }.value
        return Setup(manager: manager, browser: browser, host: host, lines: { lines }, log: log,
                     advance: { clock = clock.addingTimeInterval($0) })
    }

    private func until(_ description: String, seconds: Double, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    private func settle(_ seconds: Double = 1) async throws {
        try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }

    /// No answer; the PC app opens and its service shows up; the iPad
    /// connects once, by itself, and streams.
    func testThePCShowingUpConnectsOnceByItself() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        XCTAssertEqual(setup.attempts, 1)
        setup.browser.list([setup.listing])
        // Updates while it waits count as the same appearance.
        setup.browser.list([setup.listing])
        let accepted = await Task.detached { [host = setup.host] in host.acceptSession() }.value
        XCTAssertTrue(accepted, "no automatic attempt reached the PC")
        try await until("handshake", seconds: 15) { setup.manager.hostInfo != nil }
        setup.host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming", seconds: 30) { setup.manager.state == .connected }
        XCTAssertEqual(setup.tries, 1)
        XCTAssertEqual(setup.attempts, 2)
        XCTAssertFalse(setup.manager.watchingForPC)
        XCTAssertFalse(setup.browser.running, "streaming ends the watch")
    }

    /// The automatic try gets no answer either: the card comes back and
    /// waits for the PC to show up again. The same listing never brings
    /// another try.
    func testOneTryPerAppearance() async throws {
        let setup = try await noReply(named: false)
        defer { setup.manager.cancel() }
        let listing = DiscoveredHost(name: "EternalMonitor (Unknown)", address: "127.0.0.1", port: setup.host.port)
        setup.browser.list([listing])
        try await until("tried", seconds: 5) { setup.tries == 1 }
        try await until("didn't answer again", seconds: 15) {
            setup.manager.state == .disconnected && setup.manager.problem == .noReply(usb: false)
        }
        XCTAssertTrue(setup.manager.watchingForPC, "still waiting for the PC")
        setup.browser.list([listing])
        try await settle()
        XCTAssertEqual(setup.tries, 1)
        XCTAssertEqual(setup.attempts, 2)
        // It leaves and comes back: one more try.
        setup.browser.list([])
        setup.browser.list([listing])
        try await until("tried again", seconds: 5) { setup.tries == 2 }
    }

    func testNoTryAfterTheCardIsDismissed() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        setup.manager.problem = nil
        XCTAssertFalse(setup.manager.watchingForPC)
        XCTAssertFalse(setup.browser.running)
        setup.browser.list([setup.listing])
        try await settle()
        XCTAssertEqual(setup.attempts, 1)
    }

    func testNoTryAfterTheTimeLimit() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        setup.advance(ConnectionManager.Timing().pcReturnLimitSeconds)
        setup.browser.list([setup.listing])
        try await settle()
        XCTAssertEqual(setup.attempts, 1)
        XCTAssertFalse(setup.manager.watchingForPC)
        XCTAssertFalse(setup.browser.running)
        XCTAssertTrue(setup.lines().contains("pc_return result=expired"))
    }

    /// Nothing starts in the background. Back in the foreground, a PC that
    /// showed up meanwhile gets its try.
    func testNoTryInTheBackground() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        setup.manager.handleAppBackgrounded()
        XCTAssertFalse(setup.browser.running, "no search in the background")
        setup.browser.list([setup.listing])
        try await settle()
        XCTAssertEqual(setup.attempts, 1)
        setup.manager.handleAppForegrounded()
        XCTAssertTrue(setup.browser.running)
        try await until("tried on return", seconds: 5) { setup.tries == 1 }
    }

    func testPickingAnotherPCEndsTheWatch() async throws {
        let setup = try await noReply()
        let other = FakeUDPHost()
        setup.manager.connect(host: "127.0.0.1", port: other.port)
        defer { setup.manager.cancel() }
        XCTAssertFalse(setup.browser.running)
        setup.browser.list([setup.listing])
        try await settle()
        XCTAssertEqual(setup.tries, 0)
    }

    /// A PC that turns the automatic try down (busy here) ends the watch:
    /// it never loops.
    func testARefusalEndsTheWatch() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        setup.browser.list([setup.listing])
        let refused = await Task.detached { [host = setup.host] in host.acceptSession(status: .busy) }.value
        XCTAssertTrue(refused)
        try await until("busy", seconds: 15) { setup.manager.problem == .pcBusy }
        XCTAssertFalse(setup.browser.running)
        setup.browser.list([])
        setup.browser.list([setup.listing])
        try await settle()
        XCTAssertEqual(setup.tries, 1)
    }
}

/// Pairings kept in memory.
private final class MemoryPairingStorage: PairingStorage {
    var values: [String: Data] = [:]
    func records() throws -> [PairingRecord] { values.map { PairingRecord(account: $0.key, token: $0.value) } }
    func save(account: String, token: Data) throws { values[account] = token }
    func remove(account: String?) throws {
        if let account { values.removeValue(forKey: account) } else { values.removeAll() }
    }
}
